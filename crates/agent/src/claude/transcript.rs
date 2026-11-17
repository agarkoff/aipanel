//! Разбор транскрипта Claude Code: что записано в `.jsonl` — сообщения,
//! вызовы инструментов, диффы, очередь и токены хода. Здесь только чтение
//! файла и превращение строк в элементы ленты; состояние сессий — в `mod.rs`.

use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::SystemTime,
};

use aipanel_proto::{Item, TurnUsage};
use serde_json::Value;

use crate::util::{create_diff, edit_diff, parse_iso_ms, to_ms, tool_text, truncate, DIFF_LIMIT};

use super::{HEAD_META_BYTES, St, TAIL_META_BYTES, RESULT_LIMIT, TEXT_LIMIT, TURN_CHUNK, TURN_MAX_BYTES};

#[derive(Clone, Default)]
pub(super) struct TailMeta {
    pub(super) title: Option<String>,
    pub(super) cwd: Option<String>,
    /// Время последней записи транскрипта (мс от эпохи), а не mtime файла:
    /// живой CLI «касается» файла раз в час, даже когда новых сообщений нет.
    pub(super) updated: i64,
    /// Модель последнего ответа ассистента.
    pub(super) model: Option<String>,
}

pub(super) struct TurnCache {
    /// Смещение строки с запросом пользователя, с которого начался ход.
    at: Option<u64>,
    mtime: SystemTime,
    usage: Option<TurnUsage>,
}

/// Название и каталог сессии из хвоста (и при необходимости начала) транскрипта.
pub(super) fn tail_meta(st: &mut St, path: &Path, mtime: SystemTime) -> TailMeta {
    if let Some((cached, meta)) = st.tails.get(path) {
        if *cached == mtime {
            return meta.clone();
        }
    }
    let mut meta = TailMeta::default();
    let len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_META_BYTES);
    let tail = read_range(path, start, len);
    let text = String::from_utf8_lossy(&tail);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    for line in lines.iter().rev() {
        if meta.updated == 0 {
            meta.updated = line_ts(line);
        }
        scan_meta(line, &mut meta);
        if meta.model.is_none() {
            meta.model = line_model(line);
        }
        if meta.title.is_some() && meta.cwd.is_some() && meta.updated > 0 && meta.model.is_some() {
            break;
        }
    }
    if meta.cwd.is_none() && start > 0 {
        let head = read_range(path, 0, HEAD_META_BYTES.min(len));
        let text = String::from_utf8_lossy(&head);
        for line in text.lines() {
            scan_meta(line, &mut meta);
            if meta.cwd.is_some() {
                break;
            }
        }
    }
    st.tails.insert(path.to_owned(), (mtime, meta.clone()));
    meta
}

/// Токены текущего хода по транскрипту. Результат пересчитывается только при
/// смене mtime; смещение запроса, с которого начался ход, запоминается, чтобы
/// не искать его заново при каждой дописанной строке.
pub(super) fn turn_usage(st: &mut St, path: &Path, mtime: SystemTime) -> Option<TurnUsage> {
    if let Some(c) = st.turns.get(path).filter(|c| c.mtime == mtime) {
        return c.usage.clone();
    }
    let len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let prev = st.turns.get(path).and_then(|c| c.at);
    let (at, usage) = read_turn(path, len, prev);
    st.turns.insert(path.to_owned(), TurnCache { at, mtime, usage: usage.clone() });
    usage
}

pub(super) fn read_turn(path: &Path, len: u64, prev: Option<u64>) -> (Option<u64>, Option<TurnUsage>) {
    // Запомненный запрос годится, пока на его месте всё ещё лежит запрос: файл
    // могли переписать или обрезать.
    if let Some(from) = prev.filter(|&p| p < len) {
        if let Some((at, usage)) = sum_turn(&read_range(path, from, len), from) {
            return (Some(at), Some(usage));
        }
    }
    let Some(from) = find_prompt(path, len) else { return (None, None) };
    match sum_turn(&read_range(path, from, len), from) {
        Some((at, usage)) => (Some(at), Some(usage)),
        None => (None, None),
    }
}

