//! Сессии Claude Code.
//!
//! Открытые сессии берутся из реестра `~/.claude/sessions/<pid>.json`, лента — из
//! транскрипта `~/.claude/projects/<проект>/<sessionId>.jsonl`. В открытую сессию
//! текст вводится через её консоль, для закрытой открывается новое окно консоли
//! `claude --resume <id> -- <текст>` (вне Windows — фоновый `claude -p --resume`).

mod transcript;

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime},
};

use aipanel_proto::{
    AgentMsg, BgTask, Command, Item, Limits, ModelInfo, Prompt, SessionInfo, Subagent, MAX_SUBAGENTS, MAX_TITLE,
};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc::UnboundedSender, oneshot, Notify},
};

use crate::{
    backend::{Backend, Fut},
    config::ClaudeCfg,
    handoff,
    util::{basename, meter, now_ms, to_ms, truncate},
};

use transcript::{
    last_update, read_feed, read_items, scan_ask, scan_tasks, sub_turn, tail_meta, turn_usage, ParseOpts, Queue,
    SubTurn, TailMeta, TurnCache,
};

const TAIL_META_BYTES: u64 = 256 * 1024;
const HEAD_META_BYTES: u64 = 64 * 1024;
const HISTORY_BYTES: u64 = 4 * 1024 * 1024;
/// Сколько конца транскрипта просматривается в поисках запущенных фоновых задач.
const TASKS_BYTES: u64 = 64 * 1024 * 1024;
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

/// Подагент сессии: что уходит в панель и каким вызовом он запущен.
#[derive(Clone)]
struct SubInfo {
    info: Subagent,
    /// id вызова инструмента (`toolu_…`): по нему карточка вызова в ленте и её
    /// итог получают ссылку на ленту подагента.
    tool_id: String,
}

#[derive(Default)]
struct St {
    known: HashMap<String, Known>,
    tails: HashMap<PathBuf, (SystemTime, TailMeta)>,
    /// Токены текущего хода и смещение запроса, с которого он начался.
    turns: HashMap<PathBuf, TurnCache>,
    /// Токены подагентов: файл транскрипта -> накопленное по уже прочитанным строкам.
    sub_turns: HashMap<PathBuf, SubTurn>,
    transcripts: HashMap<String, PathBuf>,
    /// Начало показанной части транскрипта: от него назад читается догрузка
    /// ранних сообщений. Ноль — показан весь транскрипт.
    heads: HashMap<String, u64>,
    /// Метаданные подагентов: файл -> (время изменения, разобранная запись).
    /// Наблюдение за сессией спрашивает список часто, а читать десятки файлов
    /// каждый раз незачем: Claude Code дописывает их только при запуске и конце
    /// подагента, и это видно по mtime.
    sub_metas: HashMap<PathBuf, (SystemTime, SubInfo)>,
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
            // 429 — частый ответ этого эндпоинта; 401 — токен истёк, а Claude
            // Code давно не запускали и не обновил его.
            crate::util::log(&format!("лимиты Claude: HTTP {}", resp.status().as_u16()));
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

