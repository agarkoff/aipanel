<#
.SYNOPSIS
    Выпуск готовой фичи: тесты, пересборка агента и панели, коммит и пуш.

.DESCRIPTION
    Порядок шагов:
      1. проверить рабочую копию и проиндексировать её целиком (git add -A);
      2. прогнать cargo test --workspace (+ node --check для правленых .js);
      3. если менялся агент или протокол — собрать release-exe и заменить обе копии
         (~/.local/bin и ~/.aipanel/bin) с остановкой и перезапуском;
      4. если менялся сервер или статика — пересобрать образ панели в Docker
         и сверить отдаваемые файлы с репозиторием;
      5. закоммитить и запушить в текущую ветку;
      6. если статика менялась — дождаться, пока боевая панель отдаст новые файлы.

    Скрипт коммитит всё, что лежит в рабочей копии. Перед запуском убедитесь,
    что там нет чужих незакоммиченных правок.

.PARAMETER Message
    Первая строка коммита в стиле проекта: "Панель: ...", "Агент: ...", "Сервер: ...".

.PARAMETER Body
    Необязательное описание: что и почему, с пустой строкой в виде `n`n между абзацами.

.EXAMPLE
    pwsh -File scripts/release.ps1 -Message "Панель: удаление сессий свайпом" -Body "Крестик уводит сессию..."

.EXAMPLE
    pwsh -File scripts/release.ps1 -Message "Агент: отметка доверия каталогу" -DryRun
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)][string]$Message,
    [string]$Body = '',
    [switch]$SkipAgent,
    [switch]$SkipPanel,
    [switch]$SkipDeploy,
    [switch]$NoPush,
    [int]$DeployTimeoutSec = 300,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = (& git rev-parse --show-toplevel 2>$null)
if (-not $root) { throw 'запускать из репозитория проекта' }
$root = $root.Trim()
Set-Location $root

$localHome = Join-Path $env:USERPROFILE '.aipanel'
$cloudHome = Join-Path $env:USERPROFILE '.aipanel'
$localExeDir = Join-Path $env:USERPROFILE '.local\bin'
$cloudExeDir = Join-Path $cloudHome 'bin'
$builtExe = Join-Path $root 'target\release\aipanel-agent.exe'

function Write-Step([string]$title) {
    Write-Host ''
    Write-Host "== $title" -ForegroundColor Cyan
}

function Assert-LastExit([string]$what) {
    if ($LASTEXITCODE -ne 0) { throw "${what}: код выхода $LASTEXITCODE" }
}

function Invoke-Native([string]$what, [scriptblock]$action) {
    & $action
    Assert-LastExit $what
}

# Порт локальной панели: из PANEL_LISTEN в .env, иначе стандартный 8080.
$port = 8080
$envFile = Join-Path $root '.env'
if (Test-Path $envFile) {
    $listen = (Get-Content $envFile | Where-Object { $_ -match '^PANEL_LISTEN=' } | Select-Object -First 1)
    if ($listen) { $port = ($listen -replace '^PANEL_LISTEN=', '').Trim().Trim('"').Split(':')[-1] }
}
$panelUrl = "http://127.0.0.1:$port"
$prodUrl = 'https://panel.example.com'
$statics = @('app.js', 'style.css', 'index.html', 'md.js', 'suggest.js')

function Get-WebHash([string]$url) {
    $tmp = [System.IO.Path]::GetTempFileName()
    try {
        Invoke-WebRequest $url -OutFile $tmp -UseBasicParsing -TimeoutSec 20 | Out-Null
        (Get-FileHash $tmp -Algorithm SHA256).Hash
    } catch {
        # Во время пересборки образа прокси может отдавать 502 — вызывающий решает, повторять или нет.
        ''
    } finally {
        Remove-Item $tmp -ErrorAction SilentlyContinue
    }
}

# index.html отдаётся по корню, остальные файлы — по своему имени.
function Get-StaticUrl([string]$base, [string]$file) {
    if ($file -eq 'index.html') { "$base/" } else { "$base/$file" }
}

