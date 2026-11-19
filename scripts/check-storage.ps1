<#
.SYNOPSIS
Ищет хранение состояния в браузере: localStorage, sessionStorage, indexedDB.

.DESCRIPTION
Правило проекта: состояние интерфейса живёт на машине агента (`list.json`,
`favorites.json`, `push.json` в каталоге данных), а не в браузере. См.
AGENTS.md, «Состояние интерфейса: память агента, а не браузера».

Исключение — только с разрешения владельца проекта: файл и причина
записываются в `scripts/storage-allowlist.txt`. Запись, которая больше ничего
не разрешает, считается лишней: её надо удалить, иначе список разъедется с
кодом и перестанет быть явным.

Скрипт ничего не меняет: читает файлы под git и печатает `файл:строка`.
Код возврата: 0 — чисто, 1 — нарушения или лишняя запись allowlist,
2 — не удалось прочитать дерево git.

.PARAMETER Path
Проверить только эти файлы или каталоги (по умолчанию — всё дерево).

.PARAMETER Allowlist
Файл исключений. По умолчанию `scripts/storage-allowlist.txt`.

.EXAMPLE
pwsh scripts/check-storage.ps1

.EXAMPLE
pwsh scripts/check-storage.ps1 crates/server/static
#>

[CmdletBinding()]
param(
  [string[]]$Path = @(),
  [string]$Allowlist = (Join-Path $PSScriptRoot 'storage-allowlist.txt')
)

$ErrorActionPreference = 'Stop'

# Текст сообщений — по-русски и в UTF-8: в консоли с другой кодовой страницей
# он иначе превращается в мусор.
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}

# Что считаем хранением в браузере. Проверяем без учёта регистра: `LocalStorage`
# браузер не понимает, но такую опечатку всё равно надо увидеть.
$api = '\b(?:localStorage|sessionStorage|indexedDB)\b'

# Только код. Документация (`*.md`) называет эти API по имени нарочно, и это
# не нарушение.
$exts = @(
  '.js', '.mjs', '.cjs', '.jsx', '.ts', '.tsx',
  '.html', '.htm', '.css',
  '.rs', '.ps1', '.psm1',
  '.json', '.toml', '.yml', '.yaml'
)

$root = & git -C $PSScriptRoot rev-parse --show-toplevel 2>$null
if (-not $root) {
  Write-Host 'Не удалось определить корень репозитория: git не отвечает.'
  exit 2
}
$root = ($root | Select-Object -First 1).Trim()

# Отслеживаемые и новые (но не игнорируемые) файлы: проверка должна ловить и
# файл, который ещё не закоммитили.
$listed = & git -C $root ls-files -z --cached --others --exclude-standard
if ($LASTEXITCODE -ne 0) {
  Write-Host 'Не удалось получить список файлов: git ls-files завершился с ошибкой.'
  exit 2
}
$listed = ($listed -split "`0") | Where-Object { $_ }