    /// Работает ли процесс Claude Code этой сессии: открытое окно или
    /// запуск без окна, начатый агентом.
    fn is_running(&self, sid: &str) -> bool {
        let st = self.lock();
        st.known.get(sid).is_some_and(|k| k.pid.is_some())
            || st.headless.contains_key(sid)
            || st.launched.contains_key(sid)
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
                let cwd = live_cwd(v["cwd"].as_str().unwrap_or_default(), meta.cwd.as_deref());
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
                    repo: None,
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
                repo: None,
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

    /// Каталог подагентов сессии: Claude Code кладёт их рядом с транскриптом,
    /// в `<sessionId>/subagents`.
    fn subagent_dir(&self, sid: &str) -> Option<PathBuf> {
        let path = self.lock().known.get(sid).and_then(|k| k.path.clone()).or_else(|| self.find_path(sid))?;
        Some(path.parent()?.join(sid).join("subagents"))
    }

    /// Транскрипт подагента `sub` сессии.
    fn subagent_path(&self, sid: &str, sub: &str) -> Option<PathBuf> {
        let path = self.subagent_dir(sid)?.join(format!("agent-{sub}.jsonl"));
        path.is_file().then_some(path)
    }

    /// Подагенты сессии по метаданным Claude Code: тип агента, описание задачи и
    /// id вызова лежат в `agent-<id>.meta.json` рядом с его транскриптом. Порядок —
    /// по времени запуска: файл создаётся при запуске подагента.
    fn subagents(&self, sid: &str) -> Vec<SubInfo> {
        let Some(dir) = self.subagent_dir(sid) else { return Vec::new() };
        let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };
        let mut st = self.lock();
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(id) = name.strip_suffix(".meta.json").and_then(|n| n.strip_prefix("agent-")) else {
                continue;
            };
            // Открывать нечего, если транскрипта рядом нет.
            let transcript = dir.join(format!("agent-{id}.jsonl"));
            if !transcript.is_file() {
                continue;
            }
            let path = entry.path();
            let Ok(md) = entry.metadata() else { continue };
            let stamp = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let mut info = match st.sub_metas.get(&path) {
                Some((known, info)) if *known == stamp => info.clone(),
                _ => {
                    let Ok(text) = fs::read_to_string(&path) else { continue };
                    let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                    let started_at = md.created().or_else(|_| md.modified()).map(to_ms).unwrap_or(0);
                    let info = SubInfo {
                        info: Subagent {
                            id: id.to_owned(),
                            title: truncate(v["description"].as_str().unwrap_or_default(), 200),
                            agent: v["agentType"].as_str().unwrap_or_default().to_owned(),
                            running: v["finished"].as_bool() != Some(true),
                            started_at,
                            ended_at: 0,
                            turn: None,
                        },
                        tool_id: v["toolUseId"].as_str().unwrap_or_default().to_owned(),
                    };
                    st.sub_metas.insert(path, (stamp, info.clone()));
                    info
                }
            };
            // Токены и границы меняются чаще метаданных, поэтому считаются не
            // по кешу `sub_metas`, а по своему кешу транскрипта (дописываются
            // только новые строки). Время берём из строк транскрипта: оно не
            // зависит от того, когда файл создали или скопировали.
            let (turn, times) = sub_turn(&mut st, &transcript);
            info.info.turn = turn;
            if times.started_at > 0 {
                info.info.started_at = times.started_at;
            }
            info.info.ended_at = times.ended_at;
            out.push(info);
        }
        out.sort_by_key(|s| s.info.started_at);
        out
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

