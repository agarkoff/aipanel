use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// `2026-11-05 10:06:22Z` по мс с эпохи Unix (UTC).
pub fn iso_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // civil_from_days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Предел размера лога: при превышении старое содержимое удаляется, лог начинается заново.
const LOG_MAX_BYTES: u64 = 512 * 1024;

/// Пишет строку в stderr и в `~/.aipanel/agent.log` с временем.
pub fn log(message: &str) {
    use std::io::Write;
    eprintln!("{message}");
    let dir = aipanel_dir();
    let path = dir.join("agent.log");
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX_BYTES) {
        let _ = std::fs::remove_file(&path);
    }
    // Файл прежней схемы ротации больше не нужен.
    let _ = std::fs::remove_file(dir.join("agent.log.1"));
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} {message}", iso_utc(now_ms()));
    }
}

/// Каталог данных AiPanel (`~/.aipanel`): сюда плагины окон пишут свои отчёты.
pub fn aipanel_dir() -> PathBuf {
    if let Some(dir) = HOME_OVERRIDE.get() {
        return dir.clone();
    }
    std::env::var_os("AIPANEL_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".aipanel"))
}

static HOME_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Каталоги, где плагин opencode оставляет отчёты об открытых окнах.
///
/// Плагин берёт `AIPANEL_HOME` из окружения процесса opencode, а там его обычно
/// нет даже у второго экземпляра агента с `--home`: отчёты уходят в `~/.aipanel`.
/// Поэтому смотрим оба места — профильное и общее, иначе второй агент считает,
/// что окон нет, и открывает дубли вместо того, чтобы писать в уже открытую сессию.
pub fn window_reports_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![aipanel_dir().join("opencode")];
    let shared = home_dir().join(".aipanel").join("opencode");
    if !dirs.contains(&shared) {
        dirs.push(shared);
    }
    dirs
}

/// Переопределяет каталог данных (флаг `--home`). Нужен второму экземпляру
/// агента, чтобы его состояние, лог и pid не пересекались с основным.
pub fn set_aipanel_dir(dir: PathBuf) {
    let _ = HOME_OVERRIDE.set(dir);
}

/// Полный путь к программе: по PATH, затем в `~/.opencode/bin`. Агент может быть
/// запущен с урезанным PATH, в котором нет каталога установки opencode.
pub fn resolve_exe(bin: &str) -> String {
    if bin.contains(['/', '\\']) {
        return bin.to_owned();
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in [format!("{bin}.exe"), bin.to_owned()] {
                let p = dir.join(name);
                if p.is_file() {
                    return p.to_string_lossy().into_owned();
                }
            }
        }
    }
    let fallback = home_dir().join(".opencode").join("bin").join(format!("{bin}.exe"));
    if fallback.is_file() {
        return fallback.to_string_lossy().into_owned();
    }
    bin.to_owned()
}

pub fn now_ms() -> i64 {
    to_ms(SystemTime::now())
}

/// Начало ошибки агента «такого каталога нет». По этому началу панель
/// (`crates/server/static/app.js`, `DIR_MISSING`) предлагает создать каталог
/// и повторяет запуск с флагом `create`.
pub const DIR_MISSING: &str = "каталог не найден: ";

/// Каталог для новой сессии должен быть существующим абсолютным путём:
/// относительный путь зависел бы от каталога, из которого запущен агент.
pub fn session_dir(cwd: &str) -> Result<&Path, String> {
    session_dir_with(cwd, false)
}

/// Как `session_dir`, но `create` разрешает создать отсутствующий каталог:
/// панель спрашивает об этом у человека и повторяет запуск.
pub fn session_dir_with(cwd: &str, create: bool) -> Result<&Path, String> {
    let dir = Path::new(cwd);
    if !dir.is_absolute() {
        return Err(format!("укажите абсолютный путь к каталогу: {cwd}"));
    }
    if !dir.is_dir() {
        if !create {
            return Err(format!("{DIR_MISSING}{cwd}"));
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("не удалось создать каталог {cwd}: {e}"))?;
    }
    Ok(dir)
}

pub fn to_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Обрезает строку до `max` символов, не разрывая символ UTF-8.
pub fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((idx, _)) => format!("{}…\n[обрезано, всего {} симв.]", &s[..idx], s.chars().count()),
        None => s.to_owned(),
    }
}

