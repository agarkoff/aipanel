//! Фоновые задачи сессии opencode: команды `shell` с `background: true` и
//! подагенты (`subagent` — дочерняя сессия, которая всегда работает в фоне).
//!
//! В отличие от Claude Code, у opencode нет отдельного списка задач: работа
//! видна в сообщениях сессии и в потоке событий (снято с opencode 2.2.7).
//!
//! - запуск команды — часть `tool` с `name = "shell"`, `input.background = true`
//!   и `state.metadata.shellID`; сам вызов при этом уже `completed`, потому что
//!   команда возвращается сразу;
//! - запуск подагента — часть `tool` с `name = "subagent"` (в старых версиях
//!   `task`), `state.metadata.status = "running"` и `state.metadata.sessionID`;
//! - завершение — сообщение `synthetic` с `metadata.source = "shell"` (по
//!   `shellID`) или `"subagent"` (по `childID`), а у команды ещё и событие
//!   `shell.exited` / `shell.deleted`.
//!
//! Пока сессия открыта в панели, агент опрашивает её сообщения и слушает поток
//! событий — оттуда и берётся список.

use aipanel_proto::{BgTask, MAX_TASKS};
use serde_json::Value;

use crate::util::truncate;

/// Подпись задачи длиннее этого обрезается.
const TITLE_LIMIT: usize = 200;

#[derive(Default)]
pub struct Tracker {
    /// Работающие задачи в порядке запуска.
    running: Vec<BgTask>,
}

impl Tracker {
    /// Задачи, которые ещё не завершились.
    pub fn running(&self) -> Vec<BgTask> {
        self.running.clone()
    }

    /// Событие потока opencode. Нужны только концы команд: запуск берётся из
    /// сообщений — по событиям не отличить фоновую команду от обычной.
    pub fn event(&mut self, ev: &Value) {
        let kind = ev["type"].as_str().unwrap_or_default();
        if !(kind == "shell.exited" || kind == "shell.deleted") {
            return;
        }
        if let Some(id) = ev["data"]["id"].as_str() {
            self.finish(id);
        }
    }

    /// Сообщение сессии: запуски задач и уведомления об их конце.
    pub fn message(&mut self, m: &Value) {
        match m["type"].as_str() {
            Some("assistant") => self.calls(m),
            Some("synthetic") => self.finished(m),
            _ => {}
        }
    }

    fn calls(&mut self, m: &Value) {
        let at = m["time"]["created"].as_i64().unwrap_or(0);
        for c in m["content"].as_array().into_iter().flatten() {
            if c["type"].as_str() != Some("tool") {
                continue;
            }
            let state = &c["state"];
            match c["name"].as_str().unwrap_or_default() {
                // Подагент работает, даже когда вызов уже вернул управление:
                // `state.status` у него `completed`, а `metadata.status` — `running`.
                "subagent" | "task" => {
                    if state["metadata"]["status"].as_str() != Some("running") {
                        continue;
                    }
                    let id = state["metadata"]["sessionID"].as_str().or(c["id"].as_str()).unwrap_or_default();
                    if id.is_empty() {
                        continue;
                    }
                    let title = state["input"]["description"].as_str().unwrap_or_default();
                    self.add(id, "agent", title, state["time"]["created"].as_i64().unwrap_or(at));
                }
                "shell" | "bash" => {
                    // Обычная команда возвращается только после конца, поэтому
                    // фоновая — та, что запущена с `background: true`.
                    if state["input"]["background"].as_bool() != Some(true) {
                        continue;
                    }
                    let id = state["metadata"]["shellID"].as_str().unwrap_or_default();
                    if id.is_empty() {
                        continue;
                    }
                    let title = state["input"]["command"].as_str().unwrap_or_default();
                    self.add(id, "bash", title, m["time"]["created"].as_i64().unwrap_or(0));
                }
                _ => {}
            }
        }
    }

    /// Уведомление о конце: `metadata.source` говорит, чья это задача.
    fn finished(&mut self, m: &Value) {
        let meta = &m["metadata"];
        let id = match meta["source"].as_str() {
            Some("shell") => meta["shellID"].as_str().or(meta["jobID"].as_str()),
            Some("subagent") => meta["childID"].as_str(),
            _ => None,
        };
        if let Some(id) = id {
            self.finish(id);
        }
    }

    fn add(&mut self, id: &str, kind: &str, title: &str, started_at: i64) {
        self.finish(id);
        if self.running.len() >= MAX_TASKS {
            self.running.remove(0);
        }
        self.running.push(BgTask {
            id: id.to_owned(),
            kind: kind.into(),
            title: truncate(title, TITLE_LIMIT),
            started_at,
            ends_at: 0,
        });
    }