pub(super) fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Строка транскрипта, с которой начинается ход: запрос самого пользователя, а не
/// результат вызова инструмента, служебная вставка или запись подагента.
pub(super) fn is_prompt(line: &[u8]) -> bool {
    has(line, b"\"type\":\"user\"")
        && !has(line, b"\"type\":\"tool_result\"")
        && !has(line, b"\"isMeta\":true")
        && !has(line, b"\"isSidechain\":true")
        && !has(line, b"\"isCompactSummary\":true")
}

/// Смещение последнего запроса пользователя; ищет назад от конца файла.
pub(super) fn find_prompt(path: &Path, len: u64) -> Option<u64> {
    let floor = len.saturating_sub(TURN_MAX_BYTES);
    let mut end = len;
    let mut chunk = TURN_CHUNK;
    while end > floor {
        let start = end.saturating_sub(chunk).max(floor);
        let data = read_range(path, start, end);
        let mut lines: Vec<(u64, &[u8])> = Vec::new();
        let mut pos = 0u64;
        for line in data.split(|&b| b == b'\n') {
            lines.push((pos, line));
            pos += line.len() as u64 + 1;
        }
        // Первая строка куска может начинаться в предыдущем: её разбирает следующий круг.
        let whole_from = usize::from(start > 0);
        for (pos, line) in lines.iter().skip(whole_from).rev() {
            if is_prompt(line) {
                return Some(start + pos);
            }
        }
        if start == 0 {
            return None;
        }
        if lines.len() == 1 {
            // В куске нет ни одного перевода строки: одна огромная строка. Берём кусок больше.
            chunk *= 2;
        } else {
            end = start + lines[1].0;
        }
    }
    None
}

