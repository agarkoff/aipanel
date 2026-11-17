//! Фоновые задачи сессии Claude Code: команды `Bash`/`PowerShell` с
//! `run_in_background`, фоновые агенты (`Agent`) и мониторы (`Monitor`).
//!
//! Claude Code держит их список в памяти процесса и показывает в окне, а в
//! транскрипт пишет только события. Список восстанавливается по ним (формат
//! снят с Claude Code 2.1.325):
//!
//! - запуск — результат вызова: `toolUseResult.backgroundTaskId` у команды,
//!   `toolUseResult.status = "async_launched"` и `agentId` у агента,
//!   `toolUseResult.taskId` (и `timeoutMs`) у монитора;
//! - завершение — уведомление `<task-notification>` с `<task-id>` и
//!   `<status>` (`completed`, `failed`, `killed`, `stopped`). Оно приходит
//!   строкой запроса, вложением `queued_command` или операцией очереди;
//! - остановка инструментом `TaskStop` — `toolUseResult.task_id`.
//!
//! Задачи живут в процессе Claude Code: когда он завершился, список пуст,
//! что бы ни осталось в транскрипте.

use std::collections::HashMap;

use aipanel_proto::BgTask;
use serde_json::Value;

use crate::util::parse_iso_ms;

/// Подпись задачи длиннее этого обрезается.
const TITLE_LIMIT: usize = 200;
/// Незакрытых вызовов, ждущих результата, больше этого не держим.
const CALLS_LIMIT: usize = 256;

/// Вызов, результат которого может оказаться запуском фоновой задачи.
#[derive(Clone)]
struct Call {
    name: String,
    title: String,
}

#[derive(Default)]
pub struct Tracker {
    calls: HashMap<String, Call>,
    /// Запущенные задачи в порядке запуска.
    running: Vec<BgTask>,
}

impl Tracker {
    /// Задачи, которые ещё не завершились. Монитор с истёкшим сроком
    /// не показывается, даже если уведомления о конце в транскрипте нет.
    pub fn running(&self, now: i64) -> Vec<BgTask> {
        self.running.iter().filter(|t| t.ends_at == 0 || t.ends_at > now).cloned().collect()
    }

    pub fn line(&mut self, line: &str) {
        if line.contains("<task-notification>") {
            self.notification(line);
        }
        if line.starts_with("{\"type\":\"queue-operation\"") {
            return;
        }
        let assistant = line.contains("\"type\":\"assistant\"");
        if assistant && !line.contains("\"tool_use\"") {
            return;
        }
        if !assistant && !line.contains("\"toolUseResult\"") {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        if v["isSidechain"].as_bool() == Some(true) {
            return;
        }
        match v["type"].as_str() {
            Some("assistant") => self.calls(&v),
            Some("user") => self.result(&v),
            _ => {}
        }
    }

    fn calls(&mut self, v: &Value) {
        for b in v["message"]["content"].as_array().into_iter().flatten() {
            if b["type"].as_str() != Some("tool_use") {
                continue;
            }
            let (Some(id), Some(name)) = (b["id"].as_str(), b["name"].as_str()) else { continue };
            let input = &b["input"];
            let title = match name {
                "Bash" | "PowerShell" | "Monitor" => first_of(input, &["description", "command"]),
                "Agent" | "Task" => first_of(input, &["description", "prompt"]),
                _ => continue,
            };
            if self.calls.len() >= CALLS_LIMIT {
                self.calls.clear();
            }
            self.calls.insert(id.to_owned(), Call { name: name.to_owned(), title });
        }
    }

    fn result(&mut self, v: &Value) {
        let r = &v["toolUseResult"];
        if let Some(id) = r["task_id"].as_str() {
            // TaskStop: задача остановлена.
            self.finish(id);
            return;
        }
        let ts = v["timestamp"].as_str().map(parse_iso_ms).unwrap_or(0);
        for b in v["message"]["content"].as_array().into_iter().flatten() {
            if b["type"].as_str() != Some("tool_result") {
                continue;
            }
            let Some(call) = b["tool_use_id"].as_str().and_then(|id| self.calls.remove(id)) else { continue };
            let (id, kind, title, ends_at) = if let Some(id) = r["backgroundTaskId"].as_str() {
                (id, "bash", call.title, 0)
            } else if r["status"].as_str() == Some("async_launched") {
                let Some(id) = r["agentId"].as_str() else { continue };
                let title = r["description"].as_str().map(str::to_owned).unwrap_or(call.title);
                (id, "agent", title, 0)
            } else if call.name == "Monitor" {
                let Some(id) = r["taskId"].as_str() else { continue };
                let ends = match (r["persistent"].as_bool(), r["timeoutMs"].as_i64()) {
                    (Some(true), _) | (_, None) => 0,
                    (_, Some(ms)) if ts > 0 => ts + ms,
                    _ => 0,
                };
                (id, "monitor", call.title, ends)
            } else {
                continue;
            };
            self.finish(id);
            self.running.push(BgTask {
                id: id.to_owned(),
                kind: kind.into(),
                title: crate::util::truncate(&title, TITLE_LIMIT),
                started_at: ts,
                ends_at,
            });
        }
    }

    /// Уведомление о задаче: убирает её из списка, если статус конечный.
    fn notification(&mut self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        if v["isSidechain"].as_bool() == Some(true) {
            return;
        }
        let text = match v["type"].as_str() {
            Some("queue-operation") if v["operation"].as_str() == Some("enqueue") => v["content"].as_str(),
            Some("attachment") if v["attachment"]["type"].as_str() == Some("queued_command") => {
                v["attachment"]["prompt"].as_str()
            }
            Some("user") => v["message"]["content"].as_str(),
            _ => None,
        };
        let Some(text) = text else { return };
        for note in text.split("<task-notification>").skip(1) {
            let note = note.split("</task-notification>").next().unwrap_or_default();
            let (Some(id), Some(status)) = (tag(note, "task-id"), tag(note, "status")) else { continue };
            if matches!(status, "completed" | "failed" | "killed" | "stopped" | "cancelled" | "error") {
                self.finish(id);
            }
        }
    }

    fn finish(&mut self, id: &str) {
        self.running.retain(|t| t.id != id);
    }
}

/// Текст первого непустого строкового поля, в одну строку.
fn first_of(input: &Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|k| input[*k].as_str())
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}