/// Последний компонент пути с любым разделителем.
pub fn basename(path: &str) -> &str {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Короткое описание входа инструмента: команда или путь, иначе JSON.
pub fn tool_input(input: &Value) -> String {
    for key in ["command", "file_path", "path", "pattern", "url", "query", "prompt"] {
        if let Some(s) = input.get(key).and_then(Value::as_str) {
            let rest = input.as_object().map(|o| o.len()).unwrap_or(1);
            if rest == 1 || key == "command" {
                return truncate(s, 4000);
            }
            break;
        }
    }
    truncate(&serde_json::to_string_pretty(input).unwrap_or_default(), 4000)
}

/// Предел размера диффа в ленте: большие правки не должны раздувать сообщение.
pub const DIFF_LIMIT: usize = 40_000;

/// Инструменты, которые меняют файлы: их содержимое показывается диффом.
pub fn is_edit_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "edit" | "multiedit" | "write" | "notebookedit" | "patch" | "apply_patch"
    )
}

/// Инструменты, которые запускают подагента: `Agent` у Claude Code (раньше
/// `Task`) и `subagent` у opencode (в старых версиях `task`). Их вызовы в ленту
/// сессии не попадают — панель показывает подагентов отдельным списком.
pub fn is_subagent_tool(name: &str) -> bool {
    matches!(name, "Agent" | "Task" | "subagent" | "task")
}

pub fn tool_path(input: &Value) -> Option<&str> {
    input.get("file_path").or_else(|| input.get("path")).and_then(Value::as_str)
}

/// Текст вызова инструмента: для правок файлов достаточно пути, содержимое
/// приходит отдельным элементом ленты с диффом.
pub fn tool_text(name: Option<&str>, input: &Value) -> String {
    match name.filter(|n| is_edit_tool(n)).and_then(|_| tool_path(input)) {
        Some(path) => truncate(path, 4000),
        None => tool_input(input),
    }
}

/// Дифф старого и нового фрагмента без готового патча. Общие строки в начале
/// и конце показываются контекстом, между ними — удалённое и добавленное.
pub fn edit_diff(path: &str, old: &str, new: &str) -> String {
    let old = lines(old);
    let new = lines(new);
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len().saturating_sub(prefix)
        && suffix < new.len().saturating_sub(prefix)
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let path = slash_path(path);
    let mut out = format!("--- a/{path}\n+++ b/{path}\n@@ -1,{} +1,{} @@\n", old.len(), new.len());
    for line in &old[..prefix] {
        push_line(&mut out, ' ', line);
    }
    for line in &old[prefix..old.len() - suffix] {
        push_line(&mut out, '-', line);
    }
    for line in &new[prefix..new.len() - suffix] {
        push_line(&mut out, '+', line);
    }
    for line in &old[old.len() - suffix..] {
        push_line(&mut out, ' ', line);
    }
    out
}

/// Дифф нового файла: все строки добавленные.
pub fn create_diff(path: &str, content: &str) -> String {
    let lines = lines(content);
    let mut out = format!("--- /dev/null\n+++ b/{}\n@@ -0,0 +1,{} @@\n", slash_path(path), lines.len());
    for line in &lines {
        push_line(&mut out, '+', line);
    }
    out
}

fn lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn slash_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn push_line(out: &mut String, sign: char, line: &str) {
    out.push(sign);
    out.push_str(line);
    out.push('\n');
}

/// `2026-11-05T10:06:22.084Z` -> миллисекунды с эпохи Unix. 0 при ошибке разбора.
pub fn parse_iso_ms(s: &str) -> i64 {
    let b = s.as_bytes();
    if b.len() < 19 {
        return 0;
    }
    let num = |from: usize, to: usize| s.get(from..to).and_then(|p| p.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(sec)) =
        (num(0, 4), num(5, 7), num(8, 10), num(11, 13), num(14, 16), num(17, 19))
    else {
        return 0;
    };
    let ms = if b.get(19) == Some(&b'.') { num(20, 23).unwrap_or(0) } else { 0 };
    // Алгоритм days_from_civil (Howard Hinnant).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    ((days * 24 + h) * 60 + mi) * 60_000 + sec * 1000 + ms
}