/// Складывает токены хода из записей, начиная с запроса пользователя в `data`
/// (`base` — его смещение в файле). Новый запрос внутри куска начинает ход заново.
/// `None`, если кусок начинается не с запроса. Недописанная последняя строка
/// пропускается: её подхватит следующий разбор.
pub(super) fn sum_turn(data: &[u8], base: u64) -> Option<(u64, TurnUsage)> {
    let mut at = base;
    let mut usage: Option<TurnUsage> = None;
    // Ответ пишется в транскрипт несколькими записями с одним message.id и одинаковым
    // usage (отдельно мысль, текст, вызов): считаем его один раз.
    let mut outputs: HashMap<String, u64> = HashMap::new();
    let mut pos = 0usize;
    let mut parts = data.split(|&b| b == b'\n').peekable();
    while let Some(line) = parts.next() {
        let offset = pos;
        pos += line.len() + 1;
        if parts.peek().is_none() {
            break;
        }
        if is_prompt(line) {
            at = base + offset as u64;
            usage = Some(TurnUsage { started_at: line_ts(&String::from_utf8_lossy(line)), input: 0, output: 0 });
            outputs.clear();
            continue;
        }
        let turn = usage.as_mut()?;
        if !has(line, b"\"type\":\"assistant\"") || has(line, b"\"isSidechain\":true") {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
        let u = &v["message"]["usage"];
        let (Some(id), Some(out)) = (v["message"]["id"].as_str(), u["output_tokens"].as_u64()) else { continue };
        let count = |key: &str| u[key].as_u64().unwrap_or(0);
        turn.input = count("input_tokens") + count("cache_read_input_tokens") + count("cache_creation_input_tokens");
        let slot = outputs.entry(id.to_owned()).or_insert(0);
        *slot = (*slot).max(out);
        turn.output = outputs.values().sum();
    }
    usage.map(|u| (at, u))
}

/// Время последней записи транскрипта; mtime — запасной вариант, если в файле
/// не нашлось ни одной записи с меткой времени.
pub(super) fn last_update(meta: &TailMeta, mtime: SystemTime) -> i64 {
    if meta.updated > 0 {
        meta.updated
    } else {
        to_ms(mtime)
    }
}

/// Метка времени записи транскрипта: `"timestamp":"2026-11-15T03:48:19.364Z"`.
pub(super) fn line_ts(line: &str) -> i64 {
    const KEY: &str = "\"timestamp\":\"";
    let Some(idx) = line.find(KEY) else { return 0 };
    let rest = &line[idx + KEY.len()..];
    let Some(end) = rest.find('"') else { return 0 };
    parse_iso_ms(&rest[..end])
}

/// Модель ответа ассистента: `"type":"assistant"` и `"model":"claude-…"` внутри `message`.
/// Ищется по подстроке, чтобы не разбирать длинные строки с ответами целиком.
/// Служебные ответы самого CLI (`<synthetic>`) моделью не считаются.
pub(super) fn line_model(line: &str) -> Option<String> {
    const KEY: &str = "\"model\":\"";
    if !line.contains("\"type\":\"assistant\"") {
        return None;
    }
    let rest = &line[line.find(KEY)? + KEY.len()..];
    let model = &rest[..rest.find('"')?];
    (!model.is_empty() && !model.starts_with('<')).then(|| model.to_owned())
}

pub(super) fn scan_meta(line: &str, meta: &mut TailMeta) {
    let titled = meta.title.is_none() && (line.contains("\"type\":\"ai-title\"") || line.contains("\"type\":\"custom-title\""));
    let has_cwd = meta.cwd.is_none() && line.len() < 30_000 && line.contains("\"cwd\":\"");
    if !titled && !has_cwd {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    if titled {
        meta.title = v["customTitle"].as_str().or(v["aiTitle"].as_str()).map(str::to_owned);
    }
    if has_cwd {
        meta.cwd = v["cwd"].as_str().map(str::to_owned);
    }
}

pub(super) fn read_range(path: &Path, start: u64, end: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Ok(mut f) = fs::File::open(path) {
        if f.seek(SeekFrom::Start(start)).is_ok() {
            let _ = f.take(end.saturating_sub(start)).read_to_end(&mut buf);
        }
    }
    buf
}

/// Читает `[start, end)` и разбирает только завершённые строки. Возвращает
/// элементы, число обработанных байт и смещение в файле, с которого начинаются
/// оставленные элементы (конец прочитанного, если оставлено пусто).
///
/// `max_items` ограничивает размер ленты, которую панель получает сразу; обрезка
/// идёт по строкам, чтобы смещение начала оставшегося было точным — с него
/// продолжится догрузка ранних сообщений.
pub(super) fn read_items(path: &Path, start: u64, end: u64, skip_first: bool, max_items: usize) -> (Vec<Item>, u64, u64) {
    let (items, _, consumed, begin) = read_feed(path, start, end, skip_first, max_items, None);
    (items, consumed, begin)
}

/// Очередь сообщений Claude Code по строкам `queue-operation` транскрипта.
///
/// Сообщение, набранное, пока модель занята (идёт долгий инструмент), Claude Code
/// сразу пишет строкой `enqueue`, а в ход вкладывает (`queued_command`) только
/// когда инструмент закончится. Чтобы панель показала его сразу, `enqueue` даёт
/// элемент «в очереди», а когда сообщение покидает очередь — `remove` (вложено
/// в ход), `dequeue` (ушло новым запросом, всегда голова очереди) или `popAll`
/// (Esc вернул очередь в поле ввода), — элемент убирается. Настоящее сообщение
/// затем приходит обычной строкой.
///
/// В очереди лежат и уведомления фоновых задач: их не показываем, но учитываем,
/// иначе `dequeue` снял бы не тот элемент.
#[derive(Default)]
pub(crate) struct Queue {
    /// Идентификатор показанного элемента ленты (по смещению строки `enqueue`;
    /// `None` у служебных) и содержимое.
    pending: VecDeque<(Option<String>, Value)>,
}

impl Queue {
    /// Разбирает строку `queue-operation`: новый элемент «в очереди» и/или
    /// идентификаторы элементов, покинувших очередь.
    fn apply(&mut self, line: &str, at: u64, items: &mut Vec<Item>, gone: &mut Vec<String>) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        if v["type"].as_str() != Some("queue-operation") {
            return;
        }
        let content = &v["content"];
        match v["operation"].as_str() {
            Some("enqueue") => {
                let text = content.as_str().unwrap_or_default();
                // Служебное (уведомления задач, команды харнесса) начинается с тега.
                if text.trim().is_empty() || text.trim_start().starts_with('<') {
                    self.pending.push_back((None, content.clone()));
                    return;
                }
                let id = format!("cq:{at}");
                self.pending.push_back((Some(id.clone()), content.clone()));
                items.push(Item {
                    id,
                    ts: v["timestamp"].as_str().map(parse_iso_ms).unwrap_or(0),
                    role: "user".into(),
                    kind: "queued".into(),
                    text: truncate(text, TEXT_LIMIT),
                    tool: None,
                });
            }
            Some("dequeue") => gone.extend(self.pending.pop_front().and_then(|(id, _)| id)),
            Some("remove") => {
                if let Some(i) = self.pending.iter().position(|(_, c)| c == content) {
                    gone.extend(self.pending.remove(i).and_then(|(id, _)| id));
                }
            }
            Some("popAll") => gone.extend(self.pending.drain(..).filter_map(|(id, _)| id)),
            _ => {}
        }
    }
}

/// То же, что `read_items`, но с учётом очереди сообщений (см. [`Queue`]), если
/// она передана. Вторым значением — идентификаторы элементов «в очереди» из
/// прошлых кусков, которые панель должна убрать.
pub(super) fn read_feed(
    path: &Path,
    start: u64,
    end: u64,
    skip_first: bool,
    max_items: usize,
    mut queue: Option<&mut Queue>,
) -> (Vec<Item>, Vec<String>, u64, u64) {
    let buf = read_range(path, start, end);
    let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
        return (Vec::new(), Vec::new(), 0, start);
    };
    let mut lines: Vec<(u64, Vec<Item>)> = Vec::new();
    let mut drop: Vec<String> = Vec::new();
    let mut at = 0usize;
    for (i, line) in buf[..last_nl].split(|b| *b == b'\n').enumerate() {
        let from = at;
        at += line.len() + 1;
        if i == 0 && skip_first {
            continue;
        }
        let mut items = Vec::new();
        if let Ok(line) = std::str::from_utf8(line) {
            match queue.as_deref_mut() {
                Some(q) if line.starts_with("{\"type\":\"queue-operation\"") => {
                    let mut gone = Vec::new();
                    q.apply(line, start + from as u64, &mut items, &mut gone);
                    for id in gone {
                        // Элемент из этого же куска просто не отдаём.
                        let before = lines.iter().map(|(_, l)| l.len()).sum::<usize>();
                        for (_, l) in lines.iter_mut() {
                            l.retain(|it| it.id != id);
                        }
                        lines.retain(|(_, l)| !l.is_empty());
                        if lines.iter().map(|(_, l)| l.len()).sum::<usize>() == before {
                            drop.push(id);
                        }
                    }
                }
                _ => parse_line(line, &mut items),
            }
        }
        if !items.is_empty() {
            lines.push((start + from as u64, items));
        }
    }
    let consumed = last_nl as u64 + 1;
    let mut total: usize = lines.iter().map(|(_, items)| items.len()).sum();
    let mut cut = 0;
    while total > max_items && cut < lines.len() {
        total -= lines[cut].1.len();
        cut += 1;
    }
    lines.drain(..cut);
    let begin = lines.first().map(|(at, _)| *at).unwrap_or(start + consumed);
    (lines.into_iter().flat_map(|(_, items)| items).collect(), drop, consumed, begin)
}

