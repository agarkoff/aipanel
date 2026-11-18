//! Подагенты сессии opencode: дочерние сессии, которые запускает инструмент
//! `subagent` (в старых версиях `task`).
//!
//! Список берётся не из вызова инструмента, а из самих сессий opencode: у
//! дочерней сессии есть `parentID`, а её `title` — описание задачи из вызова,
//! `agent` — тип подагента, `time.created` — время запуска. Так видны и
//! синхронные вызовы (когда `subagent` ждёт ребёнка), у которых в самом вызове
//! нет ни id сессии, ни статуса: opencode заполняет `metadata.sessionID` только
//! у фонового запуска и только к его концу.
//!
//! Работает ли подагент, говорит `/api/session/active`: там перечислены
//! выполняющиеся сессии, включая дочерние.
//!
//! Из ленты сессии вызовы подагентов убираются (`convert`): панель показывает их
//! отдельным списком, а ленту выбранного подагента читает из дочерней сессии.

use std::collections::HashMap;

use aipanel_proto::{Subagent, TurnUsage, MAX_SUBAGENTS};
use serde_json::Value;

/// Подпись подагента длиннее этого обрезается.
const TITLE_LIMIT: usize = 200;

/// Подагенты сессии `parent` по ответам `/api/session` и `/api/session/active`.
pub fn list(sessions: &Value, active: &Value, parent: &str) -> Vec<Subagent> {
    let mut out = Vec::new();
    for s in sessions["data"].as_array().into_iter().flatten() {
        if s["parentID"].as_str() != Some(parent) {
            continue;
        }
        let Some(id) = s["id"].as_str().filter(|id| !id.is_empty()) else { continue };
        let started_at = s["time"]["created"].as_i64().unwrap_or(0);
        // Счётчики сессии кумулятивные (весь прогон), поэтому выход берётся
        // целиком, без чтения сообщений и предела в страницу.
        let tokens = &s["tokens"];
        out.push(Subagent {
            id: id.to_owned(),
            title: crate::util::truncate(s["title"].as_str().unwrap_or_default(), TITLE_LIMIT),
            agent: s["agent"].as_str().unwrap_or_default().to_owned(),
            running: active["data"].get(id).is_some(),
            started_at,
            ended_at: s["time"]["idle"].as_i64().unwrap_or(0),
            turn: tokens.is_object().then(|| TurnUsage {
                started_at,
                input: 0,
                output: tokens["output"].as_u64().unwrap_or(0) + tokens["reasoning"].as_u64().unwrap_or(0),
            }),
        });
    }
    out.sort_by_key(|s| s.started_at);
    if out.len() > MAX_SUBAGENTS {
        out.drain(..out.len() - MAX_SUBAGENTS);
    }
    out
}

/// Время последнего изменения каждой дочерней сессии: по нему видно, пора ли
/// перечитывать её токены. Ключ — id подагента.
pub fn updated(sessions: &Value, parent: &str) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    for s in sessions["data"].as_array().into_iter().flatten() {
        if s["parentID"].as_str() != Some(parent) {
            continue;
        }
        let Some(id) = s["id"].as_str().filter(|id| !id.is_empty()) else { continue };
        out.insert(id.to_owned(), s["time"]["updated"].as_i64().unwrap_or(0));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ответы сняты с opencode 2.2.7: синхронный `subagent` — дочерняя сессия с
    // `parentID`, в самом вызове ни id сессии, ни статуса нет.
    const SESSIONS: &str = r#"{"data":[
        {"id":"ses_child","parentID":"ses_parent","agent":"general","title":"Review branch","tokens":{"input":100,"output":40,"reasoning":10,"cache":{"read":500,"write":0}},"time":{"created":150,"updated":950,"idle":940}},
        {"id":"ses_other","parentID":"ses_other_parent","agent":"general","title":"Чужой","time":{"created":100}},
        {"id":"ses_parent","title":"Основная","time":{"created":1}},
        {"id":"ses_second","parentID":"ses_parent","title":"Второй","time":{"created":200}}
    ]}"#;

    fn sessions() -> Value {
        serde_json::from_str(SESSIONS).unwrap()
    }

    #[test]
    fn lists_children_of_the_session_in_launch_order() {
        let active: Value = serde_json::json!({"data":{"ses_child":{"type":"running"}}});
        let list = list(&sessions(), &active, "ses_parent");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "ses_child");
        assert_eq!(list[0].title, "Review branch");
        assert_eq!(list[0].agent, "general");
        assert_eq!(list[0].started_at, 150);
        // Время работы закрывается `idle` сессии, а выход берётся из её счётчиков.
        assert_eq!(list[0].ended_at, 940);
        let turn = list[0].turn.as_ref().unwrap();
        assert_eq!((turn.input, turn.output), (0, 50));
        assert!(list[0].running);
        // Завершённый ребёнок остаётся в списке, но не работает.
        assert_eq!(list[1].id, "ses_second");
        assert!(!list[1].running);
        // Без счётчиков сессии токенов нет: пустой ноль не выдаём.
        assert!(list[1].turn.is_none());
    }

    #[test]
    fn ignores_foreign_and_parent_sessions() {
        let active: Value = serde_json::json!({"data":{}});
        assert!(list(&sessions(), &active, "ses_none").is_empty());
        assert!(list(&sessions(), &active, "ses_other_parent").len() == 1);
    }

    #[test]
    fn missing_active_list_is_not_an_error() {
        let list = list(&sessions(), &Value::Null, "ses_parent");
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|s| !s.running));
    }

    #[test]
    fn update_stamps_belong_to_children_only() {
        let stamps = updated(&sessions(), "ses_parent");
        assert_eq!(stamps.get("ses_child"), Some(&950));
        // У второго ребёнка `updated` не записан: перечитывать его незачем.
        assert_eq!(stamps.get("ses_second"), Some(&0));
        assert!(!stamps.contains_key("ses_parent"));
        assert!(!stamps.contains_key("ses_other"));
    }
}
