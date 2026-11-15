//! Состояние агента для консольных команд `status` и `stop`.
//!
//! Работающий агент раз в 2 секунды перезаписывает `~/.aipanel/agent.json`. Агентом
//! считается процесс, чей файл свежий и чей pid жив: так не нужны ни служебный порт,
//! ни служба.

use std::{
    io::Read,
    process::{Command, Stdio},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::util::{aipanel_dir, now_ms};

const FRESH_MS: i64 = 15_000;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Status {
    pub pid: u32,
    pub version: String,
    pub name: String,
    pub server: String,
    pub started: i64,
    /// Когда установлено соединение с сервером; 0 — соединения нет.
    pub connected_since: i64,
    pub last_error: String,
    pub sessions: usize,
    pub updated: i64,
}

fn path() -> std::path::PathBuf {
    aipanel_dir().join("agent.json")
}

pub fn write(status: &Status) {
    let dir = aipanel_dir();
    let _ = std::fs::create_dir_all(&dir);
    let tmp = dir.join("agent.json.tmp");
    if let Ok(text) = serde_json::to_string(status) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path());
        }
    }
}

/// Работающий агент: файл состояния свежий и процесс жив.
pub fn running() -> Option<Status> {
    let status: Status = serde_json::from_str(&std::fs::read_to_string(path()).ok()?).ok()?;
    (now_ms() - status.updated < FRESH_MS && crate::platform::pid_alive(status.pid)).then_some(status)
}

fn human(ms: i64) -> String {
    let s = ms.max(0) as u64 / 1000;
    match s {
        0..=59 => format!("{s} с"),
        60..=3599 => format!("{} мин", s / 60),
        3600..=86399 => format!("{} ч {} мин", s / 3600, s % 3600 / 60),
        _ => format!("{} дн {} ч", s / 86400, s % 86400 / 3600),
    }
}

fn print(status: &Status) {
    let now = now_ms();
    println!("Агент работает (pid {}, запущен {} назад)", status.pid, human(now - status.started));
    println!("  Имя:        {}", status.name);
    println!("  Сервер:     {}", status.server);
    if status.connected_since > 0 {
        println!("  Соединение: подключён {} назад", human(now - status.connected_since));
    } else if status.last_error.is_empty() {
        println!("  Соединение: устанавливается");
    } else {
        println!("  Соединение: нет ({})", status.last_error);
    }
    println!("  Сессий:     {}", status.sessions);
    println!("  Версия:     {}", status.version);
}

fn log_tail(path: &std::path::Path) -> String {
    let mut text = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_to_string(&mut text);
    }
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(8)..].join("\n")
}

/// `status`: показывает состояние; если агент не запущен, запускает его (кроме `--no-start`).
/// Код выхода: 0 — работает и подключён, 2 — работает, но не подключён, 1 — не запущен.
pub fn cmd_status(start: bool, config: Option<&str>, home: Option<&str>) -> i32 {
    if let Some(status) = running() {
        print(&status);
        return if status.connected_since > 0 { 0 } else { 2 };
    }
    if !start {
        println!("Агент не запущен.");
        return 1;
    }
    println!("Агент не запущен, запускаю…");

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("не удалось определить путь к программе: {e}");
            return 1;
        }
    };
    // Лог пишет сам агент (с ограничением размера), здесь его потоки не нужны.
    let log = aipanel_dir().join("agent.log");
    let mut cmd = Command::new(exe);
    if let Some(config) = config {
        cmd.args(["--config", config]);
    }
    if let Some(home) = home {
        cmd.args(["--home", home]);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::platform::detach(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("не удалось запустить агента: {e}");
            return 1;
        }
    };

    for _ in 0..24 {
        std::thread::sleep(Duration::from_millis(500));
        if let Some(status) = running().filter(|s| s.pid == child.id()) {
            if status.connected_since > 0 {
                print(&status);
                println!("  Лог:        {}", log.display());
                return 0;
            }
        }
        if let Ok(Some(exit)) = child.try_wait() {
            eprintln!("Агент завершился сразу после запуска ({exit}). Лог {}:\n{}", log.display(), log_tail(&log));
            return 1;
        }
    }
    match running() {
        Some(status) => {
            print(&status);
            println!("  Лог:        {}", log.display());
            2
        }
        None => {
            eprintln!("Агент не ответил за 12 секунд. Лог {}:\n{}", log.display(), log_tail(&log));
            1
        }
    }
}

/// `stop`: останавливает работающий агент.
pub fn cmd_stop() -> i32 {
    let Some(status) = running() else {
        println!("Агент не запущен.");
        return 0;
    };
    if !crate::platform::kill_pid(status.pid) {
        eprintln!("не удалось остановить процесс {}", status.pid);
        return 1;
    }
    for _ in 0..20 {
        if !crate::platform::pid_alive(status.pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = std::fs::remove_file(path());
    println!("Агент остановлен (pid {}).", status.pid);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(human(5_000), "5 с");
        assert_eq!(human(5 * 60_000), "5 мин");
        assert_eq!(human(2 * 3_600_000 + 3 * 60_000), "2 ч 3 мин");
        assert_eq!(human(-1), "0 с");
    }
}