/// Запрос пользователя, который на деле прислал сам Claude Code: у таких строк
/// `origin.kind` не `human` (`task-notification`). В старых транскриптах поля
/// нет — тогда узнаём уведомление по тегу в начале текста.
pub(super) fn is_notice(v: &Value, text: &str) -> bool {
    match v["origin"]["kind"].as_str() {
        Some(kind) => kind != "human",
        None => text.trim_start().starts_with("<task-notification>"),
    }
}

/// Сводка после сжатия контекста (`/compact` или автосжатие).
pub(super) fn is_compact(v: &Value) -> bool {
    v["isCompactSummary"].as_bool() == Some(true)
}

/// Прогоняет завершённые строки `[start, end)` через слежение за вопросом
/// (см. [`crate::ask::Tracker`]). Кусок тот же, что только что ушёл в ленту:
/// новые строки короткие, а большой первый кусок читается один раз.
pub(super) fn scan_ask(path: &Path, start: u64, end: u64, skip_first: bool, ask: &mut crate::ask::Tracker) {
    let buf = read_range(path, start, end);
    let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else { return };
    for (i, line) in buf[..last_nl].split(|b| *b == b'\n').enumerate() {
        if i == 0 && skip_first {
            continue;
        }
        if let Ok(line) = std::str::from_utf8(line) {
            ask.line(line);
        }
    }
}

