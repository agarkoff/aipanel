<#
.SYNOPSIS
Ищет в файлах строки из локального списка приватных данных.

.DESCRIPTION
Репозиторий публикуется на GitHub, поэтому адреса, имена проектов и пути этой
машины живут в `.local/infra.md`, а не в коде и документации.

Что именно считается приватным, знает только эта машина: список лежит в
`.local/private-patterns.txt` — по строке на правило, `#` в начале строки —
комментарий, регистр не важен. В репозитории списка нет намеренно: он сам был
бы подсказкой, что именно скрывают.

Если списка нет (например, репозиторий только что склонировали на другой
машине), проверять нечего: скрипт выходит с кодом 0 и говорит об этом.

Читаются файлы под git и новые, ещё не закоммиченные: утечка видна до коммита.
Печатается `файл:строка  совпадение`.

Код возврата: 0 — чисто или списка нет, 1 — найдены совпадения,
2 — не удалось прочитать дерево git.

.PARAMETER Path
Проверить только эти файлы или каталоги (по умолчанию — всё дерево).

.PARAMETER Patterns
Файл со списком. По умолчанию — `.local/private-patterns.txt` в корне репозитория.

.EXAMPLE
pwsh scripts/check-private.ps1

.EXAMPLE
pwsh scripts/check-private.ps1 scripts
#>

[CmdletBinding()]
param(
  [string[]]$Path = @(),
  [string]$Patterns = ''
)

$ErrorActionPreference = 'Stop'

# Текст сообщений — по-русски и в UTF-8: в консоли с другой кодовой страницей
# он иначе превращается в мусор.
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}

# Только текст. Картинки, шрифты и архивы не читаем: там этих строк не бывает
# в читаемом виде, а на бинарниках проверка только тормозит.
$exts = @(
  '.js', '.mjs', '.cjs', '.jsx', '.ts', '.tsx',
  '.html', '.htm', '.css', '.svg', '.json', '.webmanifest',
  '.rs', '.toml', '.lock', '.yml', '.yaml', '.ps1', '.psm1', '.cmd', '.bat',
  '.md', '.txt', '.example', '.sh', '.py', '.sql'
)
# Файлы без расширения, которые всё равно текст.
$names = @('.gitignore', '.gitattributes', '.dockerignore', '.editorconfig', 'Dockerfile')

$root = & git -C $PSScriptRoot rev-parse --show-toplevel 2>$null
if (-not $root) {
  Write-Host 'Не удалось определить корень репозитория: git не отвечает.'
  exit 2
}
$root = ($root | Select-Object -First 1).Trim()

if (-not $Patterns) { $Patterns = Join-Path $root '.local/private-patterns.txt' }
if (-not (Test-Path -LiteralPath $Patterns)) {
  Write-Host ("Списка приватных строк нет: {0}" -f $Patterns)
  Write-Host 'Проверять нечего — это не ошибка: список ведётся на машине владельца.'
  exit 0
}

# Список: «строка на правило», `#` — комментарий. Обычная строка — подстрока
# без учёта регистра; `regex:<шаблон>` — регулярное выражение.
$literals = @()
$regexes = @()
foreach ($line in Get-Content -LiteralPath $Patterns) {
  $text = $line.Trim()
  if (-not $text -or $text.StartsWith('#')) { continue }
  if ($text -match '^regex:(.+)$') {
    $regexes += [regex]::new($Matches[1], [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)
  } else {
    $literals += $text
  }
}
if ($literals.Count -eq 0 -and $regexes.Count -eq 0) {
  Write-Host ("В {0} нет ни одного правила: проверять нечего." -f $Patterns)
  exit 0
}

# Отслеживаемые и новые (но не игнорируемые) файлы.
$listed = & git -C $root ls-files -z --cached --others --exclude-standard
if ($LASTEXITCODE -ne 0) {
  Write-Host 'Не удалось получить список файлов: git ls-files завершился с ошибкой.'
  exit 2
}
$listed = ($listed -split "`0") | Where-Object { $_ }

# Сам список приватных строк тоже текст: его не проверяем.
$patternsFull = [System.IO.Path]::GetFullPath($Patterns).Replace('\', '/')

$scan = New-Object System.Collections.Generic.List[string]
foreach ($f in $listed) {
  $rel = $f.Replace('\', '/')
  $leaf = [System.IO.Path]::GetFileName($rel)
  $ext = [System.IO.Path]::GetExtension($rel).ToLowerInvariant()
  if ($exts -notcontains $ext -and $names -notcontains $leaf) { continue }
  # Каталоги сборки и зависимостей: это не исходники проекта.
  if ($rel -match '(^|/)(target[^/]*|node_modules)/') { continue }
  $full = [System.IO.Path]::GetFullPath((Join-Path $root $rel)).Replace('\', '/')
  if ($full -ieq $patternsFull) { continue }
  $scan.Add($rel)
}

if ($Path.Count -gt 0) {
  $wanted = @()
  foreach ($p in $Path) {
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
    Write-Host 'Под указанные пути не попал ни один файл: проверять нечего.'
    exit 2
  }
}

$violations = New-Object System.Collections.Generic.List[string]
foreach ($rel in $scan) {
  $lineNo = 0
  foreach ($line in [System.IO.File]::ReadLines((Join-Path $root $rel))) {
    $lineNo++
    $found = @()
    foreach ($p in $literals) {
      if ($line.IndexOf($p, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) { $found += $p }
    }
    foreach ($re in $regexes) {
      if ($re.IsMatch($line)) { $found += ('regex:' + $re.ToString()) }
    }
    if ($found.Count -gt 0) {
      $violations.Add(('{0}:{1}  {2}' -f $rel, $lineNo, ($found -join ', ')))
    }
  }
}

Write-Host ("Проверено файлов: {0}" -f $scan.Count)
if ($violations.Count -gt 0) {
  Write-Host ''
  Write-Host 'Приватные строки в файлах, которые попадут в репозиторий:'
  foreach ($v in $violations) { Write-Host "  $v" }
  Write-Host ''
  Write-Host 'Адреса панели и хостинга, пути к копиям агента и каталог проекта держите'
  Write-Host 'в .local/infra.md (каталог в .gitignore): в репозитории их быть не должно.'
  exit 1
}

Write-Host 'Приватных строк нет.'
exit 0