$allowFull = [System.IO.Path]::GetFullPath($Allowlist).Replace('\', '/')
# Сам проверяльщик называет API по имени — это не использование.
$selfFull = [System.IO.Path]::GetFullPath($PSCommandPath).Replace('\', '/')

$scan = New-Object System.Collections.Generic.List[string]
foreach ($f in $listed) {
  $rel = $f.Replace('\', '/')
  $ext = [System.IO.Path]::GetExtension($rel).ToLowerInvariant()
  if ($exts -notcontains $ext) { continue }
  # Каталоги сборки и зависимостей: это не исходники проекта.
  if ($rel -match '(^|/)(target[^/]*|node_modules)/') { continue }
  $full = [System.IO.Path]::GetFullPath((Join-Path $root $rel)).Replace('\', '/')
  if ($full -ieq $allowFull -or $full -ieq $selfFull) { continue }
  $scan.Add($rel)
}

if ($Path.Count -gt 0) {
  $wanted = @()
  foreach ($p in $Path) {
    # Путь можно писать и от корня репозитория, и от текущего каталога.
    $abs = if ([System.IO.Path]::IsPathRooted($p)) { $p } else { Join-Path $root $p }
    if (-not (Test-Path -LiteralPath $abs)) {
      $abs = if ([System.IO.Path]::IsPathRooted($p)) { $p } else { Join-Path (Get-Location) $p }
    }
    $wanted += [System.IO.Path]::GetFullPath($abs).Replace('\', '/').TrimEnd('/')
  }
  $scan = @(
    $scan | Where-Object {
      $full = [System.IO.Path]::GetFullPath((Join-Path $root $_)).Replace('\', '/')
      $hit = $false
      foreach ($w in $wanted) {
        if ($full -ieq $w -or $full.StartsWith("$w/")) { $hit = $true; break }
      }
      $hit
    }
  )
  if ($scan.Count -eq 0) {
    Write-Host 'Под указанные пути не попал ни один файл с кодом: проверять нечего.'
    exit 2
  }
}

# Allowlist: «путь  # причина». Пустые строки и строки с `#` в начале — заметки.
$allowOrder = New-Object System.Collections.Generic.List[string]
$allowReasons = New-Object 'System.Collections.Generic.Dictionary[string,string]' ([System.StringComparer]::OrdinalIgnoreCase)
if (Test-Path -LiteralPath $Allowlist) {
  $lineNo = 0
  foreach ($line in Get-Content -LiteralPath $Allowlist) {
    $lineNo++
    $text = $line.Trim()
    if (-not $text -or $text.StartsWith('#')) { continue }
    $reason = ''
    $hash = $text.IndexOf('#')
    if ($hash -ge 0) {
      $reason = $text.Substring($hash + 1).Trim()
      $text = $text.Substring(0, $hash).Trim()
    }
    if (-not $text) { continue }
    $key = $text.Replace('\', '/').TrimStart('.', '/')
    if ($allowReasons.ContainsKey($key)) {
      Write-Host ("allowlist, строка {0}: {1} уже указан выше" -f $lineNo, $key)
      continue
    }
    $allowReasons[$key] = $reason
    $allowOrder.Add($key)
  }
}

$violations = New-Object System.Collections.Generic.List[string]
$usedAllow = New-Object 'System.Collections.Generic.Dictionary[string,int]' ([System.StringComparer]::OrdinalIgnoreCase)

foreach ($rel in $scan) {
  $full = Join-Path $root $rel
  $allowed = $allowReasons.ContainsKey($rel)
  $lineNo = 0
  foreach ($line in [System.IO.File]::ReadLines($full)) {
    $lineNo++
    foreach ($m in [regex]::Matches($line, $api, [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)) {
      if ($allowed) {
        $usedAllow[$rel] = 1 + $(if ($usedAllow.ContainsKey($rel)) { $usedAllow[$rel] } else { 0 })
      } else {
        $violations.Add(('{0}:{1}  {2}' -f $rel, $lineNo, $m.Value))
      }
    }
  }
}

$stale = New-Object System.Collections.Generic.List[string]
$scanSet = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::OrdinalIgnoreCase)
foreach ($rel in $scan) { [void]$scanSet.Add($rel) }
$listedSet = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::OrdinalIgnoreCase)
foreach ($f in $listed) { [void]$listedSet.Add($f.Replace('\', '/')) }
foreach ($key in $allowOrder) {
  if ($usedAllow.ContainsKey($key)) { continue }
  # Файл вне отбора `-Path` — не повод считать запись лишней: его просто не
  # проверяли в этом прогоне.
  if (-not $scanSet.Contains($key) -and $listedSet.Contains($key)) { continue }
  $stale.Add($key)
}

Write-Host ("Проверено файлов: {0}" -f $scan.Count)
if ($usedAllow.Count -gt 0) {
  Write-Host 'Разрешено (storage-allowlist.txt):'
  foreach ($key in $allowOrder) {
    if ($usedAllow.ContainsKey($key)) {
      Write-Host ("  {0}  — {1} ({2})" -f $key, $allowReasons[$key], $usedAllow[$key])
    }
  }
}

if ($violations.Count -gt 0) {
  Write-Host ''
  Write-Host 'Нарушения: обращение к хранилищу браузера.'
  foreach ($v in $violations) { Write-Host "  $v" }
  Write-Host ''
  Write-Host 'Храните его на машине агента (`list.json`, `favorites.json`, `push.json`) —'
  Write-Host 'см. AGENTS.md, «Состояние интерфейса: память агента, а не браузера».'
  Write-Host 'Исключение — только по разрешению владельца проекта: файл и причина'
  Write-Host 'в scripts/storage-allowlist.txt.'
}

if ($stale.Count -gt 0) {
  Write-Host ''
  Write-Host 'Лишние записи в storage-allowlist.txt:'
  foreach ($key in $stale) {
    if ($listedSet.Contains($key)) {
      Write-Host "  $key  — в файле больше нет хранилища в браузере"
    } else {
      Write-Host "  $key  — такого файла нет"
    }
  }
  Write-Host 'Удалите строки: разрешение без использования только запутывает.'
}

if ($violations.Count -gt 0 -or $stale.Count -gt 0) { exit 1 }
Write-Host 'Хранения в браузере нет.'
exit 0
