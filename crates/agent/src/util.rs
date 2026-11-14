use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
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
    fn names() {
        assert_eq!(basename("D:\\Projects\\demo"), "AiPanel");
        assert_eq!(basename("/home/u/proj/"), "proj");
        assert_eq!(truncate("привет", 3).chars().take(3).collect::<String>(), "при");
        assert_eq!(truncate("abc", 3), "abc");
    }
}
