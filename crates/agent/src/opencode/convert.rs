//! Превращение ответов HTTP API opencode в то, что понимает панель: элементы
//! ленты, запросы с вариантами, модели, токены хода. Здесь нет ни запросов,
//! ни состояния сессий — только разбор JSON.

use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    hash::{Hash, Hasher},
};

use aipanel_proto::{Item, ModelInfo, Prompt, PromptField, PromptOption, PromptWhen, TurnUsage};
use serde_json::Value;

use crate::util::{create_diff, edit_diff, is_edit_tool, tool_path, tool_text, truncate, DIFF_LIMIT};

use super::{PAGE, PROMPT_FIELDS, PROMPT_OPTIONS, RESULT_LIMIT, TEXT_LIMIT, TURN_PAGE};

/// Что удалось прочитать о ходе сессии.
pub(super) struct TurnRead {
    pub usage: Option<TurnUsage>,
    /// Страница прочитана целиком, а конца прошлого хода в ней нет: начало
    /// текущего хода осталось выше. По этому признаку видно, насколько предел
    /// `TURN_PAGE` мешает на самом деле.
    pub beyond_page: bool,
}

/// Токены текущего хода из сообщений сессии, новые первыми. Ход идёт от первого
/// запроса пользователя после последней отметки `idle` (конца прошлого хода), так
/// что сообщение, отправленное посреди хода, его не перезапускает. Токены есть только
/// у завершённых шагов ассистента: недописанный шаг в сумму не входит.
/// `usage` пуст, если запроса не видно или ход длиннее прочитанной страницы.
pub(super) fn turn_from_messages(msgs: &[Value]) -> TurnRead {
    let mut started: Option<i64> = None;
    let mut input: Option<u64> = None;
    let mut output = 0u64;
    let mut ended = false;
    for m in msgs {
        match m["type"].as_str() {
            Some("idle") => {
                ended = true;
                break;
            }
            // Идём от новых к старым, поэтому остаётся самый ранний запрос.
            Some("user") => started = m["time"]["created"].as_i64().or(started),
            Some("assistant") if m["tokens"].is_object() => {
                let t = &m["tokens"];
                let count = |v: &Value| v.as_u64().unwrap_or(0);
                // Контекст последнего шага: новые входные токены вместе с кешем.
                input.get_or_insert_with(|| {
                    count(&t["input"]) + count(&t["cache"]["read"]) + count(&t["cache"]["write"])
                });
                output += count(&t["output"]) + count(&t["reasoning"]);
            }
            _ => {}
        }
    }
    if !ended && msgs.len() >= TURN_PAGE {
        return TurnRead { usage: None, beyond_page: true };
    }
    let usage = started.map(|started_at| TurnUsage { started_at, input: input.unwrap_or(0), output });
    TurnRead { usage, beyond_page: false }
}

/// Контекст последнего ответа: входные токены вместе с кешем ближайшего ответа
/// ассистента. Сообщения приходят от новых к старым, поэтому ищется только в
/// начале: суммарный выход подагента берётся из счётчиков его сессии, а не
/// отсюда, так что глубже страницы ничего не нужно.
pub(super) fn last_context(msgs: &[Value]) -> Option<u64> {
    for m in msgs {
        if m["type"].as_str() != Some("assistant") {
            continue;
        }
        let t = &m["tokens"];
        if !t.is_object() {
            continue;
        }
        let count = |v: &Value| v.as_u64().unwrap_or(0);
        return Some(count(&t["input"]) + count(&t["cache"]["read"]) + count(&t["cache"]["write"]));
    }
    None
}