/// Содержимое `<name>…</name>` без пробелов по краям.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let rest = &text[text.find(&open)? + open.len()..];
    let value = rest[..rest.find(&close)?].trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &mut Tracker, lines: &[&str]) {
        for l in lines {
            t.line(l);
        }
    }

    const BASH_CALL: &str = r#"{"type":"assistant","uuid":"u1","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"docker build .","description":"Build image","run_in_background":true}}]}}"#;
    const BASH_RESULT: &str = r#"{"type":"user","uuid":"u2","timestamp":"2026-11-17T09:14:29.282Z","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"Command running in background with ID: bpn8xp4ds."}]},"toolUseResult":{"stdout":"","backgroundTaskId":"bpn8xp4ds"}}"#;

    #[test]
    fn bash_runs_until_notification() {
        let mut t = Tracker::default();
        feed(&mut t, &[BASH_CALL, BASH_RESULT]);
        let r = t.running(0);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].id.as_str(), r[0].kind.as_str(), r[0].title.as_str()), ("bpn8xp4ds", "bash", "Build image"));
        assert!(r[0].started_at > 0);
        // Уведомление с промежуточным статусом задачу не снимает.
        t.line(r#"{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>bpn8xp4ds</task-id>\n<status>running</status>\n</task-notification>"},"origin":{"kind":"task-notification"}}"#);
        assert_eq!(t.running(0).len(), 1);
        t.line(r#"{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>bpn8xp4ds</task-id>\n<status>completed</status>\n</task-notification>"}"#);
        assert!(t.running(0).is_empty());
    }

    #[test]
    fn queued_notification_finishes_task() {
        let mut t = Tracker::default();
        feed(&mut t, &[BASH_CALL, BASH_RESULT]);
        t.line(r#"{"type":"attachment","uuid":"a","attachment":{"type":"queued_command","prompt":"<task-notification>\n<task-id>bpn8xp4ds</task-id>\n<status>killed</status>\n</task-notification>","origin":{"kind":"task-notification"}}}"#);
        assert!(t.running(0).is_empty());
    }

    #[test]
    fn task_stop_finishes_task() {
        let mut t = Tracker::default();
        feed(&mut t, &[BASH_CALL, BASH_RESULT]);
        t.line(r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_9","type":"tool_result","content":"stopped"}]},"toolUseResult":{"message":"Successfully stopped task","task_id":"bpn8xp4ds","task_type":"local_bash"}}"#);
        assert!(t.running(0).is_empty());
    }

    #[test]
    fn foreground_command_is_not_a_task() {
        let mut t = Tracker::default();
        t.line(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_2","name":"Bash","input":{"command":"ls"}}]}}"#);
        t.line(r#"{"type":"user","message":{"content":[{"tool_use_id":"toolu_2","type":"tool_result","content":"a"}]},"toolUseResult":{"stdout":"a","stderr":""}}"#);
        assert!(t.running(0).is_empty());
        assert!(t.calls.is_empty());
    }

    #[test]
    fn agent_and_monitor() {
        let mut t = Tracker::default();
        t.line(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_a","name":"Agent","input":{"description":"Research","prompt":"long","run_in_background":true}},{"type":"tool_use","id":"toolu_m","name":"Monitor","input":{"command":"tail -f log","description":"Watch log"}}]}}"#);
        t.line(r#"{"type":"user","timestamp":"2026-11-17T09:00:00.000Z","message":{"content":[{"tool_use_id":"toolu_a","type":"tool_result","content":"launched"}]},"toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"a026fc17c2afd0e27","description":"Баг 5"}}"#);
        t.line(r#"{"type":"user","timestamp":"2026-11-17T09:00:00.000Z","message":{"content":[{"tool_use_id":"toolu_m","type":"tool_result","content":"Monitor started"}]},"toolUseResult":{"taskId":"bfvebr2ar","timeoutMs":400000,"persistent":false}}"#);
        let r = t.running(0);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].kind.as_str(), r[0].title.as_str()), ("agent", "Баг 5"));
        assert_eq!((r[1].kind.as_str(), r[1].title.as_str()), ("monitor", "Watch log"));
        // Срок монитора истёк — его больше нет.
        let after = r[1].ends_at + 1;
        assert_eq!(t.running(after).len(), 1);
        t.line(r#"{"type":"user","message":{"content":"<task-notification><task-id>a026fc17c2afd0e27</task-id><status>failed</status></task-notification>"}}"#);
        assert!(t.running(after).is_empty());
    }

    #[test]
    fn sidechain_lines_are_ignored() {
        let mut t = Tracker::default();
        t.line(&BASH_CALL.replace("\"uuid\"", "\"isSidechain\":true,\"uuid\""));
        t.line(BASH_RESULT);
        assert!(t.running(0).is_empty());
    }
}
