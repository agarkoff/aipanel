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

use aipanel_proto::{AgentMsg, Command, Item, Limits, ModelInfo, Prompt, SessionInfo, TurnUsage};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc::UnboundedSender, oneshot, Notify},
};

use crate::{
    config::ClaudeCfg,
    handoff,
    util::{basename, create_diff, edit_diff, meter, now_ms, parse_iso_ms, to_ms, tool_text, truncate, DIFF_LIMIT},
};

const TAIL_META_BYTES: u64 = 256 * 1024;
const HEAD_META_BYTES: u64 = 64 * 1024;
const HISTORY_BYTES: u64 = 4 * 1024 * 1024;
/// Для выжимки достаточно хвоста транскрипта: он содержит последние реплики.
const HANDOFF_BYTES: u64 = 2 * 1024 * 1024;
/// Сколько байт транскрипта читается за одну догрузку ранних сообщений и предел
/// на один запрос. Строки транскрипта бывают огромными (результаты вызовов),
/// поэтому читаем, пока не наберётся хоть один целый элемент.
const OLDER_BYTES: u64 = 1024 * 1024;
const OLDER_MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CHUNK: u64 = 32 * 1024 * 1024;
/// Как далеко назад от конца транскрипта искать запрос, с которого начался ход,
/// и каким куском читать. Ход длиннее предела считается неизвестным.
const TURN_MAX_BYTES: u64 = 32 * 1024 * 1024;
const TURN_CHUNK: u64 = 1024 * 1024;
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
/// Сколько ждать, пока закрываемая перед удалением сессия завершится.
pub(crate) const CLOSE_WAIT: Duration = Duration::from_secs(5);
/// Предел командной строки Windows — 32 767 символов; запас оставлен под служебные аргументы.
pub const MAX_ARG_UTF16: usize = 12_000;

/// Если агент запущен из сессии Claude Code, её переменные пометили бы дочерние
/// процессы как вложенные: такие процессы не сохраняют транскрипт.
pub fn scrub_env() {
    for var in NESTED_SESSION_ENV {
        std::env::remove_var(var);
    }
}

/// Ключ каталога в настройках Claude Code: путь через прямые слэши и без слэша
/// в конце — так его пишет сам Claude Code.
fn project_key(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    let trimmed = slashed.trim_end_matches('/');
    // Корень диска Claude записывает со слэшем: «D:/».
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        format!("{trimmed}/")
    } else {
        trimmed.to_owned()
    }
}

#[derive(Clone)]
struct Known {
    pid: Option<u32>,
    cwd: String,
    path: Option<PathBuf>,
    /// Статус из реестра открытых сессий (`waiting` — окно ждёт выбора);
    /// у закрытых пусто.
    status: String,
}

#[derive(Clone, Default)]
struct TailMeta {
    title: Option<String>,
    cwd: Option<String>,
    /// Время последней записи транскрипта (мс от эпохи), а не mtime файла:
    /// живой CLI «касается» файла раз в час, даже когда новых сообщений нет.
    updated: i64,
    /// Модель последнего ответа ассистента.
    model: Option<String>,
}

struct TurnCache {
    /// Смещение строки с запросом пользователя, с которого начался ход.
    at: Option<u64>,
    mtime: SystemTime,
    usage: Option<TurnUsage>,
}

#[derive(Default)]
struct St {
    known: HashMap<String, Known>,
    tails: HashMap<PathBuf, (SystemTime, TailMeta)>,
    /// Токены текущего хода и смещение запроса, с которого он начался.
    turns: HashMap<PathBuf, TurnCache>,
    transcripts: HashMap<String, PathBuf>,
    /// Начало показанной части транскрипта: от него назад читается догрузка
    /// ранних сообщений. Ноль — показан весь транскрипт.
    heads: HashMap<String, u64>,
    listed_at: Option<Instant>,
    /// Вопросы `AskUserQuestion`, показанные панели формой: по ним вводится ответ.
    asks: HashMap<String, crate::ask::Ask>,
    headless: HashMap<String, oneshot::Sender<()>>,
    launched: HashMap<String, Instant>,
}

pub struct Claude {
    cfg: ClaudeCfg,
    home: PathBuf,
    rescan: Arc<Notify>,
    /// Запросы остатка лимитов на api.anthropic.com (не к локальным файлам).
    http: reqwest::Client,
    st: Mutex<St>,
}

impl Claude {
    pub fn new(cfg: ClaudeCfg, rescan: Arc<Notify>) -> Self {
        let home = cfg.home.clone().unwrap_or_else(|| crate::util::home_dir().join(".claude"));
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Self { cfg, home, rescan, http, st: Mutex::new(St::default()) }
    }