pub(super) fn text_hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Хэш набора запросов: при изменении списка браузеру уходит новый.
pub(super) fn prompts_hash(prompts: &[Prompt]) -> u64 {
    let mut h = DefaultHasher::new();
    serde_json::to_string(prompts).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// Идентификаторы сессий из ответа opencode со списком запросов (`data` — массив
/// запросов, у каждого свой `sessionID`).
pub(super) fn prompt_session_ids(v: &Value) -> impl Iterator<Item = &str> {
    v["data"].as_array().into_iter().flatten().filter_map(|r| r["sessionID"].as_str())
}

pub(super) fn prompt_option(v: &Value) -> Option<PromptOption> {
    let value = v["value"].as_str()?;
    Some(PromptOption {
        value: value.to_owned(),
        label: v["label"].as_str().filter(|l| !l.is_empty()).unwrap_or(value).to_owned(),
        description: v["description"].as_str().unwrap_or_default().to_owned(),
    })
}

pub(super) fn prompt_field(f: &Value) -> Option<PromptField> {
    let key = f["key"].as_str().filter(|k| !k.is_empty())?;
    let kind = f["type"].as_str()?;
    if !matches!(kind, "string" | "number" | "integer" | "boolean" | "multiselect" | "external") {
        return None;
    }
    let options: Vec<PromptOption> = f["options"]
        .as_array()
        .into_iter()
        .flatten()
        .take(PROMPT_OPTIONS)
        .filter_map(prompt_option)
        .collect();
    let mut when = Vec::new();
    for w in f["when"].as_array().into_iter().flatten() {
        let (Some(key), Some(op)) = (w["key"].as_str(), w["op"].as_str()) else {
            continue;
        };
        if !matches!(op, "eq" | "neq") {
            continue;
        }
        if let Some(value) = w.get("value") {
            when.push(PromptWhen { key: key.to_owned(), op: op.to_owned(), value: value.clone() });
        }
    }
    Some(PromptField {
        key: key.to_owned(),
        kind: kind.to_owned(),
        title: truncate(f["title"].as_str().unwrap_or_default(), 1000),
        description: truncate(f["description"].as_str().unwrap_or_default(), 2000),
        required: f["required"].as_bool().unwrap_or(false),
        hidden: f["hidden"].as_bool().unwrap_or(false),
        custom: f["custom"].as_bool().unwrap_or(false),
        options,
        default: f.get("default").cloned(),
        min: f["minimum"].as_f64(),
        max: f["maximum"].as_f64(),
        min_items: f["minItems"].as_u64(),
        max_items: f["maxItems"].as_u64(),
        min_length: f["minLength"].as_u64(),
        max_length: f["maxLength"].as_u64(),
        pattern: f["pattern"].as_str().unwrap_or_default().to_owned(),
        placeholder: truncate(f["placeholder"].as_str().unwrap_or_default(), 300),
        format: f["format"].as_str().unwrap_or_default().to_owned(),
        url: f["url"].as_str().unwrap_or_default().to_owned(),
        when,
    })
}

/// Описание разрешения для панели: действие и ресурсы, на которые оно выдаётся.
pub(super) fn permission_prompt(p: &Value) -> Option<Prompt> {
    let id = p["id"].as_str().filter(|i| !i.is_empty())?;
    let action = p["action"].as_str().unwrap_or_default();
    let label = match action {
        "read" => "Прочитать файл".to_owned(),
        "edit" => "Изменить файл".to_owned(),
        "list" => "Показать каталог".to_owned(),
        "bash" | "shell" => "Выполнить команду".to_owned(),
        "external_directory" => "Открыть каталог вне проекта".to_owned(),
        "question" => "Задать вопрос".to_owned(),
        "browser" => "Открыть браузер".to_owned(),
        "subagent" => "Запустить подагента".to_owned(),
        "webfetch" | "fetch" => "Загрузить страницу".to_owned(),
        "websearch" => "Поиск в интернете".to_owned(),
        "lsp" => "Запрос к языковому серверу".to_owned(),
        "" => "Требуется разрешение".to_owned(),
        other => format!("Выполнить «{other}»"),
    };
    let resources: Vec<String> = p["resources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str())
        .map(|r| truncate(r, 1000))
        .collect();
    let text = match resources.len() {
        0 => label,
        1 => format!("{label}: {}", resources[0]),
        _ => format!(
            "{label}:\n{}",
            resources.iter().map(|r| format!("• {r}")).collect::<Vec<_>>().join("\n")
        ),
    };
    let text = match p["message"].as_str().filter(|m| !m.trim().is_empty()) {
        Some(message) => format!("{text}\n{}", truncate(message, 1000)),
        None => text,
    };
    let save: Vec<String> = p["save"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str())
        .map(|s| truncate(s, 500))
        .collect();
    Some(Prompt {
        id: id.to_owned(),
        kind: "permission".into(),
        title: "opencode просит разрешение".into(),
        text,
        fields: Vec::new(),
        save,
    })
}

/// Форма opencode: вопрос с вариантами ответа или полями ввода.
pub(super) fn form_prompt(f: &Value) -> Option<Prompt> {
    let id = f["id"].as_str().filter(|i| !i.is_empty())?;
    let fields: Vec<PromptField> = f["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .take(PROMPT_FIELDS)
        .filter_map(prompt_field)
        .collect();
    if fields.is_empty() {
        return None;
    }
    Some(Prompt {
        id: id.to_owned(),
        kind: "form".into(),
        title: truncate(f["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("Вопрос opencode"), 1000),
        text: String::new(),
        fields,
        save: Vec::new(),
    })
}

/// Элемент очереди из ответа `POST /prompt`: его показываем сразу, не дожидаясь,
/// пока opencode отдаст сообщение в ленту.
pub(super) fn queued_from_prompt(v: &Value, text: &str) -> Option<Item> {
    let d = &v["data"];
    let id = d["id"].as_str()?;
    Some(Item {
        id: format!("queue:{id}"),
        ts: d["time"]["created"].as_i64().unwrap_or_else(crate::util::now_ms),
        role: "user".into(),
        kind: "queued".into(),
        text: truncate(d["payload"]["text"].as_str().unwrap_or(text), TEXT_LIMIT),
        tool: None,
        sub: None,
    })
}

/// Сообщения из очереди сессии, которых ещё нет в ленте.
pub(super) fn queued_items(v: &Value, delivered: &HashSet<u64>) -> Vec<Item> {
    let mut out = Vec::new();
    for q in v["data"].as_array().into_iter().flatten() {
        let (Some(qid), Some(text)) = (q["id"].as_str(), q["payload"]["text"].as_str()) else {
            continue;
        };
        if q["type"].as_str() != Some("user")
            || text.trim().is_empty()
            || delivered.contains(&text_hash(text))
        {
            continue;
        }
        out.push(Item {
            id: format!("queue:{qid}"),
            ts: q["time"]["created"].as_i64().unwrap_or(0),
            role: "user".into(),
            kind: "queued".into(),
            text: truncate(text, TEXT_LIMIT),
            tool: None,
            sub: None,
        });
    }
    out
}

/// Дифф по входу инструмента, когда готового патча нет (например, write).
pub(super) fn input_diff(name: Option<&str>, input: &Value) -> Option<String> {
    let name = name.filter(|n| is_edit_tool(n))?;
    let path = tool_path(input).filter(|p| !p.is_empty())?;
    if name.eq_ignore_ascii_case("write") {
        return input["content"].as_str().map(|content| create_diff(path, content));
    }
    let old = input["oldString"].as_str().or_else(|| input["old_string"].as_str())?;
    let new = input["newString"].as_str().or_else(|| input["new_string"].as_str())?;
    Some(edit_diff(path, old, new))
}

/// Текст из synthetic-сообщения opencode `<subagent …>итог</subagent>`.
pub(super) fn subagent_inner(text: &str) -> String {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("<subagent") {
        return text.trim().to_owned();
    }
    let Some(start) = trimmed.find('>') else { return text.trim().to_owned() };
    let rest = &trimmed[start + 1..];
    let end = rest.rfind("</subagent>").unwrap_or(rest.len());
    rest[..end].trim().to_owned()
}

pub(super) fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Страница ранних сообщений из ответа opencode: элементы в порядке ленты (от
/// старых к новым) и есть ли за ней ещё страница.
///
/// Курсор opencode отдаёт и на последней странице, указывая за конец, поэтому о
/// конце говорит неполная страница: полная — за ней может быть ещё одна.
pub(super) fn older_page(v: &Value) -> (Vec<Item>, bool) {
    let data = v["data"].as_array().map_or(&[][..], Vec::as_slice);
    let mut items = Vec::new();
    for m in data.iter().rev() {
        convert(m, &mut items);
    }
    (items, data.len() >= PAGE)
}

pub(super) fn convert(m: &Value, out: &mut Vec<Item>) {
    let id = m["id"].as_str().unwrap_or_default();
    let ts = m["time"]["created"].as_i64().unwrap_or(0);
    // Конец шага ассистента: если у инструмента нет своих отметок, это
    // ближайшая оценка момента, когда команда закончилась. 0 — шаг ещё идёт.
    let done = m["time"]["completed"]
        .as_i64()
        .or_else(|| m["time"]["end"].as_i64())
        .unwrap_or(0);
    let mut push = |suffix: String,
                    ts: i64,
                    role: &str,
                    kind: &str,
                    text: String,
                    tool: Option<String>,
                    sub: Option<String>| {
        if !text.trim().is_empty() || kind == "tool_use" || kind == "subagent" {
            out.push(Item {
                id: format!("{id}:{suffix}"),
                ts,
                role: role.into(),
                kind: kind.into(),
                text,
                tool,
                sub,
            });
        }
    };
    match m["type"].as_str() {
        Some("user") => push(
            "0".into(),
            ts,
            "user",
            "text",
            truncate(m["text"].as_str().unwrap_or_default(), TEXT_LIMIT),
            None,
            None,
        ),
        Some("assistant") => {
            // id частей совпадает с id потоковых событий opencode (`text:0`,
            // `reasoning:0`), поэтому пришедший по потоку текст заменяется
            // итоговым элементом, а не дублируется.
            let mut texts = 0usize;
            let mut thinks = 0usize;
            for (i, c) in m["content"].as_array().into_iter().flatten().enumerate() {
                match c["type"].as_str() {
                    Some("text") => {
                        push(
                            format!("text:{texts}"),
                            ts,
                            "assistant",
                            "text",
                            truncate(c["text"].as_str().unwrap_or_default(), TEXT_LIMIT),
                            None,
                            None,
                        );
                        texts += 1;
                    }
                    Some("reasoning") => {
                        push(
                            format!("reasoning:{thinks}"),
                            ts,
                            "assistant",
                            "thinking",
                            truncate(c["text"].as_str().unwrap_or_default(), TEXT_LIMIT),
                            None,
                            None,
                        );
                        thinks += 1;
                    }
                    Some("tool") => {
                        let name = c["name"].as_str();
                        let state = &c["state"];
                        // Свои отметки команды: у вызова — создание части или
                        // начало состояния, у результата — конец. Панель считает
                        // по ним, сколько шла команда.
                        let started = state["time"]["start"]
                            .as_i64()
                            .or_else(|| state["time"]["created"].as_i64())
                            .or_else(|| c["time"]["created"].as_i64())
                            .unwrap_or(ts);
                        let finished = state["time"]["end"]
                            .as_i64()
                            .or_else(|| state["time"]["completed"].as_i64())
                            .unwrap_or(done);
                        if name.is_some_and(crate::util::is_subagent_tool) {
                            // Карточка подагента на месте вызова: его лента — в
                            // дочерней сессии opencode. У фонового запуска id
                            // известен сразу, у синхронного его в вызове нет —
                            // панель найдёт подагента по подписи и времени.
                            let input = &state["input"];
                            let child = state["metadata"]["sessionID"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned);
                            let title = input["description"]
                                .as_str()
                                .filter(|d| !d.trim().is_empty())
                                .unwrap_or("Подагент");
                            let agent = input["agent"]
                                .as_str()
                                .or_else(|| input["subagent_type"].as_str())
                                .unwrap_or_default();
                            push(
                                i.to_string(),
                                started,
                                "assistant",
                                "subagent",
                                truncate(title, 400),
                                (!agent.is_empty()).then(|| agent.to_owned()),
                                child.clone(),
                            );
                            // У синхронного вызова итог лежит в самом вызове; у
                            // фонового его пришлёт synthetic-сообщение, а в
                            // вызове может быть только отметка о запуске.
                            if input["background"].as_bool() != Some(true) {
                                let mut result = content_text(&state["content"]);
                                if result.is_empty() {
                                    result = content_text(&state["error"]);
                                }
                                if !result.trim().is_empty() {
                                    push(
                                        format!("{i}:r"),
                                        finished,
                                        "tool",
                                        "subagent_result",
                                        truncate(&result, RESULT_LIMIT),
                                        None,
                                        child,
                                    );
                                }
                            }
                            continue;
                        }
                        push(
                            i.to_string(),
                            started,
                            "assistant",
                            "tool_use",
                            tool_text(name, &state["input"]),
                            name.map(str::to_owned),
                            None,
                        );
                        let mut result = content_text(&state["content"]);
                        if result.is_empty() {
                            result = content_text(&state["error"]);
                        }
                        push(
                            format!("{i}:r"),
                            finished,
                            "tool",
                            "tool_result",
                            truncate(&result, RESULT_LIMIT),
                            None,
                            None,
                        );

                        // opencode отдаёт готовые патчи изменённых файлов.
                        let mut shown = false;
                        for (j, file) in state["metadata"]["files"].as_array().into_iter().flatten().enumerate() {
                            let Some(patch) = file["patch"].as_str().filter(|p| !p.trim().is_empty()) else {
                                continue;
                            };
                            shown = true;
                            push(
                                format!("{i}:diff{j}"),
                                finished,
                                "assistant",
                                "diff",
                                truncate(patch, DIFF_LIMIT),
                                None,
                                None,
                            );
                        }
                        if !shown {
                            if let Some(text) = input_diff(name, &state["input"]) {
                                push(
                                    format!("{i}:diff"),
                                    finished,
                                    "assistant",
                                    "diff",
                                    truncate(&text, DIFF_LIMIT),
                                    None,
                                    None,
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !m["error"].is_null() {
                let text = m["error"]["message"].as_str().map(str::to_owned).unwrap_or_else(|| m["error"].to_string());
                push("err".into(), ts, "system", "error", truncate(&text, RESULT_LIMIT), None, None);
            }
        }
        Some("shell") => {
            push(
                "0".into(),
                ts,
                "assistant",
                "tool_use",
                m["command"].as_str().unwrap_or_default().to_owned(),
                Some("shell".into()),
                None,
            );
            push(
                "0:r".into(),
                done,
                "tool",
                "tool_result",
                truncate(&content_text(&m["output"]), RESULT_LIMIT),
                None,
                None,
            );
        }
        // Сессия переехала в другой каталог (обычно — в созданный ею worktree):
        // opencode меняет её `location.directory`, и без отметки в ленте панель
        // просто начинает показывать другой путь.
        Some("location-switched") => {
            let to = m["location"]["directory"].as_str().unwrap_or_default();
            push("0".into(), ts, "system", "cwd", to.to_owned(), None, None);
        }
        // Итог фонового подагента: opencode присылает его synthetic-сообщением,
        // внутри `<subagent …>…</subagent>`. В ленте это «Итог» под карточкой.
        Some("synthetic") => {
            if m["metadata"]["source"].as_str() == Some("subagent") {
                let child = m["metadata"]["childID"].as_str().filter(|s| !s.is_empty());
                let raw = m["text"].as_str().unwrap_or_default();
                let text = subagent_inner(raw);
                if !text.trim().is_empty() {
                    let suffix = match child {
                        Some(child) => format!("sub:{child}"),
                        None => format!("sub:{ts}"),
                    };
                    push(
                        suffix,
                        ts,
                        "tool",
                        "subagent_result",
                        truncate(&text, RESULT_LIMIT),
                        None,
                        child.map(str::to_owned),
                    );
                }
            }
        }
        // Сообщение типа idle повторяет ошибку предыдущего ответа, поэтому не показывается.
        _ => {}
    }
}

/// Модель сессии в виде `провайдер/модель`, как её пишут в конфиге opencode.
pub(super) fn session_model(m: &Value) -> String {
    let id = m["id"].as_str().or(m["modelID"].as_str()).unwrap_or_default();
    match m["providerID"].as_str() {
        Some(p) if !p.is_empty() && !id.is_empty() => format!("{p}/{id}"),
        _ => id.to_owned(),
    }
}

/// Список моделей из ответа `/api/model`: в панель уходят только провайдер,
/// id и название, без настроек и возможностей модели.
pub(super) fn models_from(v: &Value) -> Vec<ModelInfo> {
    let mut out = Vec::new();
    for m in v["data"].as_array().into_iter().flatten() {
        let Some(provider) = m["providerID"].as_str().filter(|s| !s.is_empty()) else { continue };
        let Some(id) = m["id"].as_str().or_else(|| m["modelID"].as_str()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let name = m["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(id);
        out.push(ModelInfo {
            provider: provider.to_owned(),
            id: id.to_owned(),
            name: name.chars().take(120).collect(),
        });
    }
    out.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// Разбирает `провайдер/модель` из панели. Пусто — модель не выбрана.
pub(super) fn split_model(model: &str) -> Result<Option<(&str, &str)>, String> {
    let model = model.trim();
    if model.is_empty() {
        return Ok(None);
    }
    let Some((provider, id)) = model.split_once('/') else {
        return Err("модель opencode указывается как провайдер/модель".into());
    };
    let ok = !provider.is_empty()
        && !id.is_empty()
        && model.len() <= 200
        && model
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '@' | '~'));
    if !ok {
        return Err(format!("некорректная модель: {model}"));
    }
    Ok(Some((provider, id)))
}
