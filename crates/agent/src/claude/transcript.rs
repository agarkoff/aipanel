//! Разбор транскрипта Claude Code: что записано в `.jsonl` — сообщения,
//! вызовы инструментов, диффы, очередь и токены хода. Здесь только чтение
//! файла и превращение строк в элементы ленты; состояние сессий — в `mod.rs`.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::SystemTime,
};

use aipanel_proto::{Item, TurnUsage};
use serde_json::Value;

use crate::util::{create_diff, edit_diff, is_subagent_tool, parse_iso_ms, to_ms, tool_text, truncate, DIFF_LIMIT};

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

/// Границы подагента по строкам его транскрипта: первая и последняя метка
/// времени. По ним панель считает полное время работы.
#[derive(Clone, Copy, Default)]
pub(super) struct SubTimes {
    pub(super) started_at: i64,
    pub(super) ended_at: i64,
}

/// Накопленные токены подагента. Его транскрипт дописывается, поэтому читаем
/// только новые строки и помним, что уже сложено.
#[derive(Default)]
pub(super) struct SubTurn {
    mtime: Option<SystemTime>,
    /// Сколько байт файла разобрано: с этого места читается продолжение.
    len: u64,
    usage: Option<TurnUsage>,
    /// Выход по ответам: одна запись на `message.id`, хотя ответ пишется
    /// несколькими строками с одинаковым usage.
    outputs: HashMap<String, u64>,
    times: SubTimes,
}

/// Токены и границы подагента: его прогон — это весь транскрипт `agent-<id>.jsonl`.
/// Вход — контекст последнего ответа, выход — сумма по всем ответам.
pub(super) fn sub_turn(st: &mut St, path: &Path) -> (Option<TurnUsage>, SubTimes) {
    let Ok(md) = fs::metadata(path) else { return (None, SubTimes::default()) };
    let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let len = md.len();
    let cache = st.sub_turns.entry(path.to_owned()).or_default();
    if cache.mtime == Some(mtime) && cache.len == len {
        return (cache.usage.clone(), cache.times);
    }
    // Файл читается байтами, а строки дописываются целиком: недописанную
    // строку оставляем до следующего раза, иначе смещение разъедется.
    let from = if cache.mtime.is_some() && cache.len <= len { cache.len } else { 0 };
    if from == 0 {
        cache.outputs.clear();
        cache.usage = None;
        cache.times = SubTimes::default();
    }
    let data = read_range(path, from, len);
    cache.len = from + usage_lines(&data, &mut cache.usage, &mut cache.outputs, &mut cache.times);
    cache.mtime = Some(mtime);
    (cache.usage.clone(), cache.times)
}