/// Остаток окна лимита: API отдаёт процент использования, панель показывает
/// противоположное — сколько ещё доступно. `resets_at` — ISO-время сброса (UTC).
pub fn meter(label: &str, used: f64, resets_at: Option<&str>) -> aipanel_proto::Meter {
    aipanel_proto::Meter {
        label: label.to_owned(),
        left: (100.0 - used).clamp(0.0, 100.0).round() as u8,
        resets_at: resets_at.map(parse_iso_ms).unwrap_or(0),
    }
}

/// Разбирает текст, который начинается с команды `/имя`: возвращает имя без
/// слэша и остаток строки как аргументы. `None`, если команда не в начале или
/// вместо имени путь (`/home/user/…`): такой текст — обычное сообщение.
pub fn slash_command(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim_start().strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..end];
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return None;
    }
    Some((name, rest[end..].trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00.000Z"), 0);
        assert_eq!(parse_iso_ms("2026-11-05T10:06:22.084Z"), 1_793_873_182_084);
        assert_eq!(parse_iso_ms("garbage"), 0);
    }

    #[test]
    fn iso_output() {
        assert_eq!(iso_utc(0), "1970-01-01 00:00:00Z");
        assert_eq!(iso_utc(1_793_873_182_084), "2026-11-05 10:06:22Z");
        assert_eq!(iso_utc(parse_iso_ms("2024-02-29T23:59:59.000Z")), "2024-02-29 23:59:59Z");
    }

    #[test]
    fn names() {
        assert_eq!(basename("D:\\Projects\\demo"), "demo");
        assert_eq!(basename("/home/u/proj/"), "proj");
        assert_eq!(truncate("привет", 3).chars().take(3).collect::<String>(), "при");
        assert_eq!(truncate("abc", 3), "abc");
    }

    #[test]
    fn session_dirs() {
        let base = std::env::temp_dir().join(format!("aipanel-util-{}", now_ms()));
        let dir = base.join("нет-ещё");
        let path = dir.to_string_lossy().to_string();
        assert!(session_dir(&path).unwrap_err().starts_with(DIR_MISSING));
        assert!(!dir.exists());
        // Без разрешения каталог не создаётся, с разрешением — создаётся.
        assert!(session_dir_with(&path, false).is_err());
        assert!(!dir.exists());
        assert!(session_dir_with(&path, true).is_ok());
        assert!(dir.is_dir());
        assert!(session_dir(&path).is_ok());
        let rel = session_dir_with("относительный/путь", true).unwrap_err();
        assert!(rel.contains("абсолютный"), "{rel}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn diffs() {
        assert_eq!(
            edit_diff("D:\\p\\a.rs", "one\ntwo\nthree", "one\n2\nthree"),
            "--- a/D:/p/a.rs\n+++ b/D:/p/a.rs\n@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"
        );
        assert_eq!(
            create_diff("n.txt", "x\ny\n"),
            "--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n"
        );
        // Вставка и удаление без общих строк.
        assert_eq!(
            edit_diff("a", "", "new"),
            "--- a/a\n+++ b/a\n@@ -1,0 +1,1 @@\n+new\n"
        );
    }

    #[test]
    fn slash_commands() {
        assert_eq!(slash_command("/init"), Some(("init", "")));
        assert_eq!(slash_command("/review main"), Some(("review", "main")));
        assert_eq!(slash_command("  /rea:trace_crash файл"), Some(("rea:trace_crash", "файл")));
        assert_eq!(slash_command("/init\nчто-то"), Some(("init", "что-то")));
        // Путь или обычный текст командой не считается.
        assert_eq!(slash_command("/home/user/файл"), None);
        assert_eq!(slash_command("/"), None);
        assert_eq!(slash_command("текст"), None);
        assert_eq!(slash_command("смотри /init"), None);
    }

    #[test]
    fn edit_tools() {
        assert!(is_edit_tool("Edit"));
        assert!(is_edit_tool("write"));
        assert!(!is_edit_tool("Read"));
        let input = serde_json::json!({"file_path":"D:\\p\\a.rs","old_string":"a","new_string":"b"});
        assert_eq!(tool_text(Some("Edit"), &input), "D:\\p\\a.rs");
        assert_eq!(
            tool_text(Some("Read"), &serde_json::json!({"file_path":"D:\\p\\a.rs"})),
            "D:\\p\\a.rs"
        );
    }
}