# Заменяет exe запущенного агента: сначала stop, потом переименование (работающий
# файл заблокирован), потом копия и запуск с проверкой соединения.
function Update-Agent([string]$label, [string]$exeDir, [string]$home) {
    $exe = Join-Path $exeDir 'aipanel-agent.exe'
    $config = Join-Path $home 'agent.toml'
    Write-Host "  $label"
    if ($DryRun) { Write-Host "    (пропущено: -DryRun) $exe" -ForegroundColor DarkGray; return }

    if (Test-Path $exe) {
        # Код stop не проверяем: «агент не запущен» — тоже нормальный случай.
        & $exe stop --config $config --home $home | Out-Null
        # Окна, открытые обёрткой run-window, держат этот же exe, поэтому переименование
        # может не пройти — тогда копируем поверх.
        try { Move-Item $exe "$exe.old" -Force -ErrorAction Stop } catch { }
    }
    Copy-Item $builtExe $exe -Force
    $got = (Get-FileHash $exe -Algorithm SHA256).Hash
    if ($got -ne (Get-FileHash $builtExe -Algorithm SHA256).Hash) {
        throw "не удалось заменить ${exe}: файл держит запущенная копия (закройте окна, открытые агентом)"
    }
    & $exe status --config $config --home $home
    if ($LASTEXITCODE -ne 0) { throw "агент «$label» не подключился (код $LASTEXITCODE)" }
}

# ---------------------------------------------------------------- 1. рабочая копия

Write-Step 'Рабочая копия'
& git add -A
Assert-LastExit 'git add'
$changed = @(& git diff --cached --name-only)
if (-not $changed) { throw 'нечего коммитить: в рабочей копии нет изменений' }
$changed | ForEach-Object { Write-Host "  $_" }

$branch = (& git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -ne 'master' -and -not $NoPush) {
    Write-Warning "ветка «$branch»: пуш в неё ничего не разворачивает, автодеплой идёт с master"
}

# ---------------------------------------------------------------- 2. проверки

Write-Step 'Проверки'
if ($DryRun) {
    Write-Host '  (пропущено: -DryRun) cargo test --workspace' -ForegroundColor DarkGray
} else {
    Invoke-Native 'cargo test' { cargo test --workspace }
}
$scripts = $changed | Where-Object { $_ -like 'crates/server/static/*.js' }
foreach ($file in $scripts) {
    if ($DryRun) {
        Write-Host "  (пропущено: -DryRun) node --check $file" -ForegroundColor DarkGray
    } else {
        Invoke-Native "node --check $file" { node --check $file }
    }
}
$uiTest = Join-Path $root 'tests\ui\feed.mjs'
if ((Test-Path $uiTest) -and ($changed | Where-Object { $_ -like 'crates/server/static/*' })) {
    if ($DryRun) {
        Write-Host '  (пропущено: -DryRun) node tests/ui/feed.mjs' -ForegroundColor DarkGray
    } else {
        Invoke-Native 'node tests/ui/feed.mjs' { node tests/ui/feed.mjs }
    }
}

# ---------------------------------------------------------------- 3. агент

$agentChanged = @($changed | Where-Object { $_ -match '^crates/(agent|proto)/' -or $_ -match '^Cargo\.(toml|lock)$' })
if ($agentChanged.Count -and -not $SkipAgent) {
    Write-Step 'Агент: сборка и замена копий'
    if ($DryRun) {
        Write-Host '  (пропущено: -DryRun) cargo build --release -p aipanel-agent' -ForegroundColor DarkGray
    } else {
        Invoke-Native 'cargo build --release -p aipanel-agent' { cargo build --release -p aipanel-agent }
    }
    Update-Agent 'локальный' $localExeDir $localHome
    Update-Agent 'боевой' $cloudExeDir $cloudHome
    if (-not $DryRun) { Write-Host '  обе копии заменены и подключены' }
} elseif ($agentChanged.Count) {
    Write-Step 'Агент: пропущено (-SkipAgent)'
} else {
    Write-Step 'Агент: код не менялся, пропуск'
}

# ---------------------------------------------------------------- 4. панель

$panelChanged = @($changed | Where-Object {
        $_ -match '^crates/server/' -or $_ -eq 'Dockerfile' -or $_ -eq 'docker-compose.yml' -or
        $_ -match '^Cargo\.(toml|lock)$'
    })
