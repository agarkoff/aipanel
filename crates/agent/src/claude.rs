//! Сессии Claude Code.
//!
//! Открытые сессии берутся из реестра `~/.claude/sessions/<pid>.json`, лента — из
//! транскрипта `~/.claude/projects/<проект>/<sessionId>.jsonl`. В открытую сессию
//! текст вводится через её консоль, для закрытой открывается новое окно консоли
//! `claude --resume <id> -- <текст>` (вне Windows — фоновый `claude -p --resume`).

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime},
};

use aipanel_proto::{AgentMsg, Item, SessionInfo};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc::UnboundedSender, oneshot, Notify},
};

use crate::{
    config::ClaudeCfg,
    handoff,
    util::{basename, create_diff, edit_diff, now_ms, parse_iso_ms, to_ms, tool_text, truncate, DIFF_LIMIT},
};

const TAIL_META_BYTES: u64 = 256 * 1024;
const HEAD_META_BYTES: u64 = 64 * 1024;
const HISTORY_BYTES: u64 = 4 * 1024 * 1024;
/// Для выжимки достаточно хвоста транскрипта: он содержит последние реплики.
const HANDOFF_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CHUNK: u64 = 32 * 1024 * 1024;
const HISTORY_ITEMS: usize = 200;
const TEXT_LIMIT: usize = 20_000;
const RESULT_LIMIT: usize = 4_000;
const NESTED_SESSION_ENV: [&str; 8] = [
    "CLAUDECODE",
    "CLAUDE_PID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
];
/// Пока окно запускается, повторное сообщение в ту же сессию не открывает второе окно.
const LAUNCH_GUARD: Duration = Duration::from_secs(25);
/// Предел командной строки Windows — 32 767 символов; запас оставлен под служебные аргументы.
pub const MAX_ARG_UTF16: usize = 12_000;

/// Если агент запущен из сессии Claude Code, её переменные пометили бы дочерние
/// процессы как вложенные: такие процессы не сохраняют транскрипт.
pub fn scrub_env() {
    for var in NESTED_SESSION_ENV {
        std::env::remove_var(var);
    }
}

#[derive(Clone)]
struct Known {
    pid: Option<u32>,
    cwd: String,
    path: Option<PathBuf>,
}

#[derive(Clone, Default)]
struct TailMeta {
    title: Option<String>,
    cwd: Option<String>,
    /// Время последней записи транскрипта (мс от эпохи), а не mtime файла:
    /// живой CLI «касается» файла раз в час, даже когда новых сообщений нет.
    updated: i64,
}

#[derive(Default)]
struct St {
    known: HashMap<String, Known>,
    tails: HashMap<PathBuf, (SystemTime, TailMeta)>,
    transcripts: HashMap<String, PathBuf>,
    listed_at: Option<Instant>,
    headless: HashMap<String, oneshot::Sender<()>>,
    launched: HashMap<String, Instant>,
}

pub struct Claude {
    cfg: ClaudeCfg,
    home: PathBuf,
    rescan: Arc<Notify>,
    st: Mutex<St>,
}

impl Claude {
    pub fn new(cfg: ClaudeCfg, rescan: Arc<Notify>) -> Self {
        let home = cfg.home.clone().unwrap_or_else(|| crate::util::home_dir().join(".claude"));
        Self { cfg, home, rescan, st: Mutex::new(St::default()) }
    }