    /// Остаток лимитов подписки Claude. Проценты приходят как «использовано»,
    /// в панель уходит противоположное — сколько ещё доступно.
    pub async fn limits(&self) -> Option<Limits> {
        let text = fs::read_to_string(self.home.join(".credentials.json")).ok()?;
        let cred: Value = serde_json::from_str(&text).ok()?;
        let token = cred["claudeAiOauth"]["accessToken"].as_str().filter(|t| !t.is_empty())?;
        let resp = self
            .http
            .get("https://api.anthropic.com/api/oauth/usage")
            .bearer_auth(token)
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("user-agent", format!("aipanel-agent/{}", env!("CARGO_PKG_VERSION")))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: Value = resp.json().await.ok()?;
        let mut meters = Vec::new();
        // Новый ответ — массив `limits` с процентом использования; старый — поля
        // `five_hour` и `seven_day` с тем же `utilization`.
        if let Some(list) = v["limits"].as_array() {
            for limit in list {
                let label = match limit["kind"].as_str().unwrap_or_default() {
                    "session" => "5 ч".to_owned(),
                    "weekly_all" => "неделя".to_owned(),
                    // Недельный лимит на отдельную модель: у него своё имя.
                    "weekly_scoped" => limit["scope"]["model"]["display_name"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("неделя")
                        .to_owned(),
                    _ => continue,
                };
                if let Some(used) = limit["percent"].as_f64() {
                    meters.push(meter(&label, used, limit["resets_at"].as_str()));
                }
            }
        }
        if meters.is_empty() {
            for (key, label) in [("five_hour", "5 ч"), ("seven_day", "неделя")] {
                if let Some(used) = v[key]["utilization"].as_f64() {
                    meters.push(meter(label, used, v[key]["resets_at"].as_str()));
                }
            }
        }
        if meters.is_empty() {
            return None;
        }
        Some(Limits { tool: "claude".into(), title: "Claude Code".into(), meters, updated_at: now_ms() })
    }

    /// Команды сессии: встроенные команды Claude Code, пользовательские из
    /// `~/.claude/commands` и `<каталог>/.claude/commands` и навыки из `skills`.
    /// Выполнить их из панели можно только в открытой сессии: текст набирается
    /// в её консоли, а `claude -p` слэш-команды не выполняет.
    pub fn commands(&self, sid: &str) -> Vec<Command> {
        let cwd = self
            .lock()
            .known
            .get(sid)
            .map(|k| k.cwd.clone())
            .unwrap_or_default();
        commands_for(&self.home, &cwd)
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
                let status = v["status"].as_str().unwrap_or("idle").to_owned();
                let mut meta = TailMeta::default();
                let mut turn = None;
                if let Some(p) = &transcript {
                    if let Ok(mtime) = fs::metadata(p).and_then(|m| m.modified()) {
                        meta = tail_meta(&mut st, p, mtime);
                        updated = updated.max(meta.updated);
                        used.insert(p.clone());
                        if matches!(status.as_str(), "busy" | "shell" | "stalled") {
                            turn = turn_usage(&mut st, p, mtime);
                        }
                    }
                }
                let cwd = v["cwd"].as_str().unwrap_or_default().to_owned();
                let name = v["name"].as_str().unwrap_or(sid);
                out.push(SessionInfo {
                    key: format!("claude:{sid}"),
                    tool: "claude".into(),
                    title: meta.title.unwrap_or_else(|| name.to_owned()),
                    cwd: cwd.clone(),
                    status: status.clone(),
                    live: true,
                    updated_at: updated,
                    delivery: if cfg!(windows) { "console" } else { "none" }.into(),
                    model: meta.model.unwrap_or_default(),
                    turn,
                });
                known.insert(sid.to_owned(), Known { pid: Some(pid), cwd, path: transcript, status });
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
            let turn = if running { turn_usage(&mut st, &path, mtime) } else { None };
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
                model: meta.model.unwrap_or_default(),
                turn,
            });
            known.insert(sid, Known { pid: None, cwd, path: Some(path), status: String::new() });
        }

        st.tails.retain(|p, _| used.contains(p));
        st.turns.retain(|p, _| used.contains(p));
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
    /// Открытую или выполняющуюся сессию без `close` не трогаем: Claude Code
    /// дописывал бы в удалённый файл. С `close` сначала завершаем её процесс.
    pub async fn delete(&self, sid: &str, close: bool) -> Result<(), String> {
        if !close {
            return self.remove(sid);
        }
        self.close(sid).await?;
        // Убитый процесс отпускает транскрипт не сразу, а Windows не удаляет
        // открытый файл: несколько секунд повторяем попытку.
        let deadline = Instant::now() + CLOSE_WAIT;
        loop {
            match self.remove(sid) {
                Err(e) if Instant::now() < deadline && e.starts_with("не удалось удалить") => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                result => return result,
            }
        }
    }

    /// Завершает сессию, открытую в терминале или выполняемую фоновым процессом,
    /// и ждёт, пока процесс завершится.
    pub(crate) async fn close(&self, sid: &str) -> Result<(), String> {
        let pid = self.lock().known.get(sid).and_then(|k| k.pid);
        if let Some(pid) = pid {
            crate::platform::kill_pid(pid);
            let deadline = Instant::now() + CLOSE_WAIT;
            while crate::platform::pid_alive(pid) {
                if Instant::now() >= deadline {
                    return Err("не удалось закрыть сессию в терминале".into());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if let Some(k) = self.lock().known.get_mut(sid) {
                k.pid = None;
            }
        }
        if let Some(kill) = self.lock().headless.remove(sid) {
            let _ = kill.send(());
        }
        self.rescan.notify_one();
        Ok(())
    }

    fn remove(&self, sid: &str) -> Result<(), String> {
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
        st.heads.remove(sid);
        Ok(())
    }

    /// Следит за транскриптом и отправляет ленту, пока задачу не отменят.
    pub async fn watch(&self, key: String, sid: String, out: UnboundedSender<AgentMsg>) {
        let mut path: Option<PathBuf> = None;
        let mut offset = 0u64;
        let mut first = true;
        let mut told_empty = false;
        // Вопрос с вариантами, который ждёт ответа, и показанные панели формы:
        // форма уходит, только когда список меняется.
        let mut ask = crate::ask::Tracker::default();
        let mut shown: Vec<Prompt> = Vec::new();
        loop {
            if path.is_none() {
                path = self.find_path(&sid);
            }
            match path.as_deref().map(|p| (p, fs::metadata(p))) {
                Some((p, Ok(md))) => {
                    let len = md.len();
                    if first || len < offset {
                        let start = len.saturating_sub(HISTORY_BYTES);
                        let (items, consumed, begin) = read_items(p, start, len, start > 0, HISTORY_ITEMS);
                        // С конца показанной части пойдёт догрузка ранних сообщений.
                        self.lock().heads.insert(sid.clone(), begin);
                        offset = start + consumed;
                        ask = crate::ask::Tracker::default();
                        scan_ask(p, start, offset, start > 0, &mut ask);
                        first = false;
                        let _ = out.send(AgentMsg::History {
                            session: key.clone(),
                            reset: true,
                            items,
                            drop: Vec::new(),
                            // Перед показанным остался ещё не прочитанный транскрипт.
                            more: begin > 0,
                        });
                    } else if len > offset {
                        let (items, consumed, _) =
                            read_items(p, offset, len.min(offset + MAX_CHUNK), false, usize::MAX);
                        scan_ask(p, offset, offset + consumed, false, &mut ask);
                        offset += consumed;
                        if !items.is_empty() {
                            let _ = out.send(AgentMsg::History {
                                session: key.clone(),
                                reset: false,
                                items,
                                drop: Vec::new(),
                                more: false,
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
                            more: false,
                        });
                    }
                }
            }
            let prompts = self.ask_prompts(&sid, ask.pending.as_ref());
            if prompts != shown {
                shown = prompts;
                let _ = out.send(AgentMsg::Prompts { session: key.clone(), prompts: shown.clone() });
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
    }

    /// Страница ранних элементов транскрипта — то, что было до показанной ленты.
    /// `more` — остались ли ещё более старые.
    pub fn older(&self, sid: &str) -> Result<(Vec<Item>, bool), String> {
        let known = self.lock().known.get(sid).and_then(|k| k.path.clone());
        let path = known.or_else(|| self.find_path(sid)).ok_or("транскрипт сессии не найден")?;
        let mut left = self.lock().heads.get(sid).copied().unwrap_or(0);
        if left == 0 {
            return Ok((Vec::new(), false));
        }
        let mut items = Vec::new();
        let mut spent = 0u64;
        while left > 0 && items.is_empty() && spent < OLDER_MAX_BYTES {
            let from = left.saturating_sub(OLDER_BYTES);
            let (page, _, begin) = read_items(&path, from, left, from > 0, usize::MAX);
            spent += left - from;
            // Если целых строк в куске нет (одна огромная строка), шагаем дальше
            // от прочитанного: иначе следующий запрос вернулся бы на то же место.
            left = if page.is_empty() { from } else { begin };
            items = page;
        }
        self.lock().heads.insert(sid.to_owned(), left);
        Ok((items, left > 0))
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
        let (items, _, _) = read_items(&path, start, len, start > 0, usize::MAX);
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
    pub fn start(&self, cwd: &str, model: &str) -> Result<Option<String>, String> {
        self.start_with_prompt(cwd, None, model)
    }

    /// Файл настроек Claude Code (`~/.claude.json`) лежит рядом с каталогом данных.
    fn config_path(&self) -> PathBuf {
        self.home.with_file_name(".claude.json")
    }

    /// Отмечает каталог доверенным. Панель открывает интерактивное окно, а Claude Code
    /// в незнакомом каталоге сначала спрашивает, доверять ли ему, и до ответа сессию
    /// не заводит: из панели такая сессия не видна, окно же стоит на вопросе.
    fn trust_dir(&self, cwd: &str) -> Result<(), String> {
        let path = self.config_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            // Файла нет — Claude Code заведёт его сам; каталог без записи он не спрашивает.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut cfg: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let Some(projects) = cfg.get_mut("projects").and_then(Value::as_object_mut) else {
            return Ok(());
        };
        let key = project_key(cwd);
        // Запись могла быть сделана другим регистром или разделителями — правим её же.
        let found = projects.keys().find(|k| project_key(k).to_lowercase() == key.to_lowercase()).cloned();
        let entry = projects.entry(found.unwrap_or(key)).or_insert_with(|| Value::Object(Default::default()));
        let Some(entry) = entry.as_object_mut() else {
            return Ok(());
        };
        if entry.get("hasTrustDialogAccepted").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        entry.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
        let out = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
        // Через временный файл: у Claude Code открыто несколько окон, и он перезаписывает
        // эти настройки сам — обрывок файла сломал бы ему запуск.
        let tmp = path.with_extension("aipanel-tmp");
        fs::write(&tmp, out).map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Открывает новую сессию; `prompt` уходит её первым сообщением, `model` —
    /// алиас или полное имя для `--model` (пусто — модель по умолчанию).
    pub fn start_with_prompt(&self, cwd: &str, prompt: Option<&str>, model: &str) -> Result<Option<String>, String> {
        let dir = crate::util::session_dir(cwd)?;
        if self.cfg.trust_dirs {
            self.trust_dir(&dir.to_string_lossy())?;
        }
        let mut args = Vec::new();
        if let Some(model) = model_arg(model)? {
            args.push("--model".into());
            args.push(model);
        }
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

    /// Вопрос для панели: только пока окно сессии открыто и ждёт выбора. Вызов
    /// без результата в транскрипте закрытой сессии ответа уже не получит.
    fn ask_prompts(&self, sid: &str, pending: Option<&crate::ask::Ask>) -> Vec<Prompt> {
        let mut st = self.lock();
        let waiting = st.known.get(sid).is_some_and(|k| k.pid.is_some() && k.status == "waiting");
        match pending.filter(|_| waiting && cfg!(windows)) {
            Some(ask) => {
                st.asks.insert(sid.to_owned(), ask.clone());
                vec![crate::ask::prompt(ask)]
            }
            None => {
                st.asks.remove(sid);
                Vec::new()
            }
        }
    }

    /// Ответ на вопрос `prompt` из панели: ввод в меню окна нажатиями клавиш.
    /// `cancel` — закрыть меню (Esc), Claude получит отказ отвечать.
    pub async fn reply_prompt(&self, sid: &str, prompt: &str, answer: &Value, cancel: bool) -> Result<(), String> {
        let (ask, pid) = {
            let st = self.lock();
            let ask = st.asks.get(sid).filter(|a| a.id == prompt).cloned();
            let pid = st.known.get(sid).filter(|k| k.status == "waiting").and_then(|k| k.pid);
            (ask, pid)
        };
        let (Some(ask), Some(pid)) = (ask, pid) else {
            return Err("вопрос уже закрыт".into());
        };
        let keys = if cancel { vec![crate::platform::Key::Esc] } else { crate::ask::keys(&ask, answer)? };
        // Повторный ответ на тот же вопрос ушёл бы в следующее меню или в поле ввода.
        self.lock().asks.remove(sid);
        crate::platform::console_keys(pid, &keys).await
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
                    more: false,
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

/// Модели для нового окна Claude Code. CLI принимает алиасы (`/model` и
/// `--model`), а полные имена зависят от подписки и версии — поэтому списком
/// идут стабильные алиасы.
pub fn launch_models() -> Vec<ModelInfo> {
    const MODELS: &[(&str, &str)] = &[
        ("fable", "Fable"),
        ("opus", "Opus — самая мощная"),
        ("sonnet", "Sonnet — на каждый день"),
        ("haiku", "Haiku — быстрая"),
    ];
    MODELS
        .iter()
        .map(|(id, name)| ModelInfo { provider: "claude".into(), id: (*id).into(), name: (*name).into() })
        .collect()
}

/// Проверяет модель для `--model`: пусто — флага нет, иначе значение не должно
/// начинаться с «-» или содержать пробелы, иначе оно станет ещё одним параметром.
fn model_arg(model: &str) -> Result<Option<String>, String> {
    let model = model.trim();
    if model.is_empty() {
        return Ok(None);
    }
    let ok = model.len() <= 100
        && !model.starts_with('-')
        && model
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '[' | ']'));
    if !ok {
        return Err(format!("некорректная модель: {model}"));
    }
    Ok(Some(model.to_owned()))
}

/// Встроенные команды Claude Code: собственных файлов у них нет, а меню TUI из
/// панели не прочитать, поэтому список хранится здесь. Он снят с CLI 2.1;
/// на диске рядом с ним ищутся пользовательские команды и навыки, так что
/// новинки CLI попадают в панель только после обновления этого списка.
const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("add-dir", "Add a new working directory"),
    ("advisor", "Let Claude consult a stronger model at key moments"),
    ("autocompact", "Set how full the context gets before auto-summarizing"),
    ("branch", "Create a branch of the current conversation at this point"),
    ("brief", "Toggle brief-only mode"),
    ("btw", "Ask a quick side question without interrupting the main conversation"),
    ("bug", "Report a bug or share your conversation"),
    ("cd", "Move this session to a new working directory"),
    ("clear", "Start a new session with empty context; the previous one stays on disk"),
    ("compact", "Free up context by summarizing the conversation so far"),
    ("config", "Open settings"),
    ("context", "Visualize current context usage as a colored grid"),
    ("copy", "Copy Claude's last response to clipboard"),
    ("doctor", "Health-check your setup and fix issues"),
    ("effort", "Set effort level for model usage"),
    ("export", "Export the current conversation to a file or clipboard"),
    ("feedback", "Send feedback to Anthropic or report a bug"),
    ("focus", "Toggle focus view: just your prompt, summary, and response"),
    ("fork", "Copy this conversation into a new background session"),
    ("goal", "Set a goal Claude checks before stopping"),
    ("help", "Show help and available commands"),
    ("hooks", "View hook configurations for tool events"),
    ("ide", "Manage IDE integrations and show status"),
    ("import", "Import config from another AI coding agent"),
    ("init", "Initialize a new CLAUDE.md file with codebase documentation"),
    ("login", "Sign in with your Anthropic account"),
    ("logout", "Sign out from your Anthropic account"),
    ("mcp", "Manage MCP servers"),
    ("memory", "Edit CLAUDE.md files and memory settings"),
    ("model", "Set model for new sessions from this window"),
    ("output-style", "List output styles or switch to one"),
    ("permissions", "Manage allow and deny tool permission rules"),
    ("plan", "Enable plan mode or view the current session plan"),
    ("privacy-settings", "View and update your privacy settings"),
    ("recap", "Generate a one-line session recap now"),
    ("release-notes", "Show what's new in Claude Code"),
    ("reload-plugins", "Activate pending plugin changes in the current session"),
    ("reload-skills", "Pick up skills added or changed on disk during this session"),
    ("rename", "Rename the current conversation"),
    ("resume", "Resume a previous conversation"),
    ("rewind", "Rewind the conversation or restore files to an earlier point"),
    ("sandbox", "Show or change the sandbox settings"),
    ("skills", "List available skills"),
    ("status", "Show Claude Code status: version, model, account, API connectivity"),
    ("statusline", "Set up the status line UI"),
    ("stop", "Stop this background session; transcript and worktree are kept"),
    ("tasks", "View and manage everything running in the background"),
    ("terminal-setup", "Set up the terminal for Claude Code"),
    ("theme", "Change the theme"),
    ("tui", "Set the terminal UI renderer (default | fullscreen)"),
    ("upgrade", "Upgrade your plan for higher rate limits and more Opus"),
    ("usage", "Show session cost, plan usage, and activity stats"),
    ("version", "Print the version this session is running"),
];

/// Команды для сессии в каталоге `cwd`: встроенные, пользовательские из
/// `~/.claude/commands` и `<cwd>/.claude/commands`, затем навыки из `skills`.
fn commands_for(home: &Path, cwd: &str) -> Vec<Command> {
    let mut out: Vec<Command> = BUILTIN_COMMANDS
        .iter()
        .map(|(name, description)| Command { name: (*name).into(), description: (*description).into() })
        .collect();
    let mut roots = vec![home.join("commands")];
    let mut skills = vec![home.join("skills")];
    if !cwd.is_empty() {
        let project = Path::new(cwd).join(".claude");
        roots.push(project.join("commands"));
        skills.push(project.join("skills"));
    }
    for root in roots {
        collect_commands(&root, &root, 0, &mut out);
    }
    for root in skills {
        collect_skills(&root, &mut out);
    }
    // Одноимённое определение из каталога проекта важнее пользовательского,
    // а пользовательское — встроенного: оставляем последнее.
    let mut seen = HashSet::new();
    let mut unique: Vec<Command> = Vec::new();
    for command in out.into_iter().rev() {
        if seen.insert(command.name.clone()) {
            unique.push(command);
        }
    }
    unique.reverse();
    unique.sort_by(|a, b| a.name.cmp(&b.name));
    unique
}

/// Файлы команд: `<корень>/<путь>.md`. Вложенные каталоги дают имя через `:`
/// (`frontend/component.md` — команда `frontend:component`), как в Claude Code.
fn collect_commands(root: &Path, dir: &Path, depth: usize, out: &mut Vec<Command>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_commands(root, &path, depth + 1, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else { continue };
        let name = relative
            .with_extension("")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":");
        if name.is_empty() {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        out.push(Command { name, description: described(&text) });
    }
}

/// Навыки: `<корень>/<имя>/SKILL.md`; вызываются в сессии как `/имя`.
fn collect_skills(root: &Path, out: &mut Vec<Command>) {
    let Ok(entries) = fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let Ok(text) = fs::read_to_string(entry.path().join("SKILL.md")) else { continue };
        let name = front_matter(&text, "name")
            .or_else(|| entry.file_name().to_str().map(str::to_owned))
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        out.push(Command { name, description: described(&text) });
    }
}

/// Описание из YAML-заголовка файла: одной строкой и умеренной длины, чтобы
/// список в панели не раздувался.
fn described(text: &str) -> String {
    let line = front_matter(text, "description").unwrap_or_default();
    line.split('\n').next().unwrap_or("").trim().chars().take(200).collect()
}

/// Поле простого вида `ключ: значение` из YAML-заголовка между `---`.
fn front_matter(text: &str, key: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((name, value)) = line.split_once(':') else { continue };
        if name.trim() != key {
            continue;
        }
        let value = value.trim().trim_matches(['"', '\'']);
        // Многострочные значения (`description: |`) не разбираем: в описании
        // списка они превратились бы в символ «|».
        if !value.is_empty() && value != "|" && value != ">" && !value.starts_with("|") && !value.starts_with('>') {
            return Some(value.to_owned());
        }
    }
    None
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
fn turn_usage(st: &mut St, path: &Path, mtime: SystemTime) -> Option<TurnUsage> {
    if let Some(c) = st.turns.get(path).filter(|c| c.mtime == mtime) {
        return c.usage.clone();
    }
    let len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let prev = st.turns.get(path).and_then(|c| c.at);
    let (at, usage) = read_turn(path, len, prev);
    st.turns.insert(path.to_owned(), TurnCache { at, mtime, usage: usage.clone() });
    usage
}

fn read_turn(path: &Path, len: u64, prev: Option<u64>) -> (Option<u64>, Option<TurnUsage>) {
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

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Строка транскрипта, с которой начинается ход: запрос самого пользователя, а не
/// результат вызова инструмента, служебная вставка или запись подагента.
fn is_prompt(line: &[u8]) -> bool {
    has(line, b"\"type\":\"user\"")
        && !has(line, b"\"type\":\"tool_result\"")
        && !has(line, b"\"isMeta\":true")
        && !has(line, b"\"isSidechain\":true")
        && !has(line, b"\"isCompactSummary\":true")
}

/// Смещение последнего запроса пользователя; ищет назад от конца файла.
fn find_prompt(path: &Path, len: u64) -> Option<u64> {
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
fn sum_turn(data: &[u8], base: u64) -> Option<(u64, TurnUsage)> {
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

/// Модель ответа ассистента: `"type":"assistant"` и `"model":"claude-…"` внутри `message`.
/// Ищется по подстроке, чтобы не разбирать длинные строки с ответами целиком.
/// Служебные ответы самого CLI (`<synthetic>`) моделью не считаются.
fn line_model(line: &str) -> Option<String> {
    const KEY: &str = "\"model\":\"";
    if !line.contains("\"type\":\"assistant\"") {
        return None;
    }
    let rest = &line[line.find(KEY)? + KEY.len()..];
    let model = &rest[..rest.find('"')?];
    (!model.is_empty() && !model.starts_with('<')).then(|| model.to_owned())
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

/// Читает `[start, end)` и разбирает только завершённые строки. Возвращает
/// элементы, число обработанных байт и смещение в файле, с которого начинаются
/// оставленные элементы (конец прочитанного, если оставлено пусто).
///
/// `max_items` ограничивает размер ленты, которую панель получает сразу; обрезка
/// идёт по строкам, чтобы смещение начала оставшегося было точным — с него
/// продолжится догрузка ранних сообщений.
fn read_items(path: &Path, start: u64, end: u64, skip_first: bool, max_items: usize) -> (Vec<Item>, u64, u64) {
    let buf = read_range(path, start, end);
    let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
        return (Vec::new(), 0, start);
    };
    let mut lines: Vec<(u64, Vec<Item>)> = Vec::new();
    let mut at = 0usize;
    for (i, line) in buf[..last_nl].split(|b| *b == b'\n').enumerate() {
        let from = at;
        at += line.len() + 1;
        if i == 0 && skip_first {
            continue;
        }
        let mut items = Vec::new();
        if let Ok(line) = std::str::from_utf8(line) {
            parse_line(line, &mut items);
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
    (lines.into_iter().flat_map(|(_, items)| items).collect(), consumed, begin)
}

/// Запрос пользователя, который на деле прислал сам Claude Code: у таких строк
/// `origin.kind` не `human` (`task-notification`). В старых транскриптах поля
/// нет — тогда узнаём уведомление по тегу в начале текста.
fn is_notice(v: &Value, text: &str) -> bool {
    match v["origin"]["kind"].as_str() {
        Some(kind) => kind != "human",
        None => text.trim_start().starts_with("<task-notification>"),
    }
}

/// Прогоняет завершённые строки `[start, end)` через слежение за вопросом
/// (см. [`crate::ask::Tracker`]). Кусок тот же, что только что ушёл в ленту:
/// новые строки короткие, а большой первый кусок читается один раз.
fn scan_ask(path: &Path, start: u64, end: u64, skip_first: bool, ask: &mut crate::ask::Tracker) {
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
fn parse_queued(line: &str, out: &mut Vec<Item>) {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    if v["type"].as_str() != Some("attachment") {
        return;
    }
    let a = &v["attachment"];
    // Уведомления фоновых задач приходят так же, но это не сообщение человека.
    if a["type"].as_str() != Some("queued_command") || a["origin"]["kind"].as_str() != Some("human") {
        return;
    }
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
        role: "user".into(),
        kind: "text".into(),
        text: truncate(text, TEXT_LIMIT),
        tool: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt_line(ts: &str) -> String {
        format!(r#"{{"type":"user","timestamp":"{ts}","message":{{"role":"user","content":"привет"}}}}"#)
    }

    fn reply_line(id: &str, input: u64, cache: u64, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"id":"{id}","content":[],"usage":{{"input_tokens":{input},"cache_creation_input_tokens":0,"cache_read_input_tokens":{cache},"output_tokens":{output}}}}}}}"#
        )
    }

    const TOOL_RESULT: &str =
        r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"t1","type":"tool_result","content":"ok"}]}}"#;

    #[test]
    fn turn_counts_each_reply_once() {
        let text = [
            prompt_line("2026-11-15T03:48:19.364Z"),
            // Один ответ записан тремя строками с одним usage.
            reply_line("msg_1", 2, 1000, 300),
            reply_line("msg_1", 2, 1000, 300),
            TOOL_RESULT.to_owned(),
            reply_line("msg_2", 3, 1500, 120),
            String::new(),
        ]
        .join("\n");
        let (at, turn) = sum_turn(text.as_bytes(), 40).unwrap();
        assert_eq!(at, 40);
        assert_eq!(turn.started_at, parse_iso_ms("2026-11-15T03:48:19.364Z"));
        assert_eq!(turn.input, 1503);
        assert_eq!(turn.output, 420);
    }

    #[test]
    fn turn_restarts_on_new_prompt_and_skips_partial_line() {
        let first = [prompt_line("2026-11-15T03:00:00.000Z"), reply_line("msg_1", 1, 10, 50)].join("\n");
        let second = prompt_line("2026-11-15T03:10:00.000Z");
        let partial = reply_line("msg_2", 1, 10, 70);
        let text = format!("{first}\n{second}\n{}", &partial[..partial.len() / 2]);
        let (at, turn) = sum_turn(text.as_bytes(), 0).unwrap();
        assert_eq!(at, (first.len() + 1) as u64);
        assert_eq!(turn.started_at, parse_iso_ms("2026-11-15T03:10:00.000Z"));
        // У нового хода записанных ответов ещё нет: не переносим цифры прошлого.
        assert_eq!((turn.input, turn.output), (0, 0));
    }

    #[test]
    fn turn_must_start_with_prompt() {
        let text = [TOOL_RESULT.to_owned(), reply_line("msg_1", 1, 1, 1), String::new()].join("\n");
        assert!(sum_turn(text.as_bytes(), 0).is_none());
        assert!(!is_prompt(TOOL_RESULT.as_bytes()));
        let meta = r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"x"}}"#;
        assert!(!is_prompt(meta.as_bytes()));
    }

    #[test]
    fn finds_turn_on_disk_and_reuses_offset() {
        let dir = std::env::temp_dir().join(format!("aipanel-turn-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let old = [prompt_line("2026-11-15T01:00:00.000Z"), reply_line("msg_0", 1, 1, 9)].join("\n");
        let cur = [prompt_line("2026-11-15T02:00:00.000Z"), reply_line("msg_1", 2, 500, 40)].join("\n");
        fs::write(&path, format!("{old}\n{cur}\n")).unwrap();
        let len = fs::metadata(&path).unwrap().len();
        let (at, turn) = read_turn(&path, len, None);
        assert_eq!(at, Some((old.len() + 1) as u64));
        let turn = turn.unwrap();
        assert_eq!((turn.input, turn.output), (502, 40));
        // Дописанный ответ: тот же запрос, смещение берётся из кеша.
        fs::write(&path, format!("{old}\n{cur}\n{}\n", reply_line("msg_2", 2, 600, 15))).unwrap();
        let len = fs::metadata(&path).unwrap().len();
        let (at2, turn2) = read_turn(&path, len, at);
        assert_eq!(at2, at);
        let turn2 = turn2.unwrap();
        assert_eq!((turn2.input, turn2.output), (602, 55));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn launch_models_are_aliases() {
        let list = launch_models();
        assert!(list.iter().any(|m| m.id == "sonnet" && m.provider == "claude"));
        assert!(model_arg("").unwrap().is_none());
        assert_eq!(model_arg(" sonnet ").unwrap().as_deref(), Some("sonnet"));
        assert_eq!(model_arg("sonnet[1m]").unwrap().as_deref(), Some("sonnet[1m]"));
        assert!(model_arg("--help").is_err());
        assert!(model_arg("two words").is_err());
    }

    #[test]
    fn reads_front_matter() {
        let text = "---\nname: review\ndescription: \"Смотрит дифф\"\nargument-hint: [файл]\n---\n\nТело";
        assert_eq!(front_matter(text, "description").as_deref(), Some("Смотрит дифф"));
        assert_eq!(front_matter(text, "name").as_deref(), Some("review"));
        assert_eq!(front_matter(text, "missing"), None);
        assert_eq!(front_matter("без заголовка", "name"), None);
        assert_eq!(described("---\ndescription: |\n  строка\n---"), "");
    }

    #[test]
    fn collects_commands_and_skills() {
        let root = std::env::temp_dir().join(format!("aipanel-commands-{}", now_ms()));
        let home = root.join("home");
        let project = root.join("project");
        let write = |path: &Path, text: &str| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write(&home.join("commands/review.md"), "---\ndescription: Своя команда\n---\n");
        write(&home.join("commands/front/review.md"), "---\ndescription: Вложенная\n---\n");
        write(&project.join(".claude/commands/review.md"), "---\ndescription: Команда проекта\n---\n");
        write(&project.join(".claude/commands/only-project.md"), "");
        write(&home.join("skills/human-text/SKILL.md"), "---\nname: human-text\ndescription: Текст\n---\n");
        write(&home.join("skills/no-front/SKILL.md"), "---\n---\n");
        write(&home.join("skills/broken/SKILL.md"), "без заголовка");

        let list = commands_for(&home, project.to_str().unwrap());
        let find = |name: &str| list.iter().find(|c| c.name == name);
        // Проектная команда вытесняет пользовательскую с тем же именем.
        assert_eq!(find("review").unwrap().description, "Команда проекта");
        assert_eq!(find("front:review").unwrap().description, "Вложенная");
        assert_eq!(find("only-project").unwrap().description, "");
        assert_eq!(find("human-text").unwrap().description, "Текст");
        // Навык без name называется по каталогу; без заголовка тоже попадает.
        assert!(find("no-front").is_some());
        assert!(find("broken").is_some());
        // Встроенные команды на месте, порядок — по имени.
        assert!(find("clear").is_some() && find("compact").is_some());
        let names: Vec<_> = list.iter().map(|c| c.name.clone()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert_eq!(names.iter().filter(|n| *n == "review").count(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_queued_command_as_user_message() {
        let mut items = Vec::new();
        parse_line(
            r#"{"parentUuid":"p","isSidechain":false,"attachment":{"type":"queued_command","prompt":"статус?","source_uuid":"src","commandMode":"prompt","origin":{"kind":"human"},"timestamp":"2026-11-15T10:47:21.483Z","humanTurn":true},"type":"attachment","uuid":"a1","timestamp":"2026-11-15T10:47:21.483Z"}"#,
            &mut items,
        );
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.role.as_str(), i.kind.as_str(), i.text.as_str())).collect();
        assert_eq!(got, [("a1:queued", "user", "text", "статус?")]);
        assert!(items[0].ts > 0);
    }

    #[test]
    fn skips_service_queued_commands() {
        let mut items = Vec::new();
        // Уведомление фоновой задачи — не сообщение пользователя.
        parse_line(
            r#"{"type":"attachment","uuid":"a2","attachment":{"type":"queued_command","prompt":"<task-notification>","origin":{"kind":"task-notification"},"timestamp":"2026-11-15T10:47:21.483Z"}}"#,
            &mut items,
        );
        // Пустой текст и служебная ветка тоже не попадают в ленту.
        parse_line(
            r#"{"type":"attachment","uuid":"a3","attachment":{"type":"queued_command","prompt":"  ","origin":{"kind":"human"}}}"#,
            &mut items,
        );
        parse_line(
            r#"{"type":"attachment","uuid":"a4","isSidechain":true,"attachment":{"type":"queued_command","prompt":"ветка","origin":{"kind":"human"}}}"#,
            &mut items,
        );
        assert!(items.is_empty());
    }

    #[test]
    fn task_notification_is_a_notice() {
        let mut items = Vec::new();
        let note = "<task-notification>\n<status>completed</status>\n<summary>Готово</summary>\n</task-notification>";
        let line = |origin: &str| {
            format!(
                r#"{{"type":"user","uuid":"u",{origin}"message":{{"role":"user","content":{}}}}}"#,
                serde_json::to_string(note).unwrap()
            )
        };
        parse_line(&line(r#""origin":{"kind":"task-notification"},"#), &mut items);
        // Старый транскрипт без `origin` — по тегу.
        parse_line(&line(""), &mut items);
        parse_line(r#"{"type":"user","uuid":"h","origin":{"kind":"human"},"message":{"role":"user","content":"<b>моё</b>"}}"#, &mut items);
        let got: Vec<_> = items.iter().map(|i| (i.role.as_str(), i.kind.as_str())).collect();
        assert_eq!(got, [("system", "notice"), ("system", "notice"), ("user", "text")]);
        assert_eq!(items[0].text, note);
    }

    #[test]
    fn reads_line_model() {
        let line = r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"\"model\":\"x\""}]}}"#;
        assert_eq!(line_model(line).as_deref(), Some("claude-opus-5-5"));
        assert_eq!(line_model(r#"{"type":"assistant","message":{"model":"<synthetic>"}}"#), None);
        assert_eq!(line_model(r#"{"type":"user","message":{"model":"claude-opus-5-5"}}"#), None);
    }

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

    /// Без `close` удаляется только закрытая сессия: открытую в терминале трогать нельзя.
    #[tokio::test]
    async fn deletes_closed_transcript() {
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

        claude.delete(sid, false).await.unwrap();
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
        assert!(claude.delete(sid, false).await.is_err());
        assert!(path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    /// С `close` открытая сессия сначала закрывается: процесс завершается,
    /// затем удаляется транскрипт.
    #[tokio::test]
    async fn closes_open_session_before_delete() {
        let dir = std::env::temp_dir().join(format!("aipanel-claude-close-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let project = dir.join("projects").join("p");
        let sessions = dir.join("sessions");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        let sid = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff";
        let path = project.join(format!("{sid}.jsonl"));
        fs::write(&path, "{}\n").unwrap();

        // Долгий процесс вместо Claude Code в терминале.
        let mut child = if cfg!(windows) {
            std::process::Command::new("ping").args(["-n", "60", "127.0.0.1"]).stdout(Stdio::null()).spawn()
        } else {
            std::process::Command::new("sleep").arg("60").spawn()
        }
        .unwrap();
        let pid = child.id();
        fs::write(
            sessions.join(format!("{pid}.json")),
            format!("{{\"pid\":{pid},\"sessionId\":\"{sid}\",\"cwd\":\"D:/p\",\"name\":\"n\",\"status\":\"idle\"}}"),
        )
        .unwrap();

        let cfg = ClaudeCfg { home: Some(dir.clone()), ..ClaudeCfg::default() };
        let claude = Claude::new(cfg, Arc::new(Notify::new()));
        assert!(claude.scan()[0].live);

        claude.delete(sid, true).await.unwrap();
        assert!(!path.exists());
        assert!(!crate::platform::pid_alive(pid));
        let _ = child.wait();

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

    /// Ключ каталога совпадает с тем, что пишет Claude Code: прямые слэши, без слэша
    /// в конце (у корня диска слэш остаётся).
    #[test]
    fn trust_key_uses_slashes() {
        assert_eq!(project_key(r"D:\Projects\demo"), "D:/Projects/demo");
        assert_eq!(project_key("D:/Projects/demo/"), "D:/Projects/demo");
        assert_eq!(project_key("D:\\"), "D:/");
    }

    /// Каталог отмечается доверенным: без этого окно встаёт на вопросе про доверие
    /// и сессии в панели нет.
    #[test]
    fn marks_project_trusted() {
        let dir = std::env::temp_dir().join(format!("aipanel-claude-trust-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".claude")).unwrap();
        let settings = dir.join(".claude.json");
        // Запись уже есть, но другим регистром и без отметки доверия.
        fs::write(
            &settings,
            r#"{"numStartups":730,"projects":{"D:/projects/demo":{"allowedTools":[]},
               "D:/Projects/Other":{"hasTrustDialogAccepted":true}}}"#,
        )
        .unwrap();

        let cfg = ClaudeCfg { home: Some(dir.join(".claude")), ..ClaudeCfg::default() };
        let claude = Claude::new(cfg, Arc::new(Notify::new()));
        claude.trust_dir(r"D:\Projects\demo").unwrap();

        let saved: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(saved["numStartups"], 730);
        // Вторая запись не появилась: правилась существующая, несмотря на регистр.
        assert_eq!(saved["projects"].as_object().unwrap().len(), 2);
        assert_eq!(saved["projects"]["D:/projects/demo"]["hasTrustDialogAccepted"], true);
        assert_eq!(saved["projects"]["D:/projects/demo"]["allowedTools"], serde_json::json!([]));
        assert!(!dir.join(".claude.aipanel-tmp").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    /// Без файла настроек доверие не трогаем: Claude Code заводит его сам.
    #[test]
    fn skips_trust_without_settings_file() {
        let dir = std::env::temp_dir().join(format!("aipanel-claude-nosettings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".claude")).unwrap();
        let cfg = ClaudeCfg { home: Some(dir.join(".claude")), ..ClaudeCfg::default() };
        let claude = Claude::new(cfg, Arc::new(Notify::new()));
        assert!(claude.trust_dir(r"D:\Projects\demo").is_ok());
        assert!(!dir.join(".claude.json").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Обрезка ленты по числу элементов — построчная: от начала оставшегося
    /// элемента отсчитывается догрузка ранних сообщений, и она не должна ни
    /// потерять строку, ни повторить её.
    #[test]
    fn trims_history_by_lines_and_remembers_head() {
        let dir = std::env::temp_dir().join(format!("aipanel-claude-older-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let line = |uuid: &str, text: &str| {
            format!(
                r#"{{"type":"user","uuid":"{uuid}","timestamp":"2026-11-15T10:00:00.000Z","message":{{"role":"user","content":"{text}"}}}}"#
            )
        };
        let body = format!("{}\n{}\n{}\n", line("u1", "первый"), line("u2", "второй"), line("u3", "третий"));
        fs::write(&path, &body).unwrap();
        let len = body.len() as u64;

        // Оставляем один элемент: начало оставшегося — строка третьего.
        let (items, consumed, begin) = read_items(&path, 0, len, false, 1);
        let third = body.find(&line("u3", "третий")).unwrap() as u64;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "u3:0");
        assert_eq!(consumed, len);
        assert_eq!(begin, third);

        // Назад от начала оставшегося читается остальное и без повторов.
        let (older, _, older_begin) = read_items(&path, 0, begin, false, usize::MAX);
        let ids: Vec<_> = older.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["u1:0", "u2:0"]);
        assert_eq!(older_begin, 0);

        // Кусок без целой строки элементов не даёт и байт не съедает: догрузка
        // продолжит читать с того же места.
        let cut = third + 5;
        let (empty, consumed, begin) = read_items(&path, cut, len - 1, false, usize::MAX);
        assert!(empty.is_empty());
        assert_eq!((consumed, begin), (0, cut));
        let _ = fs::remove_dir_all(&dir);
    }
}