if ($panelChanged.Count -and -not $SkipPanel) {
    Write-Step 'Панель: пересборка образа в Docker'
    if ($DryRun) {
        Write-Host '  (пропущено: -DryRun) docker compose up -d --build' -ForegroundColor DarkGray
    } else {
        Invoke-Native 'docker compose up -d --build' { docker compose up -d --build }
        # Панель перезапускается: ждём healthz, затем сверяем отдаваемые файлы с репозиторием.
        $deadline = (Get-Date).AddSeconds(60)
        do {
            Start-Sleep -Seconds 3
            $ready = $false
            try { $ready = (Invoke-WebRequest "$panelUrl/healthz" -UseBasicParsing -TimeoutSec 5).Content -eq 'ok' } catch { }
        } while (-not $ready -and (Get-Date) -lt $deadline)
        if (-not $ready) { throw "панель не ответила на $panelUrl/healthz" }

        foreach ($file in $statics) {
            $local = Join-Path $root "crates\server\static\$file"
            if (-not (Test-Path $local)) { continue }
            $want = (Get-FileHash $local -Algorithm SHA256).Hash
            $got = Get-WebHash (Get-StaticUrl $panelUrl $file)
            if (-not $got) { throw "панель не отдаёт $file по адресу $(Get-StaticUrl $panelUrl $file)" }
            if ($want -ne $got) { throw "панель отдаёт старый $file — образ собрался не из рабочей копии?" }
        }
        Write-Host "  панель на $panelUrl отдаёт файлы из рабочей копии"
    }
} elseif ($panelChanged.Count) {
    Write-Step 'Панель: пропущено (-SkipPanel)'
} else {
    Write-Step 'Панель: сервер и статика не менялись, пропуск'
}

# ---------------------------------------------------------------- 5. коммит и пуш

Write-Step 'Коммит'
$msgFile = Join-Path ([System.IO.Path]::GetTempPath()) ("aipanel-commit-{0}.txt" -f $PID)
# Если в -Body текст передали вместе с первой строкой, дубль заголовка убираем.
$bodyText = $Body.Trim()
if ($bodyText -and ($bodyText -split "`r?`n")[0].Trim() -eq $Message) {
    $bodyText = (@($bodyText -split "`r?`n") | Select-Object -Skip 1) -join "`n"
    $bodyText = $bodyText.Trim()
}
$text = if ($bodyText) { "$Message`n`n$bodyText`n" } else { "$Message`n" }
[System.IO.File]::WriteAllText($msgFile, $text, (New-Object System.Text.UTF8Encoding($false)))
try {
    if ($DryRun) {
        Write-Host "  (пропущено: -DryRun) git commit -F <$Message>" -ForegroundColor DarkGray
    } else {
        & git -c core.safecrlf=false commit -F $msgFile
        Assert-LastExit 'git commit'
    }
} finally {
    Remove-Item $msgFile -ErrorAction SilentlyContinue
}

if ($NoPush) {
    Write-Step 'Пуш: пропущен (-NoPush)'
} elseif ($DryRun) {
    Write-Step "Пуш (пропущено: -DryRun): git push origin $branch"
} else {
    Write-Step "Пуш в origin/$branch"
    Invoke-Native 'git push' { git push origin $branch }
}

# ---------------------------------------------------------------- 6. боевая панель

$prodStatics = @($changed | Where-Object { $_ -match '^crates/server/static/' -or $_ -eq 'Dockerfile' } | ForEach-Object { Split-Path $_ -Leaf })
if ($prodStatics.Count -and -not $SkipDeploy -and -not $NoPush -and -not $DryRun -and $branch -eq 'master') {
    Write-Step "Боевая панель: ждём обновления (до $DeployTimeoutSec с)"
    $deadline = (Get-Date).AddSeconds($DeployTimeoutSec)
    $pending = @($prodStatics)
    while ($pending.Count -and (Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 10
        $pending = @($pending | Where-Object {
                $local = Join-Path $root "crates\server\static\$_"
                if (-not (Test-Path $local)) { return $false }
                (Get-FileHash $local -Algorithm SHA256).Hash -ne (Get-WebHash (Get-StaticUrl $prodUrl $_))
            })
    }
    if ($pending.Count) {
        Write-Warning "боевая панель ещё не отдаёт: $($pending -join ', ') — сборка образа на хостинг не закончилась, проверьте позже"
    } else {
        Write-Host "  $prodUrl отдаёт новые файлы"
    }
} elseif (-not $SkipDeploy -and -not $DryRun) {
    Write-Step 'Боевая панель: статика не менялась, проверка не нужна'
}

Write-Host ''
Write-Host 'Готово.' -ForegroundColor Green
