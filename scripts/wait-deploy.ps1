<#
.SYNOPSIS
Ждёт, пока боевая панель начнёт отдавать файлы из origin/master.

.DESCRIPTION
Пуш в master сам по себе ничего не разворачивает: хостинг собирает образ по
пушу и перезапускает контейнер, на это уходит до 20 минут (см. CLAUDE.md).
До конца сборки сервер отвечает `ok` и отдаёт прошлую версию — именно поэтому
«выпустили» и «доехало» это два разных события.

Скрипт сравнивает SHA-256 файлов интерфейса с их байтами в origin/master.
Список файлов берётся из `crates/server/src/assets.rs` того же коммита, поэтому
новый файл панели не нужно дописывать сюда руками.

Скрипт только читает: ничего не коммитит, не пушит и не пересобирает.

.PARAMETER Url
Адрес панели. По умолчанию боевой; локальную проверку можно делать ключом
`-Url http://127.0.0.1:18080`.

.PARAMETER TimeoutSec
Сколько ждать сборку. По умолчанию 1200 секунд (20 минут), как в CLAUDE.md.

.PARAMETER EverySec
Пауза между проверками. По умолчанию 45 секунд.

.PARAMETER File
Проверить только эти пути (`/app.js`, `/style.css`). По умолчанию — все файлы
интерфейса.

.PARAMETER NoFetch
Не делать `git fetch`: сверять с тем origin/master, который уже известен.

.PARAMETER DryRun
Показать, что и с чем будет сравниваться, и выйти.

.EXAMPLE
pwsh scripts/wait-deploy.ps1

.EXAMPLE
pwsh scripts/wait-deploy.ps1 -DryRun

.EXAMPLE
pwsh scripts/wait-deploy.ps1 -File /,/app.voice.js -TimeoutSec 300
#>
[CmdletBinding()]
param(
    [string]$Url = 'https://panel.example.com',
    [int]$TimeoutSec = 1200,
    [int]$EverySec = 45,
    [string[]]$File = @(),
    [switch]$NoFetch,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$rev = 'origin/master'

function Git-Bytes {
    param([string]$spec)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = 'git'
    $psi.Arguments = "cat-file blob `"$spec`""
    $psi.WorkingDirectory = $repo
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $p = [System.Diagnostics.Process]::Start($psi)
    $ms = New-Object System.IO.MemoryStream
    $p.StandardOutput.BaseStream.CopyTo($ms)
    $err = $p.StandardError.ReadToEnd()
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) {
        throw "git cat-file $spec`: $err"
    }
    # Запятая в начале: иначе PowerShell развернёт массив и вернёт пустоту на пустом файле.
    , $ms.ToArray()
}

function Hash-Bytes {
    param([byte[]]$bytes)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        (($sha.ComputeHash($bytes) | ForEach-Object { $_.ToString('x2') }) -join '')
    } finally {
        $sha.Dispose()
    }
}

function File-Name {
    param([string]$path)
    if ($path -eq '/') { 'index.html' } else { $path.TrimStart('/') }
}

if (-not $NoFetch) {
    Write-Host "git fetch origin…"
    & git -C $repo fetch origin --quiet
    if ($LASTEXITCODE -ne 0) { throw "git fetch origin завершился с кодом $LASTEXITCODE" }
}

$head = (& git -C $repo log -1 --format='%h %s' $rev).Trim()
Write-Host "сверяем с $rev ($head)"

if ($File.Count -gt 0) {
    # Запятая в `-File /,/app.voice.js` доходит одной строкой: pwsh -File передаёт
    # аргументы как есть, не разбирая их на массив.
    $paths = @($File | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
} else {
    # Файлы интерфейса перечислены в assets.rs (`include_str!("../static/…")`):
    # читаем список оттуда, чтобы он не разъезжался с сервером.
    $assets = [System.Text.Encoding]::UTF8.GetString((Git-Bytes "${rev}:crates/server/src/assets.rs"))
    $names = [regex]::Matches($assets, 'include_(?:str|bytes)!\("\.\./static/([^"]+)"\)') |
        ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
    if (-not $names) { throw "в assets.rs не нашлось файлов интерфейса" }
    $paths = @($names | ForEach-Object { if ($_ -eq 'index.html') { '/' } else { "/$_" } })
}

$expected = @{}
foreach ($path in $paths) {
    $bytes = Git-Bytes "${rev}:crates/server/static/$(File-Name $path)"
    $expected[$path] = Hash-Bytes $bytes
}

Write-Host ("файлов: {0}" -f $paths.Count)
if ($DryRun) {
    foreach ($path in $paths) {
        Write-Host ("  {0,-26} {1}" -f $path, $expected[$path].Substring(0, 12))
    }
    return
}

$live = Join-Path ([System.IO.Path]::GetTempPath()) ("aipanel-live-" + $PID)
New-Item -ItemType Directory -Force -Path $live | Out-Null
$tmp = Join-Path $live 'file'
$deadline = (Get-Date).AddSeconds($TimeoutSec)
$last = ''

while ($true) {
    $stale = @()
    $missing = 0
    foreach ($path in $paths) {
        $target = $Url.TrimEnd('/') + $path
        try {
            Invoke-WebRequest -Uri $target -OutFile $tmp -UseBasicParsing -TimeoutSec 25
            if ((Get-FileHash $tmp -Algorithm SHA256).Hash -ne $expected[$path]) { $stale += $path }
        } catch {
            $missing += 1
            $stale += $path
        }
    }

    $now = (Get-Date).ToString('HH:mm:ss')
    if ($stale.Count -eq 0) {
        Write-Host "готово ${now}: боевая панель отдаёт файлы $rev ($head)"
        Remove-Item -Recurse -Force $live
        exit 0
    }

    # Одинаковые строки не печатаем: иначе ожидание забивает вывод.
    $line = "ещё старо ${now}: обновилось $($paths.Count - $stale.Count) из $($paths.Count)"
    if ($missing -eq $paths.Count) { $line = "ещё старо ${now}: все файлы 404 — образ ещё не пересобран" }
    if ($line -ne $last) {
        Write-Host $line
        $last = $line
    }

    if ((Get-Date) -ge $deadline) {
        Write-Host ""
        Write-Host ("за {0} минут не собралось. Не обновились:" -f [int]($TimeoutSec / 60))
        foreach ($path in $stale) { Write-Host "  $path" }
        Write-Host ""
        Write-Host "Скорее всего, упала сборка, а не «ещё собирается». Смотреть статус и логи:"
        Write-Host "  <хостинг> repository list --project <проект>"
        Write-Host "  <хостинг> container logs <контейнер> --tail 200"
        Remove-Item -Recurse -Force $live
        exit 1
    }

    Start-Sleep -Seconds $EverySec
}