/// Складывает токены ответов из куска транскрипта и двигает границы подагента.
/// Возвращает число байт до последней завершённой строки: её продолжение
/// дочитается в следующий раз.
pub(super) fn usage_lines(
    data: &[u8],
    usage: &mut Option<TurnUsage>,
    outputs: &mut HashMap<String, u64>,
    times: &mut SubTimes,
) -> u64 {
    let mut pos = 0;
    let mut consumed = 0;
    while let Some(nl) = data[pos..].iter().position(|&b| b == b'\n') {
        let line = &data[pos..pos + nl];
        pos += nl + 1;
        consumed = pos;
        let ts = line_ts_bytes(line);
        if ts > 0 {
            if times.started_at == 0 {
                times.started_at = ts;
            }
            times.ended_at = times.ended_at.max(ts);
        }
        if !has(line, b"\"type\":\"assistant\"") {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
        let u = &v["message"]["usage"];
        let (Some(id), Some(out)) = (v["message"]["id"].as_str(), u["output_tokens"].as_u64()) else { continue };
        let count = |key: &str| u[key].as_u64().unwrap_or(0);
        let turn = usage.get_or_insert_with(|| TurnUsage { started_at: ts, input: 0, output: 0 });
        turn.input = count("input_tokens") + count("cache_read_input_tokens") + count("cache_creation_input_tokens");
        let slot = outputs.entry(id.to_owned()).or_insert(0);
        if out > *slot {
            // Ответ пишется несколькими строками с одинаковым usage: выход
            // прибавляется только на разницу с уже учтённым.
            turn.output += out - *slot;
            *slot = out;
        }
    }
    consumed as u64
}

/// Метка времени строки транскрипта без выделения памяти.
fn line_ts_bytes(line: &[u8]) -> i64 {
    const KEY: &[u8] = b"\"timestamp\":\"";
    let Some(idx) = line.windows(KEY.len()).position(|w| w == KEY) else { return 0 };
    let rest = &line[idx + KEY.len()..];
    let Some(end) = rest.iter().position(|&b| b == b'"') else { return 0 };
    std::str::from_utf8(&rest[..end]).map(parse_iso_ms).unwrap_or(0)
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

/// Что разбирать и что прятать при чтении строк транскрипта.
#[derive(Default)]
pub(super) struct ParseOpts<'a> {
    /// Не показывать вызовы подагентов (`Agent`, `Task`) и их результаты: у них
    /// своя лента, которая открывается из списка подагентов.
    pub(super) hide_subagents: bool,
    /// id вызовов подагентов из файлов `<id>/subagents/*.meta.json`: по ним
    /// прячется результат вызова, даже если строка самого вызова осталась за
    /// пределами прочитанного куска.
    pub(super) hide_ids: Option<&'a HashSet<String>>,
    /// Разбирать записи подагента: в его транскрипте все строки помечены
    /// `isSidechain`, а в ленте сессии такие строки пропускаются.
    pub(super) sidechain: bool,
    /// id вызовов подагентов, встреченных в этом куске: по ним прячется
    /// результат вызова, даже когда файла метаданных ещё нет.
    pub(super) seen: HashSet<String>,
}

/// Читает `[start, end)` и разбирает только завершённые строки. Возвращает
/// элементы, число обработанных байт и смещение в файле, с которого начинаются
/// оставленные элементы (конец прочитанного, если оставлено пусто).
///
/// `max_items` ограничивает размер ленты, которую панель получает сразу; обрезка
/// идёт по строкам, чтобы смещение начала оставшегося было точным — с него
/// продолжится догрузка ранних сообщений.
pub(super) fn read_items(
    path: &Path,
    start: u64,
    end: u64,
    skip_first: bool,
    max_items: usize,
    mut opts: ParseOpts,
) -> (Vec<Item>, u64, u64) {
    let (items, _, consumed, begin) = read_feed(path, start, end, skip_first, max_items, None, &mut opts);
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
    opts: &mut ParseOpts,
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
                _ => parse_line_opts(line, &mut items, opts),
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
    scan_lines(path, start, end, skip_first, |l| ask.line(l));
}

/// То же для фоновых задач (см. [`crate::tasks::Tracker`]).
pub(super) fn scan_tasks(path: &Path, start: u64, end: u64, skip_first: bool, tasks: &mut crate::tasks::Tracker) {
    scan_lines(path, start, end, skip_first, |l| tasks.line(l));
}

fn scan_lines(path: &Path, start: u64, end: u64, skip_first: bool, mut f: impl FnMut(&str)) {
    let buf = read_range(path, start, end);
    let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else { return };
    for (i, line) in buf[..last_nl].split(|b| *b == b'\n').enumerate() {
        if i == 0 && skip_first {
            continue;
        }
        if let Ok(line) = std::str::from_utf8(line) {
            f(line);
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

#[cfg(test)]
pub(super) fn parse_line(line: &str, out: &mut Vec<Item>) {
    parse_line_opts(line, out, &mut ParseOpts::default());
}

pub(super) fn parse_line_opts(line: &str, out: &mut Vec<Item>, opts: &mut ParseOpts) {
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
    // Записи подагента лежат и в его собственном транскрипте, и отдельными
    // строками с `isSidechain` в основном. В ленте сессии вторые не нужны.
    if v["isMeta"].as_bool() == Some(true) || (!opts.sidechain && v["isSidechain"].as_bool() == Some(true)) {
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
                    Some("tool_use") => {
                        let name = b["name"].as_str();
                        let id = b["id"].as_str().unwrap_or_default();
                        let subagent = name.is_some_and(is_subagent_tool);
                        // id из метаданных подагентов: вызов мог остаться за
                        // пределами этого куска, а его результат — попасть в него.
                        let known = !id.is_empty() && opts.hide_ids.is_some_and(|ids| ids.contains(id));
                        if subagent && opts.hide_subagents {
                            // По этому id прячется результат вызова ниже.
                            if !id.is_empty() {
                                opts.seen.insert(id.to_owned());
                            }
                        }
                        if !(opts.hide_subagents && (subagent || known)) {
                            push(
                                i,
                                "assistant",
                                "tool_use",
                                match name {
                                    // Панель разбирает вопросы из JSON: обрезка по пределу
                                    // аргументов (4000) сломала бы его на длинных вопросах.
                                    Some("AskUserQuestion") => {
                                        truncate(&serde_json::to_string(&b["input"]).unwrap_or_default(), TEXT_LIMIT)
                                    }
                                    name => tool_text(name, &b["input"]),
                                },
                                name.map(str::to_owned),
                            );
                        }
                    }
                    Some("tool_result") => {
                        let use_id = b["tool_use_id"].as_str().unwrap_or_default();
                        let hidden = opts.hide_subagents
                            && !use_id.is_empty()
                            && (opts.hide_ids.is_some_and(|ids| ids.contains(use_id))
                                || opts.seen.contains(use_id));
                        if !hidden {
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
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if role == "user" {
        push_cwd_switch(&v, uuid, ts, out);
    }
}

/// Отметка о смене каталога сессии: EnterWorktree переводит Claude Code в
/// `<репозиторий>/.claude/worktrees/<имя>`, ExitWorktree возвращает обратно.
/// Сам вызов в ленте и так виден, но строкой инструмента; отметка — явная.
///
/// Результат EnterWorktree: `toolUseResult` с `worktreePath` (проверено на
/// транскриптах). Результат ExitWorktree узнаём по `originalCwd` — куда вернулись.
pub(super) fn push_cwd_switch(v: &Value, uuid: &str, ts: i64, out: &mut Vec<Item>) {
    let r = &v["toolUseResult"];
    let to = match (r["originalCwd"].as_str(), r["worktreePath"].as_str()) {
        (Some(back), _) if !back.is_empty() => back,
        (None, Some(wt)) if !wt.is_empty() => wt,
        _ => return,
    };
    out.push(Item {
        id: format!("{uuid}:cwd"),
        ts,
        role: "system".into(),
        kind: "cwd".into(),
        text: to.to_owned(),
        tool: None,
    });
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