/// Дифф из результата правки: Claude Code кладёт в `toolUseResult` готовый
/// `structuredPatch`, а для нового файла — его содержимое.
pub(super) fn push_diff(v: &Value, uuid: &str, ts: i64, out: &mut Vec<Item>) {
    let r = &v["toolUseResult"];
    let Some(file) = r["filePath"].as_str().filter(|p| !p.is_empty()) else { return };
    let file = file.replace('\\', "/");
    let mut text = String::new();
    if let Some(hunks) = r["structuredPatch"].as_array().filter(|h| !h.is_empty()) {
        text.push_str(&format!("--- a/{file}\n+++ b/{file}\n"));
        for h in hunks {
            let old_start = h["oldStart"].as_i64().unwrap_or(0);
            let old_lines = h["oldLines"].as_i64().unwrap_or(0);
            let new_start = h["newStart"].as_i64().unwrap_or(0);
            let new_lines = h["newLines"].as_i64().unwrap_or(0);
            text.push_str(&format!("@@ -{old_start},{old_lines} +{new_start},{new_lines} @@\n"));
            for line in h["lines"].as_array().into_iter().flatten() {
                if let Some(line) = line.as_str() {
                    text.push_str(line);
                    text.push('\n');
                }
            }
        }
    } else if r["type"].as_str() == Some("create") {
        if let Some(content) = r["content"].as_str() {
            text = create_diff(&file, content);
        }
    } else if let (Some(old), Some(new)) = (r["oldString"].as_str(), r["newString"].as_str()) {
        text = edit_diff(&file, old, new);
    }
    if text.is_empty() {
        return;
    }
    out.push(Item {
        id: format!("{uuid}:diff"),
        ts,
        role: "assistant".into(),
        kind: "diff".into(),
        text: truncate(&text, DIFF_LIMIT),
        tool: None,
    });
}