    fn lock(&self) -> MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Все транскрипты верхнего уровня: sessionId -> путь.
    fn list_transcripts(&self) -> HashMap<String, PathBuf> {
        let mut map = HashMap::new();
        let Ok(projects) = fs::read_dir(self.home.join("projects")) else {
            return map;
        };
        for project in projects.flatten() {
            let Ok(files) = fs::read_dir(project.path()) else { continue };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().is_some_and(|e| e == "jsonl") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        map.insert(stem.to_owned(), path.clone());
                    }
                }
            }
        }
        map
    }

    pub fn scan(&self) -> Vec<SessionInfo> {
        let mut st = self.lock();
        if st.listed_at.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
            st.transcripts = self.list_transcripts();
            st.listed_at = Some(Instant::now());
        }

        st.launched.retain(|_, t| t.elapsed() < LAUNCH_GUARD);
        let mut out = Vec::new();
        let mut known = HashMap::new();
        let mut used = HashSet::new();

        // Открытые сессии.
        if let Ok(entries) = fs::read_dir(self.home.join("sessions")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let Some(v) = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
                else {
                    continue;
                };
                let (Some(pid), Some(sid)) = (v["pid"].as_u64(), v["sessionId"].as_str()) else {
                    continue;
                };
                let pid = pid as u32;
                if !crate::platform::pid_alive(pid) {
                    continue;
                }
                let transcript = st.transcripts.get(sid).cloned();
                let mut updated = v["statusUpdatedAt"].as_i64().or(v["updatedAt"].as_i64()).unwrap_or(0);
                let mut meta = TailMeta::default();
                if let Some(p) = &transcript {
                    if let Ok(mtime) = fs::metadata(p).and_then(|m| m.modified()) {
                        meta = tail_meta(&mut st, p, mtime);
                        updated = updated.max(meta.updated);
                        used.insert(p.clone());
                    }
                }
                let cwd = v["cwd"].as_str().unwrap_or_default().to_owned();
                let name = v["name"].as_str().unwrap_or(sid);
                out.push(SessionInfo {
                    key: format!("claude:{sid}"),
                    tool: "claude".into(),
                    title: meta.title.unwrap_or_else(|| name.to_owned()),
                    cwd: cwd.clone(),
                    status: v["status"].as_str().unwrap_or("idle").to_owned(),
                    live: true,
                    updated_at: updated,
                    delivery: if cfg!(windows) { "console" } else { "none" }.into(),
                });
                known.insert(sid.to_owned(), Known { pid: Some(pid), cwd, path: transcript });
            }
        }

        // Недавние закрытые сессии. Порядок и время берём по последней записи
        // транскрипта: mtime файла сдвигается и без новых сообщений.
        let cutoff = SystemTime::now() - Duration::from_secs(self.cfg.recent_days * 86_400);
        let recent: Vec<(String, PathBuf, SystemTime)> = st
            .transcripts
            .iter()
            .filter(|(sid, _)| !known.contains_key(*sid))
            .filter_map(|(sid, p)| {
                let mtime = fs::metadata(p).and_then(|m| m.modified()).ok()?;
                (mtime >= cutoff).then(|| (sid.clone(), p.clone(), mtime))
            })
            .collect();
        let mut closed: Vec<(String, PathBuf, SystemTime, TailMeta)> = recent
            .into_iter()
            .map(|(sid, path, mtime)| {
                let meta = tail_meta(&mut st, &path, mtime);
                (sid, path, mtime, meta)
            })
            .collect();
        closed.sort_by_key(|(_, _, mtime, meta)| -last_update(meta, *mtime));
        closed.truncate(self.cfg.max_recent);
        for (sid, path, mtime, meta) in closed {
            used.insert(path.clone());
            let updated = last_update(&meta, mtime);
            let cwd = meta.cwd.clone().unwrap_or_default();
            let running = st.headless.contains_key(&sid) || st.launched.contains_key(&sid);
            let title = meta.title.unwrap_or_else(|| match basename(&cwd) {
                "" => sid.chars().take(8).collect(),
                dir => dir.to_owned(),
            });
            out.push(SessionInfo {
                key: format!("claude:{sid}"),
                tool: "claude".into(),
                title,
                cwd: cwd.clone(),
                status: if running { "busy" } else { "stopped" }.into(),
                live: false,
                updated_at: updated,
                delivery: if cwd.is_empty() {
                    "none"
                } else if cfg!(windows) {
                    "window"
                } else {
                    "headless"
                }
                .into(),
            });
            known.insert(sid, Known { pid: None, cwd, path: Some(path) });
        }

        st.tails.retain(|p, _| used.contains(p));
        st.known = known;
        out
    }

    fn find_path(&self, sid: &str) -> Option<PathBuf> {
        if let Some(p) = self.lock().known.get(sid).and_then(|k| k.path.clone()) {
            return Some(p);
        }
        let file = format!("{sid}.jsonl");
        fs::read_dir(self.home.join("projects"))
            .ok()?
            .flatten()
            .map(|d| d.path().join(&file))
            .find(|p| p.is_file())
    }

    /// Удаляет транскрипт закрытой сессии вместе с каталогом её подпроцессов.
    /// Открытую или выполняющуюся сессию не трогаем: Claude Code дописывал бы
    /// в удалённый файл.
    pub fn delete(&self, sid: &str) -> Result<(), String> {
        let known = self.lock().known.get(sid).cloned();
        if known.as_ref().is_some_and(|k| k.pid.is_some()) {
            return Err("сессия открыта в терминале — закройте её перед удалением".into());
        }
        {
            let st = self.lock();
            if st.headless.contains_key(sid) || st.launched.contains_key(sid) {
                return Err("сессия выполняется — дождитесь завершения или прервите её".into());
            }
        }
        let path = known
            .and_then(|k| k.path)
            .or_else(|| self.find_path(sid))
            .ok_or("транскрипт сессии не найден")?;
        fs::remove_file(&path).map_err(|e| format!("не удалось удалить транскрипт: {e}"))?;
        // Рядом Claude Code хранит транскрипты подпроцессов: <id>/subagents/…
        if let Some(dir) = path.parent().map(|p| p.join(sid)).filter(|p| p.is_dir()) {
            let _ = fs::remove_dir_all(dir);
        }
        let mut st = self.lock();
        st.known.remove(sid);
        st.tails.remove(&path);
        st.transcripts.remove(sid);
        Ok(())
    }

    /// Следит за транскриптом и отправляет ленту, пока задачу не отменят.
    pub async fn watch(&self, key: String, sid: String, out: UnboundedSender<AgentMsg>) {
        let mut path: Option<PathBuf> = None;
        let mut offset = 0u64;
        let mut first = true;
        let mut told_empty = false;
        loop {
            if path.is_none() {
                path = self.find_path(&sid);
            }
            match path.as_deref().map(|p| (p, fs::metadata(p))) {
                Some((p, Ok(md))) => {
                    let len = md.len();
                    if first || len < offset {
                        let start = len.saturating_sub(HISTORY_BYTES);
                        let (mut items, consumed) = read_items(p, start, len, start > 0);
                        if items.len() > HISTORY_ITEMS {
                            items.drain(..items.len() - HISTORY_ITEMS);
                        }
                        offset = start + consumed;
                        first = false;
                        let _ = out.send(AgentMsg::History {
                            session: key.clone(),
                            reset: true,
                            items,
                            drop: Vec::new(),
                        });
                    } else if len > offset {
                        let (items, consumed) = read_items(p, offset, len.min(offset + MAX_CHUNK), false);
                        offset += consumed;
                        if !items.is_empty() {
                            let _ = out.send(AgentMsg::History {
                                session: key.clone(),
                                reset: false,
                                items,
                                drop: Vec::new(),
                            });
                        }
                    }
                }
                Some((_, Err(_))) => path = None,
                None => {
                    if !told_empty {
                        told_empty = true;
                        let _ = out.send(AgentMsg::History {
                            session: key.clone(),
                            reset: true,
                            items: vec![],
                            drop: Vec::new(),
                        });
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
    }

    pub async fn send(self: &Arc<Self>, sid: &str, text: &str, out: &UnboundedSender<AgentMsg>) -> Result<(), String> {
        let known = self.lock().known.get(sid).cloned().ok_or("сессия не найдена")?;
        match known.pid {
            Some(pid) => crate::platform::console_input(pid, Some(text)).await,
            None if cfg!(windows) => self.open_window(sid, &known.cwd, text),
            None => self.headless(sid, &known.cwd, text, out),
        }
    }

    /// Каталог и выжимка из транскрипта для продолжения в другом инструменте.
    pub fn handoff_source(&self, sid: &str) -> Result<(String, String), String> {
        let known = self.lock().known.get(sid).cloned();
        let path = known
            .as_ref()
            .and_then(|k| k.path.clone())
            .or_else(|| self.find_path(sid))
            .ok_or("транскрипт сессии не найден")?;
        let cwd = match known.as_ref().map(|k| k.cwd.clone()).filter(|c| !c.is_empty()) {
            Some(cwd) => cwd,
            None => {
                let mtime = fs::metadata(&path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                tail_meta(&mut self.lock(), &path, mtime).cwd.unwrap_or_default()
            }
        };
        if cwd.is_empty() {
            return Err("каталог сессии неизвестен".into());
        }
        let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let start = len.saturating_sub(HANDOFF_BYTES);
        let (items, _) = read_items(&path, start, len, start > 0);
        let messages: Vec<(String, String)> = items
            .into_iter()
            .filter(|i| i.kind == "text" && (i.role == "user" || i.role == "assistant"))
            .map(|i| (i.role, i.text))
            .collect();
        if messages.is_empty() {
            return Err("в сессии нет текста для переноса".into());
        }
        Ok((cwd.clone(), handoff::build("Claude Code", &cwd, &messages)))
    }

    /// Открывает новую сессию Claude Code в каталоге `cwd`. Идентификатор появится
    /// в списке после того, как Claude Code запишет реестр сессий.
    pub fn start(&self, cwd: &str) -> Result<Option<String>, String> {
        self.start_with_prompt(cwd, None)
    }

    /// Открывает новую сессию; `prompt` уходит её первым сообщением.
    pub fn start_with_prompt(&self, cwd: &str, prompt: Option<&str>) -> Result<Option<String>, String> {
        crate::util::session_dir(cwd)?;
        let mut args = Vec::new();
        if let Some(text) = prompt {
            if text.encode_utf16().count() > MAX_ARG_UTF16 {
                return Err(format!("сообщение слишком длинное для запуска окна (до {MAX_ARG_UTF16} символов)"));
            }
            // `--` нужен, чтобы текст, начинающийся с «-», не приняли за параметр.
            args.push("--".into());
            args.push(text.to_owned());
        }
        crate::platform::spawn_window(&self.cfg.bin, &args, cwd)?;
        self.rescan.notify_one();
        Ok(None)
    }

    /// Открывает видимое окно консоли с сессией; сообщение уходит первым запросом.
    fn open_window(&self, sid: &str, cwd: &str, text: &str) -> Result<(), String> {
        if cwd.is_empty() || !Path::new(cwd).is_dir() {
            return Err(format!("каталог сессии недоступен: {cwd}"));
        }
        if text.encode_utf16().count() > MAX_ARG_UTF16 {
            return Err(format!("сообщение слишком длинное для запуска окна (до {MAX_ARG_UTF16} символов)"));
        }
        {
            let mut st = self.lock();
            if st.launched.get(sid).is_some_and(|t| t.elapsed() < LAUNCH_GUARD) {
                return Err("окно этой сессии уже открывается".into());
            }
            st.launched.insert(sid.to_owned(), Instant::now());
        }
        let mut args = vec!["--resume".to_owned(), sid.to_owned()];
        args.extend(self.cfg.resume_args.iter().cloned());
        // `--` нужен, чтобы текст, начинающийся с «-», не приняли за параметр.
        args.push("--".into());
        args.push(text.to_owned());
        match crate::platform::spawn_window(&self.cfg.bin, &args, cwd) {
            Ok(_) => {
                self.rescan.notify_one();
                Ok(())
            }
            Err(e) => {
                self.lock().launched.remove(sid);
                Err(e)
            }
        }
    }

    pub async fn interrupt(&self, sid: &str) -> Result<(), String> {
        let known = self.lock().known.get(sid).cloned().ok_or("сессия не найдена")?;
        if let Some(pid) = known.pid {
            return crate::platform::console_input(pid, None).await;
        }
        match self.lock().headless.remove(sid) {
            Some(kill) => {
                let _ = kill.send(());
                Ok(())
            }
            None => Err("сессия сейчас ничего не выполняет".into()),
        }
    }

    /// Продолжает закрытую сессию фоновым процессом `claude -p --resume <id>`.
    fn headless(self: &Arc<Self>, sid: &str, cwd: &str, text: &str, out: &UnboundedSender<AgentMsg>) -> Result<(), String> {
        if cwd.is_empty() || !Path::new(cwd).is_dir() {
            return Err(format!("каталог сессии недоступен: {cwd}"));
        }
        let (kill_tx, kill_rx) = oneshot::channel();
        {
            let mut st = self.lock();
            if st.headless.contains_key(sid) {
                return Err("сессия уже выполняет предыдущее сообщение".into());
            }
            st.headless.insert(sid.to_owned(), kill_tx);
        }

        let mut cmd = tokio::process::Command::new(&self.cfg.bin);
        cmd.args(["-p", "--resume", sid])
            .args(&self.cfg.resume_args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        crate::platform::no_window(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.lock().headless.remove(sid);
                return Err(format!("не удалось запустить {}: {e}", self.cfg.bin));
            }
        };

        let me = Arc::clone(self);
        let sid = sid.to_owned();
        let text = text.to_owned();
        let out = out.clone();
        self.rescan.notify_one();
        tokio::spawn(async move {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes()).await;
            }
            let stderr = child.stderr.take();
            let read_err = tokio::spawn(async move {
                let mut buf = String::new();
                if let Some(mut e) = stderr {
                    let _ = e.read_to_string(&mut buf).await;
                }
                buf
            });
            let status = tokio::select! {
                s = child.wait() => s.ok(),
                _ = kill_rx => {
                    let _ = child.kill().await;
                    None
                }
            };
            let err = read_err.await.unwrap_or_default();
            me.lock().headless.remove(&sid);
            me.rescan.notify_one();
            if let Some(status) = status.filter(|s| !s.success()) {
                let _ = out.send(AgentMsg::History {
                    session: format!("claude:{sid}"),
                    reset: false,
                    drop: Vec::new(),
                    items: vec![Item {
                        id: format!("err-{}", now_ms()),
                        ts: now_ms(),
                        role: "system".into(),
                        kind: "error".into(),
                        text: format!(
                            "claude -p завершился с кодом {}: {}",
                            status.code().map_or("?".into(), |c| c.to_string()),
                            truncate(err.trim(), 2000)
                        ),
                        tool: None,
                    }],
                });
            }
        });
        Ok(())
    }
}

/// Название и каталог сессии из хвоста (и при необходимости начала) транскрипта.
fn tail_meta(st: &mut St, path: &Path, mtime: SystemTime) -> TailMeta {
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
        if meta.title.is_some() && meta.cwd.is_some() && meta.updated > 0 {
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

/// Время последней записи транскрипта; mtime — запасной вариант, если в файле
/// не нашлось ни одной записи с меткой времени.
fn last_update(meta: &TailMeta, mtime: SystemTime) -> i64 {
    if meta.updated > 0 {
        meta.updated
    } else {
        to_ms(mtime)
    }
}

/// Метка времени записи транскрипта: `"timestamp":"2026-11-15T03:48:19.364Z"`.
fn line_ts(line: &str) -> i64 {
    const KEY: &str = "\"timestamp\":\"";
    let Some(idx) = line.find(KEY) else { return 0 };
    let rest = &line[idx + KEY.len()..];
    let Some(end) = rest.find('"') else { return 0 };
    parse_iso_ms(&rest[..end])
}

fn scan_meta(line: &str, meta: &mut TailMeta) {
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

fn read_range(path: &Path, start: u64, end: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Ok(mut f) = fs::File::open(path) {
        if f.seek(SeekFrom::Start(start)).is_ok() {
            let _ = f.take(end.saturating_sub(start)).read_to_end(&mut buf);
        }
    }
    buf
}

/// Читает `[start, end)` и разбирает только завершённые строки.
/// Возвращает элементы ленты и число обработанных байт.
fn read_items(path: &Path, start: u64, end: u64, skip_first: bool) -> (Vec<Item>, u64) {
    let buf = read_range(path, start, end);
    let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
        return (Vec::new(), 0);
    };
    let mut items = Vec::new();
    for (i, line) in buf[..last_nl].split(|b| *b == b'\n').enumerate() {
        if i == 0 && skip_first {
            continue;
        }
        if let Ok(line) = std::str::from_utf8(line) {
            parse_line(line, &mut items);
        }
    }
    (items, last_nl as u64 + 1)
}

/// Дифф из результата правки: Claude Code кладёт в `toolUseResult` готовый
/// `structuredPatch`, а для нового файла — его содержимое.
fn push_diff(v: &Value, uuid: &str, ts: i64, out: &mut Vec<Item>) {
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

fn parse_line(line: &str, out: &mut Vec<Item>) {
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
                        tool_text(b["name"].as_str(), &b["input"]),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_edit_diff() {
        let mut items = Vec::new();
        parse_line(
            r#"{"type":"user","uuid":"u4","timestamp":"2026-11-05T10:06:22.084Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]},"toolUseResult":{"filePath":"D:\\p\\a.rs","oldString":"x","newString":"y","structuredPatch":[{"oldStart":3,"oldLines":2,"newStart":3,"newLines":3,"lines":[" ctx","-old","+new","+more"]}]}}"#,
            &mut items,
        );
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.kind.as_str(), i.text.as_str())).collect();
        assert_eq!(
            got,
            [
                ("u4:diff", "diff", "--- a/D:/p/a.rs\n+++ b/D:/p/a.rs\n@@ -3,2 +3,3 @@\n ctx\n-old\n+new\n+more\n"),
                ("u4:0", "tool_result", "ok"),
            ]
        );
    }

    #[test]
    fn parses_created_file_diff() {
        let mut items = Vec::new();
        parse_line(
            r#"{"type":"user","uuid":"u5","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]},"toolUseResult":{"type":"create","filePath":"D:\\p\\new.txt","content":"a\nb\n","structuredPatch":[]}}"#,
            &mut items,
        );
        assert_eq!(items[0].kind, "diff");
        assert_eq!(items[0].text, "--- /dev/null\n+++ b/D:/p/new.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n");
    }

    #[test]
    fn parses_transcript_lines() {
        let mut items = Vec::new();
        parse_line(r#"{"type":"mode","mode":"normal"}"#, &mut items);
        parse_line(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-11-05T10:06:22.084Z","message":{"role":"user","content":"привет"}}"#,
            &mut items,
        );
        parse_line(
            r#"{"type":"user","uuid":"u2","isMeta":true,"message":{"role":"user","content":"skip"}}"#,
            &mut items,
        );
        parse_line(
            r#"{"type":"assistant","uuid":"a1","message":{"role":"assistant","content":[{"type":"thinking","thinking":""},{"type":"text","text":"ок"},{"type":"tool_use","name":"Bash","input":{"command":"ls","description":"d"}}]}}"#,
            &mut items,
        );
        parse_line(
            r#"{"type":"user","uuid":"u3","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"out"}]}]}}"#,
            &mut items,
        );
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.kind.as_str(), i.text.as_str())).collect();
        assert_eq!(
            got,
            [
                ("u1:0", "text", "привет"),
                ("a1:1", "text", "ок"),
                ("a1:2", "tool_use", "ls"),
                ("u3:0", "tool_result", "out"),
            ]
        );
        assert_eq!(items[2].tool.as_deref(), Some("Bash"));
    }

    /// Удаляется только закрытая сессия: открытую в терминале трогать нельзя.
    #[test]
    fn deletes_closed_transcript() {
        let dir = std::env::temp_dir().join(format!("aipanel-claude-delete-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let project = dir.join("projects").join("p");
        let sessions = dir.join("sessions");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        let sid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let path = project.join(format!("{sid}.jsonl"));
        fs::write(&path, "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n").unwrap();
        // Рядом Claude Code хранит транскрипты подпроцессов.
        fs::create_dir_all(project.join(sid).join("subagents")).unwrap();

        let cfg = ClaudeCfg { home: Some(dir.clone()), ..ClaudeCfg::default() };
        let claude = Claude::new(cfg, Arc::new(Notify::new()));
        assert_eq!(claude.scan().len(), 1);

        claude.delete(sid).unwrap();
        assert!(!path.exists());
        assert!(!project.join(sid).exists());

        // Та же сессия, но открытая: удаление отклоняется.
        fs::write(&path, "{}\n").unwrap();
        fs::write(
            sessions.join(format!("{}.json", std::process::id())),
            format!(
                "{{\"pid\":{},\"sessionId\":\"{sid}\",\"cwd\":\"D:/p\",\"name\":\"n\",\"status\":\"idle\"}}",
                std::process::id()
            ),
        )
        .unwrap();
        assert!(claude.scan()[0].live);
        assert!(claude.delete(sid).is_err());
        assert!(path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    /// Регресс: mtime транскрипта сдвигается и без новых сообщений (живой CLI
    /// «касается» файла раз в час), поэтому время сессии считается по записям.
    #[test]
    fn session_time_comes_from_records() {
        let dir = std::env::temp_dir().join(format!("aipanel-claude-time-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let project = dir.join("projects").join("p");
        let sessions = dir.join("sessions");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        let sid = "11111111-2222-3333-4444-555555555555";
        fs::write(
            project.join(format!("{sid}.jsonl")),
            "{\"type\":\"user\",\"cwd\":\"D:/p\",\"timestamp\":\"2026-11-15T03:48:19.364Z\",\
             \"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )
        .unwrap();

        let registry = sessions.join(format!("{}.json", std::process::id()));
        fs::write(
            &registry,
            format!(
                "{{\"pid\":{},\"sessionId\":\"{sid}\",\"cwd\":\"D:/p\",\"name\":\"n\",\
                 \"status\":\"idle\",\"statusUpdatedAt\":1794714499387}}",
                std::process::id()
            ),
        )
        .unwrap();

        let cfg = ClaudeCfg { home: Some(dir.clone()), ..ClaudeCfg::default() };
        let claude = Claude::new(cfg, Arc::new(Notify::new()));

        // Открытая сессия: время из реестра (оно новее последней записи), а не mtime.
        let live = claude.scan();
        assert_eq!(live.len(), 1);
        assert!(live[0].live);
        assert_eq!(live[0].updated_at, 1_794_714_499_387);

        // Закрытая сессия: время последней записи, хотя файл только что создан.
        fs::remove_file(&registry).unwrap();
        let closed = claude.scan();
        assert_eq!(closed.len(), 1);
        assert!(!closed[0].live);
        assert_eq!(closed[0].updated_at, 1_794_714_499_364);

        let _ = fs::remove_dir_all(&dir);
    }
}