    fn finish(&mut self, id: &str) {
        self.running.retain(|t| t.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(t: &mut Tracker, text: &str) {
        t.message(&serde_json::from_str::<Value>(text).unwrap());
    }

    fn event(t: &mut Tracker, text: &str) {
        t.event(&serde_json::from_str::<Value>(text).unwrap());
    }

    // Форматы сняты с opencode 2.2.7: команда запускается в фоне и сразу
    // возвращает управление, а о конце приходит отдельное сообщение `synthetic`.
    const SHELL_CALL: &str = r#"{"type":"assistant","time":{"created":1794954526554},"content":[{"type":"tool","id":"call_1","name":"shell","state":{"status":"completed","input":{"command":"Start-Sleep -Seconds 45","background":true},"metadata":{"status":"running","shellID":"sh_1"}},"time":{"created":1794954528641}}]}"#;
    const SHELL_DONE: &str = r#"{"type":"synthetic","metadata":{"source":"shell","shellID":"sh_1","jobID":"sh_1","state":"completed"},"time":{"created":1794954574128},"text":"<shell id=\"sh_1\" state=\"completed\">done</shell>","description":"Start-Sleep -Seconds 45"}"#;
    const FOREGROUND_CALL: &str = r#"{"type":"assistant","time":{"created":1},"content":[{"type":"tool","id":"call_2","name":"shell","state":{"status":"completed","input":{"command":"git status"},"metadata":{"status":"completed","exit":0}}}]}"#;

    #[test]
    fn background_shell_runs_until_notice() {
        let mut t = Tracker::default();
        message(&mut t, SHELL_CALL);
        assert_eq!(t.running().len(), 1);
        assert_eq!(t.running()[0].kind, "bash");
        assert_eq!(t.running()[0].title, "Start-Sleep -Seconds 45");
        message(&mut t, SHELL_DONE);
        assert!(t.running().is_empty());
    }

    #[test]
    fn foreground_shell_is_not_a_task() {
        let mut t = Tracker::default();
        message(&mut t, FOREGROUND_CALL);
        assert!(t.running().is_empty());
    }

    // Подагент: вызов вернул управление, а `metadata.status` остаётся `running`,
    // пока ребёнок не закончит; о конце говорит `metadata.source = "subagent"`.
    const SUBAGENT_CALL: &str = r#"{"type":"assistant","time":{"created":100},"content":[{"type":"tool","id":"call_3","name":"subagent","state":{"status":"completed","input":{"agent":"explore","background":true,"description":"Поиск по коду"},"metadata":{"sessionID":"ses_child","status":"running"},"time":{"created":150,"completed":900}}}]}"#;
    const SUBAGENT_DONE: &str = r#"{"type":"synthetic","metadata":{"source":"subagent","childID":"ses_child","agent":"Explore","state":"completed"},"time":{"created":2000},"text":"<subagent sessionID=\"ses_child\" state=\"completed\">итог</subagent>"}"#;

    #[test]
    fn subagent_runs_until_notice() {
        let mut t = Tracker::default();
        message(&mut t, SUBAGENT_CALL);
        assert_eq!(t.running().len(), 1);
        assert_eq!(t.running()[0].id, "ses_child");
        assert_eq!(t.running()[0].kind, "agent");
        assert_eq!(t.running()[0].started_at, 150);
        message(&mut t, SUBAGENT_DONE);
        assert!(t.running().is_empty());
    }

    #[test]
    fn repeated_message_does_not_duplicate_task() {
        let mut t = Tracker::default();
        for _ in 0..3 {
            message(&mut t, SHELL_CALL);
        }
        assert_eq!(t.running().len(), 1);
    }

    #[test]
    fn shell_exit_event_finishes_task() {
        let mut t = Tracker::default();
        message(&mut t, SHELL_CALL);
        event(&mut t, r#"{"type":"shell.exited","data":{"id":"sh_1","exit":0,"status":"exited"}}"#);
        assert!(t.running().is_empty());
    }

    #[test]
    fn foreign_shell_exit_is_ignored() {
        let mut t = Tracker::default();
        message(&mut t, SHELL_CALL);
        event(&mut t, r#"{"type":"shell.deleted","data":{"id":"sh_other"}}"#);
        assert_eq!(t.running().len(), 1);
    }

    #[test]
    fn subagent_without_metadata_is_skipped() {
        let mut t = Tracker::default();
        message(
            &mut t,
            r#"{"type":"assistant","time":{"created":1},"content":[{"type":"tool","id":"call_4","name":"subagent","state":{"status":"completed","input":{"agent":"x","description":"d"}}}]}"#,
        );
        assert!(t.running().is_empty());
    }
}
