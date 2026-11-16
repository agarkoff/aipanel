//! Вопрос Claude Code с вариантами ответа (инструмент `AskUserQuestion`).
//!
//! Пока вопрос ждёт ответа, в транскрипте лежит вызов без результата, а окно
//! Claude Code показывает меню. Панель получает вопрос формой (как запросы
//! opencode), а ответ уходит в окно нажатиями клавиш. Порядок клавиш снят
//! с Claude Code 2.1.325:
//!
//! - один вариант: цифра пункта выбирает его и открывает следующий вопрос;
//! - свой ответ: цифра пункта «Type something», текст, Enter;
//! - несколько вариантов: цифры переключают флажки, курсор остаётся на первом
//!   пункте; текст на строке своего варианта сам ставит его флажок; ↓ до строки
//!   «Submit» под вариантами и Enter;
//! - в конце экран проверки ответов, где «1» — «Submit answers». Его нет, только
//!   если вопрос один и в нём выбирается один вариант: ответ уходит сразу.

use aipanel_proto::{Prompt, PromptField, PromptOption};
use serde_json::Value;

use crate::platform::Key;

/// Ключ поля формы для вопроса `i`.
fn field_key(i: usize) -> String {
    format!("q{i}")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    pub text: String,
    pub header: String,
    pub multi: bool,
    /// Варианты: подпись и пояснение.
    pub options: Vec<(String, String)>,
}

/// Вопрос, который ждёт ответа: идентификатор вызова и его вопросы.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub id: String,
    pub questions: Vec<Question>,
}