    /// Переименовывает сессию так же, как это делает её `/rename`: в конец
    /// транскрипта дописывается строка `custom-title`, которую Claude Code и
    /// панель читают как название. Файл остаётся валидным для самого CLI: он
    /// только дописывает строки, а не переписывает транскрипт.
    pub fn rename(&self, sid: &str, title: &str) -> Result<(), String> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > MAX_TITLE {
            return Err(format!("название сессии — от 1 до {MAX_TITLE} символов"));
        }
        if title.chars().any(char::is_control) {
            return Err("в названии есть управляющие символы".into());
        }
        let path = self.find_path(sid).ok_or("транскрипт сессии не найден")?;
        let line = serde_json::json!({
            "type": "custom-title",
            "customTitle": title,
            "sessionId": sid,
        })
        .to_string();
        let mut file = fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("не удалось открыть транскрипт: {e}"))?;
        // Название не должно склеиться с недописанной последней строкой.
        let mut prefix = "";
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len > 0 {
            let mut last = [0u8; 1];
            let read_last = file.seek(SeekFrom::End(-1)).is_ok() && file.read_exact(&mut last).is_ok();
            if read_last && last[0] != b'\n' {
                prefix = "\n";
            }
        }
        file.write_all(format!("{prefix}{line}\n").as_bytes())
            .map_err(|e| format!("не удалось записать название: {e}"))?;
        // Заголовок берётся из хвоста транскрипта, а его разбор кешируется по mtime.
        self.lock().tails.remove(&path);
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
        let sub_dir = path.parent().map(|p| p.join(sid));
        if let Some(dir) = sub_dir.as_ref().filter(|p| p.is_dir()) {
            let _ = fs::remove_dir_all(dir);
        }
        let mut st = self.lock();
        st.known.remove(sid);
        st.tails.remove(&path);
        st.transcripts.remove(sid);
        st.heads.remove(sid);
        if let Some(dir) = sub_dir {
            // Кеши разобранных файлов подагентов удалённой сессии больше не нужны.
            st.sub_metas.retain(|p, _| !p.starts_with(&dir));
            st.sub_turns.retain(|p, _| !p.starts_with(&dir));
        }
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
        // Фоновые задачи: запуск мог быть задолго до показанной части ленты,
        // поэтому при первом чтении транскрипт просматривается глубже.
        let mut tasks = crate::tasks::Tracker::default();
        let mut shown_tasks: Option<Vec<BgTask>> = None;
        let mut queue = Queue::default();
        let mut shown_subs: Option<Vec<Subagent>> = None;
        loop {
            // Список подагентов нужен и до ленты: по нему карточки в ленте
            // показывают статус, а их вызовы получают ссылку на ленту подагента.
            let subs = self.subagents(&sid);
            let sub_of: HashMap<String, String> = subs
                .iter()
                .filter(|s| !s.tool_id.is_empty())
                .map(|s| (s.tool_id.clone(), s.info.id.clone()))
                .collect();
            let mut now_subs: Vec<Subagent> = subs.into_iter().map(|s| s.info).collect();
            // Панели список нужен для статуса карточек: старые вытесняются, но
            // сами карточки в ленте остаются (ссылки на них берутся из sub_of).
            if now_subs.len() > MAX_SUBAGENTS {
                now_subs.drain(..now_subs.len() - MAX_SUBAGENTS);
            }
            if shown_subs.as_ref() != Some(&now_subs) {
                let _ = out.send(AgentMsg::Subagents { session: key.clone(), subagents: now_subs.clone() });
                shown_subs = Some(now_subs);
            }
            if path.is_none() {
                path = self.find_path(&sid);
            }
            match path.as_deref().map(|p| (p, fs::metadata(p))) {
                Some((p, Ok(md))) => {
                    let len = md.len();
                    if first || len < offset {
                        let start = len.saturating_sub(HISTORY_BYTES);
                        queue = Queue::default();
                        let mut opts = ParseOpts {
                            subs: Some(&sub_of),
                            sidechain: false,
                            ..Default::default()
                        };
                        let (items, _, consumed, begin) =
                            read_feed(p, start, len, start > 0, HISTORY_ITEMS, Some(&mut queue), &mut opts);
                        // С конца показанной части пойдёт догрузка ранних сообщений.
                        self.lock().heads.insert(sid.clone(), begin);
                        offset = start + consumed;
                        ask = crate::ask::Tracker::default();
                        scan_ask(p, start, offset, start > 0, &mut ask);
                        tasks = crate::tasks::Tracker::default();
                        let from = offset.saturating_sub(TASKS_BYTES);
                        scan_tasks(p, from, offset, from > 0, &mut tasks);
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
                        let mut opts = ParseOpts {
                            subs: Some(&sub_of),
                            sidechain: false,
                            ..Default::default()
                        };
                        let (items, drop, consumed, _) = read_feed(
                            p,
                            offset,
                            len.min(offset + MAX_CHUNK),
                            false,
                            usize::MAX,
                            Some(&mut queue),
                            &mut opts,
                        );
                        scan_ask(p, offset, offset + consumed, false, &mut ask);
                        scan_tasks(p, offset, offset + consumed, false, &mut tasks);
                        offset += consumed;
                        if !items.is_empty() || !drop.is_empty() {
                            let _ = out.send(AgentMsg::History {
                                session: key.clone(),
                                reset: false,
                                items,
                                drop,
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
            // Задачи живут в процессе Claude Code: закрыта сессия — их нет.
            let now_tasks = if self.is_running(&sid) { tasks.running(now_ms()) } else { Vec::new() };
            if shown_tasks.as_ref() != Some(&now_tasks) {
                let _ = out.send(AgentMsg::Tasks { session: key.clone(), tasks: now_tasks.clone() });
                shown_tasks = Some(now_tasks);
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
    }

    /// Следит за лентой подагента `sub`: она открывается в панели из его
    /// карточки в ленте сессии. Транскрипт подагента — обычный транскрипт Claude
    /// Code, только все его строки помечены `isSidechain`: их разбираем как есть.
    pub async fn watch_sub(&self, key: String, sid: String, sub: String, out: UnboundedSender<AgentMsg>) {
        let mut path: Option<PathBuf> = None;
        let mut offset = 0u64;
        let mut first = true;
        loop {
            if path.is_none() {
                path = self.subagent_path(&sid, &sub);
            }
            match path.as_deref().map(|p| (p, fs::metadata(p))) {
                Some((p, Ok(md))) => {
                    let len = md.len();
                    if first || len < offset {
                        let start = len.saturating_sub(HISTORY_BYTES);
                        let (items, consumed, _) =
                            read_items(p, start, len, start > 0, HISTORY_ITEMS, ParseOpts { sidechain: true, ..Default::default() });
                        offset = start + consumed;
                        first = false;
                        let _ = out.send(AgentMsg::SubHistory {
                            session: key.clone(),
                            sub: sub.clone(),
                            reset: true,
                            items,
                            more: false,
                            error: None,
                        });
                    } else if len > offset {
                        let mut opts = ParseOpts { sidechain: true, ..Default::default() };
                        let (items, _, consumed, _) = read_feed(
                            p,
                            offset,
                            len.min(offset + MAX_CHUNK),
                            false,
                            usize::MAX,
                            None,
                            &mut opts,
                        );
                        offset += consumed;
                        if !items.is_empty() {
                            let _ = out.send(AgentMsg::SubHistory {
                                session: key.clone(),
                                sub: sub.clone(),
                                reset: false,
                                items,
                                more: false,
                                error: None,
                            });
                        }
                    }
                }
                Some((_, Err(_))) => path = None,
                None => {
                    // Файла ещё нет (или его уже удалили): панель показывает
                    // пустую ленту, а наблюдение продолжает искать файл.
                    if first {
                        first = false;
                        let _ = out.send(AgentMsg::SubHistory {
                            session: key.clone(),
                            sub: sub.clone(),
                            reset: true,
                            items: Vec::new(),
                            more: false,
                            error: None,
                        });
                    }
                }
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
        // Карточки подагентов есть и в ранних сообщениях: ссылку на ленту
        // берём из метаданных, а встреченные в куске id запоминаем для итогов.
        let subs = self.subagents(sid);
        let sub_of: HashMap<String, String> = subs
            .iter()
            .filter(|s| !s.tool_id.is_empty())
            .map(|s| (s.tool_id.clone(), s.info.id.clone()))
            .collect();
        let mut items = Vec::new();
        let mut spent = 0u64;
        while left > 0 && items.is_empty() && spent < OLDER_MAX_BYTES {
            let from = left.saturating_sub(OLDER_BYTES);
            let opts = ParseOpts {
                subs: Some(&sub_of),
                sidechain: false,
                ..Default::default()
            };
            let (page, _, begin) = read_items(&path, from, left, from > 0, usize::MAX, opts);
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
            None if cfg!(windows) => {
                // Открытие окна — синхронный запуск процесса (`wt.exe` и сама
                // программа), а он бывает небыстрым: в отдельном потоке задержка
                // не останавливает ни пинги серверу, ни остальные сессии.
                let me = Arc::clone(self);
                let (sid, cwd, text) = (sid.to_owned(), known.cwd.clone(), text.to_owned());
                tokio::task::spawn_blocking(move || me.open_window(&sid, &cwd, &text))
                    .await
                    .map_err(|e| format!("окно сессии не открылось: {e}"))?
            }
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
        let (items, _, _) = read_items(&path, start, len, start > 0, usize::MAX, ParseOpts::default());
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
                        sub: None,
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

/// Каталог, в котором открытая сессия работает сейчас. В файле процесса
/// (`~/.claude/sessions/<pid>.json`) — каталог запуска, а после EnterWorktree
/// сессия работает в worktree, и это знает только транскрипт (последний `cwd`).
/// Тот же каталог показывается в панели и берётся для «Передать сессию»;
/// каталог запуска — только если в транскрипте каталога ещё нет.
fn live_cwd(process: &str, transcript: Option<&str>) -> String {
    transcript.filter(|c| !c.is_empty()).unwrap_or(process).to_owned()
}

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

/// `Backend` для Claude Code: агент не разбирает ключ сессии в каждом
/// обработчике, а зовёт эти методы (см. `crate::backend`).
/// Запуск окна Claude Code: `CreateProcess` (`wt.exe` и сама программа) —
/// синхронный вызов, и он уходит в отдельный поток. Окно открывается не
/// мгновенно, а агент всё это время должен отвечать серверу.
async fn window_start(
    me: &Arc<Claude>,
    cwd: &str,
    prompt: Option<&str>,
    model: &str,
) -> Result<Option<String>, String> {
    let me = Arc::clone(me);
    let (cwd, prompt, model) = (cwd.to_owned(), prompt.map(str::to_owned), model.to_owned());
    tokio::task::spawn_blocking(move || Claude::start_with_prompt(&me, &cwd, prompt.as_deref(), &model))
        .await
        .map_err(|e| format!("окно сессии не открылось: {e}"))?
}

impl Backend for Arc<Claude> {
    fn tool(&self) -> &'static str {
        "claude"
    }

    fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    fn scan(&self) -> Fut<'_, Vec<SessionInfo>> {
        // Обход `~/.claude` — обычное чтение файлов, и на медленном диске оно
        // занимает поток целиком. В отдельной задаче оно не останавливает ни
        // пинги серверу, ни файл состояния: панель просто дольше видит прежний
        // список сессий.
        let me = Arc::clone(self);
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || Claude::scan(&me)).await {
                Ok(list) => list,
                Err(e) => {
                    crate::util::log(&format!("claude: обход сессий прервался: {e}"));
                    Vec::new()
                }
            }
        })
    }

    fn limits(&self) -> Fut<'_, Option<Limits>> {
        Box::pin(async move { Claude::limits(self).await })
    }

    fn watch(&self, key: String, id: String, out: UnboundedSender<AgentMsg>) -> Fut<'_, ()> {
        Box::pin(async move { Claude::watch(self, key, id, out).await })
    }

    fn watch_sub(&self, key: String, id: String, sub: String, out: UnboundedSender<AgentMsg>) -> Fut<'_, ()> {
        Box::pin(async move { Claude::watch_sub(self, key, id, sub, out).await })
    }

    fn older<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(Vec<Item>, bool), String>> {
        Box::pin(async move { Claude::older(self, id) })
    }

    fn send<'a>(
        &'a self,
        id: &'a str,
        text: &'a str,
        out: &'a UnboundedSender<AgentMsg>,
    ) -> Fut<'a, Result<Option<Item>, String>> {
        // Claude Code показывает сообщение сам: в окне консоли или в транскрипте
        // фонового процесса. Панели показывать нечего.
        Box::pin(async move { Claude::send(self, id, text, out).await.map(|_| None) })
    }

    fn commands<'a>(&'a self, id: &'a str) -> Fut<'a, Vec<Command>> {
        Box::pin(async move { Claude::commands(self, id) })
    }

    fn models<'a>(&'a self, _id: &'a str) -> Fut<'a, Vec<ModelInfo>> {
        // Модель выбирается при запуске сессии, переключать её на ходу нельзя.
        Box::pin(async move { Vec::new() })
    }

    fn launch_models<'a>(&'a self, _cwd: &'a str) -> Fut<'a, Vec<ModelInfo>> {
        // CLI принимает алиасы (`--model`), каталог на список не влияет.
        Box::pin(async move { crate::claude::launch_models() })
    }

    fn set_model<'a>(&'a self, _id: &'a str, _provider: &'a str, _model: &'a str) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { Err("смена модели есть только у сессий opencode".into()) })
    }

    fn interrupt<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { Claude::interrupt(self, id).await })
    }

    fn rename<'a>(&'a self, id: &'a str, title: &'a str) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { Claude::rename(self, id, title) })
    }

    fn move_to<'a>(&'a self, _id: &'a str, _cwd: &'a str) -> Fut<'a, Result<(), String>> {
        // Транскрипт Claude Code лежит в `~/.claude/projects/<каталог>/`: другой
        // каталог — другой проект, и `--resume` сессию там не найдёт. Перенос
        // файла с переписыванием `cwd` — приватный формат, поэтому не делаем.
        Box::pin(async move { Err("перемещать можно только сессии opencode".into()) })
    }

    fn delete<'a>(&'a self, id: &'a str, close: bool) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { Claude::delete(self, id, close).await })
    }

    fn close<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { Claude::close(self, id).await })
    }

    fn start<'a>(&'a self, cwd: &'a str, model: &'a str) -> Fut<'a, Result<Option<String>, String>> {
        Box::pin(async move { window_start(self, cwd, None, model).await })
    }

    fn start_with_prompt<'a>(
        &'a self,
        cwd: &'a str,
        prompt: Option<&'a str>,
        model: &'a str,
    ) -> Fut<'a, Result<Option<String>, String>> {
        Box::pin(async move { Claude::start_with_prompt(self, cwd, prompt, model) })
    }

    fn handoff_source<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(String, String), String>> {
        Box::pin(async move { Claude::handoff_source(self, id) })
    }

    #[allow(clippy::too_many_arguments)]
    fn reply_prompt<'a>(
        &'a self,
        id: &'a str,
        prompt: &'a str,
        kind: &'a str,
        _decision: &'a str,
        answer: &'a Value,
        _message: &'a str,
        cancel: bool,
    ) -> Fut<'a, Result<(), String>> {
        Box::pin(async move {
            // У Claude Code из панели отвечают только на вопросы с вариантами.
            if kind != "form" {
                return Err("на этот запрос из панели не ответить".into());
            }
            Claude::reply_prompt(self, id, prompt, answer, cancel).await
        })
    }

    fn cancel_queued<'a>(&'a self, _id: &'a str, _queue: &'a str) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { Err("очередь есть только у сессий opencode".into()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::transcript::{
        is_prompt, line_model, parse_line, parse_line_opts, read_turn, sub_turn, sum_turn, ParseOpts,
    };
    use crate::util::parse_iso_ms;

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

    /// Подагент: токены собираются по всему его транскрипту, дописанные строки
    /// дочитываются с прежнего места, а недописанная ждёт продолжения.
    #[test]
    fn subagent_turn_reads_transcript_incrementally() {
        let dir = std::env::temp_dir().join(format!("aipanel-sub-turn-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-a1.jsonl");
        let first = format!(
            "{}\n{}\n",
            prompt_line("2026-11-15T03:00:00.000Z"),
            reply_line("msg_1", 2, 100, 30)
        );
        fs::write(&path, &first).unwrap();
        let mut st = St::default();
        let (turn, times) = sub_turn(&mut st, &path);
        let turn = turn.unwrap();
        assert_eq!((turn.input, turn.output), (102, 30));
        let start = parse_iso_ms("2026-11-15T03:00:00.000Z");
        assert_eq!((times.started_at, times.ended_at), (start, start));

        // Тот же ответ второй строкой (мысль, текст, вызов) не удваивает выход.
        let second = format!("{first}{}\n", reply_line("msg_1", 2, 100, 30));
        fs::write(&path, &second).unwrap();
        let (turn, _) = sub_turn(&mut st, &path);
        let turn = turn.unwrap();
        assert_eq!((turn.input, turn.output), (102, 30));

        // Дописанный ответ добавляет выход, вход берётся из последнего ответа.
        let third = format!("{second}{}\n", reply_line("msg_2", 3, 200, 20));
        fs::write(&path, &third).unwrap();
        let (turn, _) = sub_turn(&mut st, &path);
        let turn = turn.unwrap();
        assert_eq!((turn.input, turn.output), (203, 50));

        // Недописанная строка не съедается: смещение остаётся на её начале.
        let partial = reply_line("msg_3", 4, 300, 5);
        fs::write(&path, format!("{third}{}", &partial[..20])).unwrap();
        let (turn, _) = sub_turn(&mut st, &path);
        let turn = turn.unwrap();
        assert_eq!((turn.input, turn.output), (203, 50));
        fs::write(&path, format!("{third}{partial}\n")).unwrap();
        let (turn, _) = sub_turn(&mut st, &path);
        let turn = turn.unwrap();
        assert_eq!((turn.input, turn.output), (304, 55));

        // Время подагента — по меткам его строк: конец сдвигается последней.
        let tail = r#"{"type":"user","timestamp":"2026-11-15T03:00:07.000Z","message":{"role":"user","content":"итог"}}"#;
        fs::write(&path, format!("{third}{partial}\n{tail}\n")).unwrap();
        let (_, times) = sub_turn(&mut st, &path);
        assert_eq!(times.started_at, start);
        assert_eq!(times.ended_at, parse_iso_ms("2026-11-15T03:00:07.000Z"));
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
    fn live_session_uses_current_cwd() {
        // Вошла в worktree: и показ, и передача — из worktree.
        assert_eq!(live_cwd("D:\\p", Some("D:\\p\\.claude\\worktrees\\opt")), "D:\\p\\.claude\\worktrees\\opt");
        // Транскрипта ещё нет или в нём нет каталога — каталог запуска.
        assert_eq!(live_cwd("D:\\p", None), "D:\\p");
        assert_eq!(live_cwd("D:\\p", Some("")), "D:\\p");
    }

    #[test]
    fn enter_worktree_result_marks_cwd_switch() {
        let mut items = Vec::new();
        // Строка из настоящего транскрипта (сокращена): результат EnterWorktree.
        parse_line(
            r#"{"parentUuid":"p","isSidechain":false,"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"Created worktree at D:\\p\\.claude\\worktrees\\opt","tool_use_id":"t1"}]},"uuid":"u1","timestamp":"2026-10-28T22:06:20.000Z","toolUseResult":{"worktreePath":"D:\\p\\.claude\\worktrees\\opt","worktreeBranch":"worktree-opt","message":"Created worktree"},"cwd":"D:\\p"}"#,
            &mut items,
        );
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.kind.as_str(), i.text.as_str())).collect();
        assert_eq!(got.last(), Some(&("u1:cwd", "cwd", "D:\\p\\.claude\\worktrees\\opt")));
        items.clear();
        parse_line(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok","tool_use_id":"t2"}]},"uuid":"u2","timestamp":"2026-10-28T23:00:00.000Z","toolUseResult":{"originalCwd":"D:\\p","worktreePath":"D:\\p\\.claude\\worktrees\\opt"}}"#,
            &mut items,
        );
        assert_eq!(items.last().map(|i| (i.kind.as_str(), i.text.as_str())), Some(("cwd", "D:\\p")));
        items.clear();
        // Обычный результат инструмента отметки не даёт.
        parse_line(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x","tool_use_id":"t3"}]},"uuid":"u3","toolUseResult":{"stdout":"x"}}"#,
            &mut items,
        );
        assert!(items.iter().all(|i| i.kind != "cwd"));
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
        // Уведомление фоновой задачи посреди хода — не сообщение пользователя,
        // а служебная плашка.
        parse_line(
            r#"{"type":"attachment","uuid":"a2","attachment":{"type":"queued_command","prompt":"<task-notification><status>failed</status></task-notification>","origin":{"kind":"task-notification"},"timestamp":"2026-11-15T10:47:21.483Z"}}"#,
            &mut items,
        );
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.role.as_str(), i.kind.as_str())).collect();
        assert_eq!(got, [("a2:queued", "system", "notice")]);
        items.clear();
        // Вложение без известного источника, пустой текст и служебная ветка
        // в ленту не попадают.
        parse_line(
            r#"{"type":"attachment","uuid":"a5","attachment":{"type":"queued_command","prompt":"что-то","origin":{"kind":"other"}}}"#,
            &mut items,
        );
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
    fn compact_summary_is_a_notice() {
        let mut items = Vec::new();
        parse_line(
            r#"{"type":"user","uuid":"c","isVisibleInTranscriptOnly":true,"isCompactSummary":true,"message":{"role":"user","content":"This session is being continued…"}}"#,
            &mut items,
        );
        parse_line(
            r#"{"type":"user","uuid":"d","isCompactSummary":true,"message":{"role":"user","content":[{"type":"text","text":"Summary"}]}}"#,
            &mut items,
        );
        let got: Vec<_> = items.iter().map(|i| (i.role.as_str(), i.kind.as_str(), i.tool.as_deref())).collect();
        assert_eq!(got, [("system", "notice", Some("compact")), ("system", "notice", Some("compact"))]);
        assert_eq!(items[1].text, "Summary");
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

    /// Переименование дописывает в транскрипт ту же строку, что и `/rename`
    /// в Claude Code: следующий обзор сессий показывает новое имя.
    #[test]
    fn rename_appends_custom_title() {
        let dir = std::env::temp_dir().join(format!("aipanel-claude-rename-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let project = dir.join("projects").join("p");
        fs::create_dir_all(&project).unwrap();
        let sid = "cccccccc-dddd-eeee-ffff-000000000000";
        let path = project.join(format!("{sid}.jsonl"));
        // Недописанный транскрипт: последняя строка без перевода строки.
        fs::write(&path, r#"{"type":"user","message":{"role":"user","content":"hi"}}"#).unwrap();

        let cfg = ClaudeCfg { home: Some(dir.clone()), ..ClaudeCfg::default() };
        let claude = Claude::new(cfg, Arc::new(Notify::new()));
        claude.rename(sid, "  Новое имя  ").unwrap();
        assert_eq!(claude.scan()[0].title, "Новое имя");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#"{"customTitle":"Новое имя","sessionId":"#));
        assert!(text.ends_with('\n'));

        // Позднее имя перекрывает прежнее, пустое и с переводами строк — не принимается.
        claude.rename(sid, "Второе имя").unwrap();
        assert_eq!(claude.scan()[0].title, "Второе имя");
        assert!(claude.rename(sid, "   ").is_err());
        assert!(claude.rename(sid, "имя\nс переводом").is_err());
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
        let (items, consumed, begin) = read_items(&path, 0, len, false, 1, ParseOpts::default());
        let third = body.find(&line("u3", "третий")).unwrap() as u64;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "u3:0");
        assert_eq!(consumed, len);
        assert_eq!(begin, third);

        // Назад от начала оставшегося читается остальное и без повторов.
        let (older, _, older_begin) = read_items(&path, 0, begin, false, usize::MAX, ParseOpts::default());
        let ids: Vec<_> = older.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["u1:0", "u2:0"]);
        assert_eq!(older_begin, 0);

        // Кусок без целой строки элементов не даёт и байт не съедает: догрузка
        // продолжит читать с того же места.
        let cut = third + 5;
        let (empty, consumed, begin) = read_items(&path, cut, len - 1, false, usize::MAX, ParseOpts::default());
        assert!(empty.is_empty());
        assert_eq!((consumed, begin), (0, cut));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Сообщение, набранное во время долгого инструмента, видно сразу по
    /// `enqueue` и пропадает, когда покидает очередь, — в том же куске или в
    /// следующем. Уведомления задач не показываются, но `dequeue` их учитывает.
    #[test]
    fn queued_message_shows_until_it_leaves_the_queue() {
        let dir = std::env::temp_dir().join(format!("aipanel-claude-queue-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let op = |op: &str, content: &str| {
            format!(r#"{{"type":"queue-operation","operation":"{op}","timestamp":"2026-11-16T10:00:00.000Z","content":"{content}"}}"#)
        };
        let dequeue = r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-11-16T10:00:01.000Z"}"#;
        let first = format!(
            "{}\n{}\n{}\n{}\n{}\n",
            op("enqueue", "<task-notification>x</task-notification>"),
            op("enqueue", "статус?"),
            op("enqueue", "и ещё"),
            op("remove", "и ещё"),
            op("enqueue", "третье"),
        );
        fs::write(&path, &first).unwrap();
        let mut queue = Queue::default();
        let (items, drop, consumed, _) = read_feed(
            &path,
            0,
            first.len() as u64,
            false,
            usize::MAX,
            Some(&mut queue),
            &mut ParseOpts::default(),
        );
        let got: Vec<_> = items.iter().map(|i| (i.kind.as_str(), i.text.as_str())).collect();
        assert_eq!(got, [("queued", "статус?"), ("queued", "третье")]);
        assert!(drop.is_empty());
        let status = items[0].id.clone();
        let third = items[1].id.clone();

        // Голова очереди — уведомление, второй `dequeue` снимает «статус?»;
        // `popAll` (Esc в консоли) — всё остальное.
        let rest = format!("{dequeue}\n{dequeue}\n{}\n", op("popAll", "третье"));
        fs::write(&path, format!("{first}{rest}")).unwrap();
        let end = (first.len() + rest.len()) as u64;
        let (items, drop, _, _) = read_feed(
            &path,
            consumed,
            end,
            false,
            usize::MAX,
            Some(&mut queue),
            &mut ParseOpts::default(),
        );
        assert!(items.is_empty());
        assert_eq!(drop, [status, third]);

        // Без очереди (догрузка ранних, перенос) строки очереди не дают ничего.
        let (items, _, _) = read_items(&path, 0, end, false, usize::MAX, ParseOpts::default());
        assert!(items.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Вызов подагента — карточка в ленте сессии, его результат — «Итог», а с
    /// метаданными карточка получает ссылку на ленту подагента. Записи самого
    /// подагента видны только в его транскрипте.
    #[test]
    fn subagent_call_becomes_a_card_in_session_feed() {
        let call = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-11-15T10:00:00.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Agent","input":{"description":"Найти причину","subagent_type":"explore"}}]}}"#;
        let result = r#"{"type":"user","uuid":"u1","timestamp":"2026-11-15T10:01:00.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"готово"}]}]}}"#;
        let plain = r#"{"type":"assistant","uuid":"a2","timestamp":"2026-11-15T10:02:00.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"a.rs"}}]}}"#;
        let mut items = Vec::new();
        let mut opts = ParseOpts::default();
        for line in [call, result, plain] {
            parse_line_opts(line, &mut items, &mut opts);
        }
        let got: Vec<_> = items
            .iter()
            .map(|i| (i.kind.as_str(), i.tool.as_deref(), i.sub.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("subagent", Some("explore"), None),
                ("subagent_result", None, None),
                ("tool_use", Some("Read"), None),
            ]
        );
        assert_eq!(items[0].text, "Найти причину");
        assert_eq!(items[1].text, "готово");

        // Связь из метаданных: id вызова -> id подагента. Она работает и тогда,
        // когда строка самого вызова осталась за пределами прочитанного куска.
        let mut sub_of = HashMap::new();
        sub_of.insert("toolu_1".to_owned(), "a1".to_owned());
        let mut opts = ParseOpts { subs: Some(&sub_of), ..Default::default() };
        let mut items = Vec::new();
        for line in [call, result] {
            parse_line_opts(line, &mut items, &mut opts);
        }
        assert_eq!(items[0].sub.as_deref(), Some("a1"));
        assert_eq!(items[1].sub.as_deref(), Some("a1"));
        let mut opts = ParseOpts { subs: Some(&sub_of), ..Default::default() };
        let mut items = Vec::new();
        parse_line_opts(result, &mut items, &mut opts);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "subagent_result");
        assert_eq!(items[0].sub.as_deref(), Some("a1"));

        // Фоновый запуск: результат — отметка о запуске, а не итог подагента.
        let background = r#"{"type":"user","uuid":"u2","timestamp":"2026-11-15T10:01:00.000Z","toolUseResult":{"backgroundTaskId":"bpn1"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"running in background"}]}]}}"#;
        let mut opts = ParseOpts { subs: Some(&sub_of), ..Default::default() };
        let mut items = Vec::new();
        parse_line_opts(background, &mut items, &mut opts);
        assert!(items.is_empty());

        // Записи подагента: в его транскрипте все строки `isSidechain`.
        let side = r#"{"type":"assistant","isSidechain":true,"uuid":"s1","timestamp":"2026-11-15T10:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"ищу"}]}}"#;
        let mut items = Vec::new();
        parse_line_opts(side, &mut items, &mut ParseOpts { sidechain: true, ..Default::default() });
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "s1:0");
        // Та же строка в ленте сессии не показывается: её пишет подагент.
        let mut items = Vec::new();
        parse_line(side, &mut items);
        assert!(items.is_empty());
    }
}