pub(super) fn parse_line(line: &str, out: &mut Vec<Item>) {
    // Сообщение, отправленное в открытую сессию, пока модель работает: Claude Code
    // не пишет для него отдельную строку `type: user`, а вкладывает в текущий ход
    // вложением `queued_command`. Без разбора этой строки такие сообщения в ленту
    // не попадают вовсе.
    if line.contains("\"type\":\"queued_command\"") {
        parse_queued(line, out);
        return;
    }
    // Быстрый отсев: в транскрипте много служебных строк, в том числе очень длинных.
    if !(line.contains("\"type\":\"user\"") || line.contains("\"type\":\"assistant\"")) {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    let role = match v["type"].as_str() {
        Some("user") => "user",
        Some("assistant") => "assistant",
        _ => return,
    };
    if v["isSidechain"].as_bool() == Some(true) || v["isMeta"].as_bool() == Some(true) {
        return;
    }
    let uuid = v["uuid"].as_str().unwrap_or_default();
    let ts = v["timestamp"].as_str().map(parse_iso_ms).unwrap_or(0);
    // Правка файла: готовый патч лежит в toolUseResult строки с tool_result.
    if role == "user" {
        push_diff(&v, uuid, ts, out);
    }
    let mut push = |idx: usize, role: &str, kind: &str, text: String, tool: Option<String>| {
        out.push(Item { id: format!("{uuid}:{idx}"), ts, role: role.into(), kind: kind.into(), text, tool });
    };
    match &v["message"]["content"] {
        // Запрос, который Claude Code отправил сам (уведомление о фоновой задаче),
        // а не человек: в ленте это служебная строка, а не своё сообщение.
        // Сводка, которой Claude Code продолжает сессию после сжатия контекста:
        // запрос от имени пользователя, но писал его не человек.
        Value::String(s) if role == "user" && is_compact(&v) => {
            push(0, "system", "notice", truncate(s, TEXT_LIMIT), Some("compact".into()))
        }
        Value::Array(blocks) if role == "user" && is_compact(&v) => {
            let text = blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n");
            if !text.trim().is_empty() {
                push(0, "system", "notice", truncate(&text, TEXT_LIMIT), Some("compact".into()));
            }
        }
        Value::String(s) if role == "user" && is_notice(&v, s) => push(0, "system", "notice", truncate(s, TEXT_LIMIT), None),
        Value::String(s) if !s.trim().is_empty() => push(0, role, "text", truncate(s, TEXT_LIMIT), None),
        Value::Array(blocks) => {
            for (i, b) in blocks.iter().enumerate() {
                match b["type"].as_str() {
                    Some("text") => {
                        let text = b["text"].as_str().unwrap_or_default();
                        if !text.trim().is_empty() {
                            push(i, role, "text", truncate(text, TEXT_LIMIT), None);
                        }
                    }
                    Some("thinking") => {
                        let text = b["thinking"].as_str().unwrap_or_default();
                        if !text.trim().is_empty() {
                            push(i, role, "thinking", truncate(text, TEXT_LIMIT), None);
                        }
                    }
                    Some("tool_use") => push(
                        i,
                        "assistant",
                        "tool_use",
                        match b["name"].as_str() {
                            // Панель разбирает вопросы из JSON: обрезка по пределу
                            // аргументов (4000) сломала бы его на длинных вопросах.
                            Some("AskUserQuestion") => {
                                truncate(&serde_json::to_string(&b["input"]).unwrap_or_default(), TEXT_LIMIT)
                            }
                            name => tool_text(name, &b["input"]),
                        },
                        b["name"].as_str().map(str::to_owned),
                    ),
                    Some("tool_result") => {
                        let text = match &b["content"] {
                            Value::String(s) => s.clone(),
                            Value::Array(parts) => parts
                                .iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n"),
                            _ => String::new(),
                        };
                        push(i, "tool", "tool_result", truncate(&text, RESULT_LIMIT), None);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Сообщение из очереди, вставленное Claude Code в текущий ход. Отдельной строки
/// `type: user` у него в транскрипте нет — только вложение `queued_command`
/// с текстом в `prompt`. Показываем его как своё сообщение в ленте.
///
/// Так же приходит уведомление о фоновой задаче, если она кончилась посреди
/// хода (`origin.kind` = `task-notification`): это не сообщение человека,
/// а плашка `notice`, как и у уведомления отдельной строкой.
pub(super) fn parse_queued(line: &str, out: &mut Vec<Item>) {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    if v["type"].as_str() != Some("attachment") {
        return;
    }
    let a = &v["attachment"];
    if a["type"].as_str() != Some("queued_command") {
        return;
    }
    let notice = match a["origin"]["kind"].as_str() {
        Some("human") => false,
        Some("task-notification") => true,
        _ => return,
    };
    if v["isSidechain"].as_bool() == Some(true) || v["isMeta"].as_bool() == Some(true) {
        return;
    }
    let text = a["prompt"].as_str().unwrap_or_default();
    if text.trim().is_empty() {
        return;
    }
    let id = v["uuid"].as_str().or_else(|| a["source_uuid"].as_str()).unwrap_or_default();
    let ts = a["timestamp"]
        .as_str()
        .or_else(|| v["timestamp"].as_str())
        .map(parse_iso_ms)
        .unwrap_or(0);
    out.push(Item {
        id: format!("{id}:queued"),
        ts,
        role: if notice { "system" } else { "user" }.into(),
        kind: if notice { "notice" } else { "text" }.into(),
        text: truncate(text, TEXT_LIMIT),
        tool: None,
    });
}