/// Вызов `AskUserQuestion` из блока `tool_use`.
fn parse_call(block: &Value) -> Option<Ask> {
    if block["type"].as_str() != Some("tool_use") || block["name"].as_str() != Some("AskUserQuestion") {
        return None;
    }
    let id = block["id"].as_str().filter(|i| !i.is_empty())?;
    let questions: Vec<Question> = block["input"]["questions"]
        .as_array()?
        .iter()
        .map(|q| Question {
            text: q["question"].as_str().unwrap_or_default().to_owned(),
            header: q["header"].as_str().unwrap_or_default().to_owned(),
            multi: q["multiSelect"].as_bool().unwrap_or(false),
            options: q["options"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|o| {
                    (
                        o["label"].as_str().unwrap_or_default().to_owned(),
                        o["description"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect(),
        })
        .collect();
    // Цифрой выбираются пункты 1–9: варианты и «свой ответ» после них.
    if questions.is_empty() || questions.iter().any(|q| q.options.is_empty() || q.options.len() > 8) {
        return None;
    }
    Some(Ask { id: id.to_owned(), questions })
}

/// Следит по строкам транскрипта, ждёт ли сессия ответа на вопрос.
#[derive(Default)]
pub struct Tracker {
    pub pending: Option<Ask>,
}

impl Tracker {
    pub fn line(&mut self, line: &str) {
        let call = line.contains("\"AskUserQuestion\"");
        // Строки разбираем, только если они могут что-то поменять: в транскрипте
        // много длинных строк, и большинство к вопросу не относится.
        let user = self.pending.is_some() && line.contains("\"type\":\"user\"");
        if !call && !user {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        if v["isSidechain"].as_bool() == Some(true) {
            return;
        }
        let content = &v["message"]["content"];
        let blocks = content.as_array().map(Vec::as_slice).unwrap_or_default();
        if let Some(id) = self.pending.as_ref().map(|a| a.id.clone()) {
            let answered = blocks
                .iter()
                .any(|b| b["type"].as_str() == Some("tool_result") && b["tool_use_id"].as_str() == Some(&id));
            // Новый запрос человека: вопрос уже не ждёт ответа, как бы он ни закончился.
            let prompt = v["type"].as_str() == Some("user")
                && v["isMeta"].as_bool() != Some(true)
                && (content.as_str().is_some_and(|s| !s.trim().is_empty())
                    || blocks.iter().any(|b| b["type"].as_str() == Some("text")));
            if answered || prompt {
                self.pending = None;
            }
        }
        if call && v["type"].as_str() == Some("assistant") {
            if let Some(ask) = blocks.iter().find_map(parse_call) {
                self.pending = Some(ask);
            }
        }
    }
}

/// Форма для панели. Значение варианта — его подпись: свой ответ, совпавший
/// с подписью, — тот же выбор.
pub fn prompt(ask: &Ask) -> Prompt {
    let fields = ask
        .questions
        .iter()
        .enumerate()
        .map(|(i, q)| PromptField {
            key: field_key(i),
            kind: if q.multi { "multiselect" } else { "string" }.into(),
            title: if q.text.is_empty() { q.header.clone() } else { q.text.clone() },
            description: String::new(),
            required: true,
            hidden: false,
            custom: true,
            options: q
                .options
                .iter()
                .map(|(label, description)| PromptOption {
                    value: label.clone(),
                    label: label.clone(),
                    description: description.clone(),
                })
                .collect(),
            default: None,
            min: None,
            max: None,
            min_items: None,
            max_items: None,
            min_length: None,
            max_length: None,
            pattern: String::new(),
            placeholder: String::new(),
            format: String::new(),
            url: String::new(),
            when: Vec::new(),
        })
        .collect();
    let title = match ask.questions.as_slice() {
        [q] if !q.header.is_empty() => format!("Claude спрашивает: {}", q.header),
        _ => "Claude спрашивает".into(),
    };
    Prompt { id: ask.id.clone(), kind: "form".into(), title, text: String::new(), fields, save: Vec::new() }
}

/// Свой ответ в одну строку: поле в окне однострочное.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn digit(i: usize) -> Key {
    Key::Text((i + 1).to_string())
}

/// Нажатия, которые вводят ответ `answer` (`{"q0": "подпись", "q1": ["…"]}`) в меню.
pub fn keys(ask: &Ask, answer: &Value) -> Result<Vec<Key>, String> {
    let mut out = Vec::new();
    for (i, q) in ask.questions.iter().enumerate() {
        let name = if q.header.is_empty() { format!("вопрос {}", i + 1) } else { format!("«{}»", q.header) };
        let n = q.options.len();
        let index = |v: &str| q.options.iter().position(|(label, _)| label == v);
        let value = &answer[field_key(i)];
        if q.multi {
            let items: Vec<String> = match value {
                Value::Array(a) => a.iter().filter_map(Value::as_str).map(one_line).filter(|s| !s.is_empty()).collect(),
                _ => Vec::new(),
            };
            let mut chosen: Vec<usize> = items.iter().filter_map(|v| index(v)).collect();
            chosen.sort_unstable();
            chosen.dedup();
            let custom: Vec<&str> = items.iter().filter(|v| index(v).is_none()).map(String::as_str).collect();
            if chosen.is_empty() && custom.is_empty() {
                return Err(format!("нет ответа на {name}"));
            }
            out.extend(chosen.into_iter().map(digit));
            if custom.is_empty() {
                out.extend(std::iter::repeat_n(Key::Down, n + 1));
            } else {
                out.extend(std::iter::repeat_n(Key::Down, n));
                out.push(Key::Text(custom.join(", ")));
                out.push(Key::Wait(200));
                out.push(Key::Down);
            }
            out.push(Key::Enter);
        } else {
            let v = one_line(value.as_str().unwrap_or_default());
            if v.is_empty() {
                return Err(format!("нет ответа на {name}"));
            }
            match index(&v) {
                Some(k) => out.push(digit(k)),
                None => {
                    out.push(digit(n));
                    out.push(Key::Wait(300));
                    out.push(Key::Text(v));
                    out.push(Key::Wait(200));
                    out.push(Key::Enter);
                }
            }
        }
        // Следующий вопрос или экран проверки открываются не мгновенно.
        out.push(Key::Wait(400));
    }
    let instant = matches!(ask.questions.as_slice(), [q] if !q.multi);
    if !instant {
        out.push(digit(0));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call_line(id: &str, questions: Value) -> String {
        json!({"type":"assistant","message":{"role":"assistant","content":[
            {"type":"tool_use","id":id,"name":"AskUserQuestion","input":{"questions":questions}}]}})
        .to_string()
    }

    fn result_line(id: &str) -> String {
        json!({"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":id,"content":"answered"}]}})
        .to_string()
    }

    fn two() -> Value {
        json!([
            {"question":"Цвет?","header":"Цвет","multiSelect":false,
             "options":[{"label":"Красный","description":"тёплый"},{"label":"Синий","description":"холодный"}]},
            {"question":"Фрукты?","header":"Фрукты","multiSelect":true,
             "options":[{"label":"Яблоко"},{"label":"Груша"},{"label":"Слива"}]}
        ])
    }

    #[test]
    fn tracks_pending_question() {
        let mut t = Tracker::default();
        t.line(&call_line("toolu_1", two()));
        let ask = t.pending.clone().unwrap();
        assert_eq!(ask.id, "toolu_1");
        assert_eq!(ask.questions.len(), 2);
        assert!(ask.questions[1].multi);
        assert_eq!(ask.questions[0].options[0], ("Красный".into(), "тёплый".into()));
        // Чужой результат вопрос не закрывает, свой — закрывает.
        t.line(&result_line("toolu_other"));
        assert!(t.pending.is_some());
        t.line(&result_line("toolu_1"));
        assert!(t.pending.is_none());
    }

    #[test]
    fn new_prompt_clears_question() {
        let mut t = Tracker::default();
        t.line(&call_line("toolu_1", two()));
        t.line(r#"{"type":"user","message":{"role":"user","content":"другое"}}"#);
        assert!(t.pending.is_none());
    }

    #[test]
    fn ignores_subagent_questions() {
        let mut t = Tracker::default();
        let mut v: Value = serde_json::from_str(&call_line("toolu_1", two())).unwrap();
        v["isSidechain"] = json!(true);
        t.line(&v.to_string());
        assert!(t.pending.is_none());
    }

    #[test]
    fn builds_form() {
        let mut t = Tracker::default();
        t.line(&call_line("toolu_1", two()));
        let p = prompt(t.pending.as_ref().unwrap());
        assert_eq!(p.id, "toolu_1");
        assert_eq!(p.kind, "form");
        assert_eq!(p.fields[0].key, "q0");
        assert_eq!(p.fields[0].kind, "string");
        assert_eq!(p.fields[1].kind, "multiselect");
        assert!(p.fields.iter().all(|f| f.required && f.custom));
        assert_eq!(p.fields[0].options[1].value, "Синий");
    }

    fn texts(keys: &[Key]) -> Vec<String> {
        keys.iter()
            .filter(|k| !matches!(k, Key::Wait(_)))
            .map(|k| match k {
                Key::Text(t) => t.clone(),
                Key::Down => "↓".into(),
                Key::Enter => "⏎".into(),
                Key::Esc => "esc".into(),
                Key::Wait(_) => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn keys_for_options_and_review() {
        let mut t = Tracker::default();
        t.line(&call_line("toolu_1", two()));
        let ask = t.pending.unwrap();
        let k = keys(&ask, &json!({"q0":"Синий","q1":["Слива","Яблоко"]})).unwrap();
        assert_eq!(texts(&k), ["2", "1", "3", "↓", "↓", "↓", "↓", "⏎", "1"]);
    }

    #[test]
    fn keys_for_custom_answers() {
        let mut t = Tracker::default();
        t.line(&call_line("toolu_1", two()));
        let ask = t.pending.unwrap();
        let k = keys(&ask, &json!({"q0":"Зелёный\nсветлый","q1":["Груша","Киви"]})).unwrap();
        assert_eq!(texts(&k), ["3", "Зелёный светлый", "⏎", "2", "↓", "↓", "↓", "Киви", "↓", "⏎", "1"]);
    }

    #[test]
    fn single_choice_question_submits_at_once() {
        let mut t = Tracker::default();
        t.line(&call_line("toolu_1", json!([{"question":"Чай?","header":"Чай","options":[{"label":"Чёрный"},{"label":"Зелёный"}]}])));
        let ask = t.pending.unwrap();
        assert_eq!(texts(&keys(&ask, &json!({"q0":"Зелёный"})).unwrap()), ["2"]);
        assert!(keys(&ask, &json!({})).is_err());
        assert_eq!(prompt(&ask).title, "Claude спрашивает: Чай");
    }
}
