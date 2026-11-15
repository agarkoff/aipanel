//! Локальный агент AiPanel: сам подключается к серверу панели (исходящий WebSocket),
//! сообщает о сессиях Claude Code и opencode и выполняет команды из панели.

mod claude;
mod config;
mod handoff;
mod image;
mod install;
mod opencode;
mod platform;
mod status;
mod util;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use aipanel_proto::{AgentMsg, Limits, ServerMsg, SessionInfo};
use futures_util::{SinkExt, StreamExt};
use tokio::{
    net::TcpStream,
    sync::{mpsc, Notify},
    task::JoinHandle,
};
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, Message},
    MaybeTlsStream, WebSocketStream,
};

use crate::{claude::Claude, config::Config, opencode::OpenCode};

struct Agent {
    cfg: Config,
    claude: Arc<Claude>,
    opencode: Arc<OpenCode>,
    out: mpsc::UnboundedSender<AgentMsg>,
    rescan: Arc<Notify>,
    /// Будит проверку лимитов: сразу после подключения к серверу.
    limits_now: Arc<Notify>,
    force_limits: AtomicBool,
    online: AtomicBool,
    force: AtomicBool,
    watchers: Mutex<HashMap<String, JoinHandle<()>>>,
    /// Каталог каждой известной сессии: от него считаются относительные пути картинок.
    cwds: Mutex<HashMap<String, String>>,
    // Состояние для команды `status` (см. status.rs).
    started: i64,
    connected_since: AtomicI64,
    sessions: AtomicUsize,
    last_error: Mutex<String>,
}

/// Идентификатор сессии попадает в имя файла, аргумент команды и URL,
/// поэтому от сервера принимаются только безопасные символы.
fn split_key(key: &str) -> Result<(&str, &str), String> {
    match key.split_once(':') {
        Some((tool, id)) if safe_id(id) => Ok((tool, id)),
        _ => Err(format!("некорректный ключ сессии: {key}")),
    }
}

/// Идентификатор попадает в имя файла, аргумент команды и URL,
/// поэтому от сервера принимаются только безопасные символы.
fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Agent {
    fn handle(self: &Arc<Self>, msg: ServerMsg) {
        match msg {
            ServerMsg::Watch { session } => {
                let me = Arc::clone(self);
                let key = session.clone();
                let task = tokio::spawn(async move {
                    let out = me.out.clone();
                    match split_key(&key) {
                        Ok(("claude", id)) => me.claude.watch(key.clone(), id.to_owned(), out).await,
                        Ok(("opencode", id)) => me.opencode.watch(key.clone(), id.to_owned(), out).await,
                        _ => {}
                    }
                });
                if let Some(old) = self.watchers.lock().unwrap().insert(session, task) {
                    old.abort();
                }
            }
            ServerMsg::Unwatch { session } => {
                if let Some(task) = self.watchers.lock().unwrap().remove(&session) {
                    task.abort();
                }
            }
            ServerMsg::Send { id, session, text } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let mut queued = None;
                    let result = match split_key(&session) {
                        Ok(("claude", sid)) => me.claude.send(sid, &text, &me.out).await,
                        Ok(("opencode", sid)) => match me.opencode.send(sid, &text).await {
                            Ok(item) => {
                                queued = item;
                                Ok(())
                            }
                            Err(e) => Err(e),
                        },
                        Ok(_) => Err("неизвестный тип сессии".into()),
                        Err(e) => Err(e),
                    };
                    // Сообщение, принятое в очередь, показываем сразу: в ленту opencode
                    // его отдаст только после завершения текущего ответа.
                    if let Some(item) = queued {
                        let item_id = item.id.clone();
                        let _ = me.out.send(AgentMsg::History {
                            session: session.clone(),
                            reset: false,
                            items: vec![item],
                            drop: Vec::new(),
                        });
                        // Только после отправки клиенту: иначе наблюдение могло бы убрать
                        // сообщение из ленты раньше, чем оно туда попало.
                        if let Ok(("opencode", sid)) = split_key(&session) {
                            me.opencode.note_optimistic(sid, &item_id);
                        }
                    }
                    me.finish(id, result.map(|_| None));
                });
            }
            ServerMsg::Commands { session } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let commands = match split_key(&session) {
                        Ok(("claude", sid)) => me.claude.commands(sid),
                        Ok(("opencode", sid)) => me.opencode.commands(sid).await,
                        _ => Vec::new(),
                    };
                    let _ = me.out.send(AgentMsg::Commands { session, commands });
                });
            }
            ServerMsg::Models { session } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let models = match split_key(&session) {
                        Ok(("opencode", sid)) => me.opencode.models(sid).await,
                        _ => Vec::new(),
                    };
                    let _ = me.out.send(AgentMsg::Models { session, models });
                });
            }
            ServerMsg::SetModel { id, session, provider, model } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result = match split_key(&session) {
                        Ok(("opencode", sid)) => me.opencode.set_model(sid, &provider, &model).await,
                        Ok(_) => Err("смена модели есть только у сессий opencode".into()),
                        Err(e) => Err(e),
                    };
                    me.finish(id, result.map(|_| None));
                });
            }
            ServerMsg::Interrupt { id, session } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result = match split_key(&session) {
                        Ok(("claude", sid)) => me.claude.interrupt(sid).await,
                        Ok(("opencode", sid)) => me.opencode.interrupt(sid).await,
                        Ok(_) => Err("неизвестный тип сессии".into()),
                        Err(e) => Err(e),
                    };
                    me.finish(id, result.map(|_| None));
                });
            }
            ServerMsg::ToolModels { id, tool, cwd } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let models = match tool.as_str() {
                        "claude" => crate::claude::launch_models(),
                        "opencode" => me.opencode.models_at(&cwd).await,
                        _ => Vec::new(),
                    };
                    let _ = me.out.send(AgentMsg::ToolModels { id, models });
                });
            }
            ServerMsg::Start { id, tool, cwd, model } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result = match tool.as_str() {
                        "claude" => me.claude.start(&cwd, &model),
                        "opencode" => me.opencode.start(&cwd, &model).await,
                        _ => Err("неизвестный тип сессии".into()),
                    };
                    me.finish(id, result);
                });
            }
            ServerMsg::Handoff { id, session, tool } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result = me.handoff(&session, &tool).await;
                    me.finish(id, result);
                });
            }
            ServerMsg::CancelQueued { id, session, queue } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result = me.cancel_queued(&session, &queue).await;
                    me.finish(id, result.map(|_| None));
                });
            }
            ServerMsg::Delete { id, session, close } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result = match split_key(&session) {
                        Ok(("claude", sid)) => me.claude.delete(sid, close).await,
                        Ok(("opencode", sid)) => me.opencode.delete(sid, close).await,
                        Ok(_) => Err("неизвестный тип сессии".into()),
                        Err(e) => Err(e),
                    };
                    me.finish(id, result.map(|_| None));
                });
            }
            ServerMsg::PromptReply { id, session, prompt, kind, decision, answer, message, cancel } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result =
                        me.reply_prompt(&session, &prompt, &kind, &decision, &answer, &message, cancel).await;
                    me.finish(id, result.map(|_| None));
                });
            }
            ServerMsg::ReadImage { id, session, path } => {
                let cwd = self.cwds.lock().unwrap().get(&session).cloned();
                let out = self.out.clone();
                tokio::task::spawn_blocking(move || {
                    let msg = match image::read(cwd.as_deref(), &path) {
                        Ok((mime, data)) => {
                            AgentMsg::Image { id, mime: mime.into(), data: aipanel_proto::b64::encode(&data), error: None }
                        }
                        Err(e) => {
                            util::log(&format!("read image failed: {e}"));
                            AgentMsg::Image { id, mime: String::new(), data: String::new(), error: Some(e) }
                        }
                    };
                    let _ = out.send(msg);
                });
            }
        }
    }

    /// Отвечает на запрос opencode: разрешение, форма или её отмена.
    #[allow(clippy::too_many_arguments)]
    async fn reply_prompt(
        &self,
        session: &str,
        prompt: &str,
        kind: &str,
        decision: &str,
        answer: &serde_json::Value,
        message: &str,
        cancel: bool,
    ) -> Result<(), String> {
        if !safe_id(prompt) {
            return Err("некорректный идентификатор запроса".into());
        }
        if message.chars().count() > 4000 {
            return Err("слишком длинное сообщение к запросу".into());
        }
        match split_key(session)? {
            ("opencode", id) => {
                self.opencode.reply_prompt(id, prompt, kind, decision, answer, message, cancel).await
            }
            _ => Err("запросы выбора есть только у сессий opencode".into()),
        }
    }

    /// Убирает сообщение из очереди opencode, пока оно не отправлено.
    async fn cancel_queued(&self, session: &str, queue: &str) -> Result<(), String> {
        if !safe_id(queue) {
            return Err("некорректный идентификатор очереди".into());
        }
        match split_key(session)? {
            ("opencode", id) => self.opencode.cancel_queued(id, queue).await,
            _ => Err("очередь есть только у сессий opencode".into()),
        }
    }

    /// Продолжает сессию в другом инструменте: собирает выжимку из её переписки
    /// и открывает новую сессию в том же каталоге.
    async fn handoff(&self, session: &str, tool: &str) -> Result<Option<String>, String> {
        let (source, id) = split_key(session)?;
        if source == tool {
            return Err("сессия уже открыта в этом инструменте".into());
        }
        let (cwd, text) = match source {
            "claude" => self.claude.handoff_source(id)?,
            "opencode" => self.opencode.handoff_source(id).await?,
            _ => return Err("неизвестный тип сессии".into()),
        };
        match tool {
            "claude" if self.cfg.claude.enabled => self.claude.start_with_prompt(&cwd, Some(&text), ""),
            "opencode" if self.cfg.opencode.enabled => {
                self.opencode.start_with_prompt(&cwd, Some(&text), "").await
            }
            "claude" | "opencode" => Err(format!("инструмент {tool} выключен в agent.toml")),
            _ => Err("неизвестный тип сессии".into()),
        }
    }

    fn finish(&self, id: String, result: Result<Option<String>, String>) {
        if let Err(e) = &result {
            util::log(&format!("command failed: {e}"));
        }
        let (ok, session, error) = match result {
            Ok(session) => (true, session, None),
            Err(e) => (false, None, Some(e)),
        };
        let _ = self.out.send(AgentMsg::Result { id, ok, error, session });
        self.rescan.notify_one();
    }

    fn stop_watchers(&self) {
        for (_, task) in self.watchers.lock().unwrap().drain() {
            task.abort();
        }
    }
}

async fn scanner(agent: Arc<Agent>) {
    let mut last: Vec<SessionInfo> = Vec::new();
    loop {
        if agent.online.load(Ordering::Relaxed) {
            let mut list = Vec::new();
            if agent.cfg.claude.enabled {
                list.extend(agent.claude.scan());
            }
            if agent.cfg.opencode.enabled {
                list.extend(agent.opencode.scan().await);
            }
            list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            agent.sessions.store(list.len(), Ordering::Relaxed);
            *agent.cwds.lock().unwrap() = list.iter().map(|s| (s.key.clone(), s.cwd.clone())).collect();
            if agent.force.swap(false, Ordering::Relaxed) || list != last {
                let _ = agent.out.send(AgentMsg::Sessions { sessions: list.clone() });
                last = list;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = agent.rescan.notified() => {}
        }
    }
}

/// Раз в несколько минут проверяет остаток лимитов подписок на этой машине
/// и отправляет серверу, если он изменился.
async fn limits_watcher(agent: Arc<Agent>) {
    let mut last: Vec<Limits> = Vec::new();
    loop {
        if agent.online.load(Ordering::Relaxed) {
            let mut list = Vec::new();
            if agent.cfg.claude.enabled {
                if let Some(limits) = agent.claude.limits().await {
                    list.push(limits);
                }
            }
            if agent.cfg.opencode.enabled {
                if let Some(limits) = agent.opencode.limits().await {
                    list.push(limits);
                }
            }
            // Неудачный опрос не стирает известные лимиты: пустой список уходит,
            // только когда сервер сам попросил свежие данные после подключения.
            if agent.force_limits.swap(false, Ordering::Relaxed) || list != last {
                let _ = agent.out.send(AgentMsg::Limits { limits: list.clone() });
                last = list;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(180)) => {}
            _ = agent.limits_now.notified() => {}
        }
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn connect(cfg: &Config) -> Result<Ws, String> {
    let mut req = cfg.server.as_str().into_client_request().map_err(|e| e.to_string())?;
    let auth = format!("Bearer {}", cfg.token).parse().map_err(|_| "token contains invalid characters".to_owned())?;
    req.headers_mut().insert("Authorization", auth);
    let (ws, _) = tokio::time::timeout(Duration::from_secs(20), tokio_tungstenite::connect_async(req))
        .await
        .map_err(|_| "timeout".to_owned())?
        .map_err(|e| e.to_string())?;
    Ok(ws)
}

fn text_msg<T: serde::Serialize>(value: &T) -> Message {
    Message::Text(serde_json::to_string(value).unwrap_or_default().into())
}

async fn session(agent: &Arc<Agent>, ws: Ws, rx: &mut mpsc::UnboundedReceiver<AgentMsg>) {
    let (mut sink, mut stream) = ws.split();
    let hello = AgentMsg::Hello {
        name: agent.cfg.name.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
    };
    if sink.send(text_msg(&hello)).await.is_err() {
        return;
    }
    agent.force.store(true, Ordering::Relaxed);
    agent.force_limits.store(true, Ordering::Relaxed);
    agent.online.store(true, Ordering::Relaxed);
    agent.connected_since.store(util::now_ms(), Ordering::Relaxed);
    agent.rescan.notify_one();
    agent.limits_now.notify_one();

    let mut tick = tokio::time::interval(Duration::from_secs(30));
    let mut last_rx = Instant::now();
    loop {
        tokio::select! {
            m = stream.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    last_rx = Instant::now();
                    match serde_json::from_str::<ServerMsg>(t.as_str()) {
                        Ok(msg) => agent.handle(msg),
                        Err(e) => util::log(&format!("bad message from server: {e}")),
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => last_rx = Instant::now(),
            },
            m = rx.recv() => match m {
                Some(msg) => {
                    if sink.send(text_msg(&msg)).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            _ = tick.tick() => {
                if last_rx.elapsed() > Duration::from_secs(90) || sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    agent.online.store(false, Ordering::Relaxed);
    agent.connected_since.store(0, Ordering::Relaxed);
}

/// Раз в 2 секунды обновляет файл состояния для команд `status` и `stop`.
async fn status_writer(agent: Arc<Agent>) {
    loop {
        status::write(&status::Status {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
            name: agent.cfg.name.clone(),
            server: agent.cfg.server.clone(),
            started: agent.started,
            connected_since: agent.connected_since.load(Ordering::Relaxed),
            last_error: agent.last_error.lock().unwrap().clone(),
            sessions: agent.sessions.load(Ordering::Relaxed),
            updated: util::now_ms(),
        });
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn run(cfg: Config) {
    let (out, mut rx) = mpsc::unbounded_channel();
    let rescan = Arc::new(Notify::new());
    let limits_now = Arc::new(Notify::new());
    let agent = Arc::new(Agent {
        claude: Arc::new(Claude::new(cfg.claude.clone(), Arc::clone(&rescan))),
        opencode: Arc::new(OpenCode::new(cfg.opencode.clone())),
        cfg,
        out,
        rescan,
        limits_now,
        force_limits: AtomicBool::new(false),
        online: AtomicBool::new(false),
        force: AtomicBool::new(false),
        watchers: Mutex::new(HashMap::new()),
        cwds: Mutex::new(HashMap::new()),
        started: util::now_ms(),
        connected_since: AtomicI64::new(0),
        sessions: AtomicUsize::new(0),
        last_error: Mutex::new(String::new()),
    });
    tokio::spawn(scanner(Arc::clone(&agent)));
    tokio::spawn(limits_watcher(Arc::clone(&agent)));
    tokio::spawn(status_writer(Arc::clone(&agent)));

    let mut backoff = 1;
    // Одна и та же ошибка подключения пишется в лог один раз, иначе лог рос бы
    // строкой в 30 секунд, пока сервер недоступен.
    let mut last_failure: Option<String> = None;
    loop {
        match connect(&agent.cfg).await {
            Ok(ws) => {
                util::log(&format!("connected to {}", agent.cfg.server));
                last_failure = None;
                agent.last_error.lock().unwrap().clear();
                backoff = 1;
                while rx.try_recv().is_ok() {} // сообщения прошлого подключения уже неактуальны
                session(&agent, ws, &mut rx).await;
                util::log("disconnected");
            }
            Err(e) => {
                if last_failure.as_deref() != Some(e.as_str()) {
                    util::log(&format!("connect to {} failed: {e} (повторы не пишутся)", agent.cfg.server));
                }
                last_failure = Some(e.clone());
                *agent.last_error.lock().unwrap() = e;
            }
        }
        agent.stop_watchers();
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

/// Значение флага вида `--config <путь>` / `--home <путь>`.
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Второй экземпляр агента (например, для удалённой панели) живёт в своём
    // каталоге состояния: так его лог, pid-файл и статус не пересекаются с
    // основным. Задаётся флагом `--home` или переменной AIPANEL_HOME.
    if let Some(home) = arg_value(&args, "--home") {
        util::set_aipanel_dir(PathBuf::from(home));
    }

    // Внутренний режим: ввод в консоль другого процесса (см. platform.rs).
    if args.first().map(String::as_str) == Some("inject") {
        let Some(pid) = args.get(1).and_then(|p| p.parse::<u32>().ok()) else {
            eprintln!("usage: aipanel-agent inject <pid> [--esc]");
            std::process::exit(2);
        };
        let esc = args.iter().any(|a| a == "--esc");
        if let Err(e) = platform::inject_main(pid, esc) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }

    // Внутренний режим: обёртка окна терминала (см. platform::run_window).
    if args.first().map(String::as_str) == Some("run-window") {
        let Some(program) = args.get(1) else {
            eprintln!("usage: aipanel-agent run-window <program> [args...]");
            std::process::exit(2);
        };
        std::process::exit(platform::run_window(program, &args[2..]));
    }

    // Консольные команды управления агентом (см. status.rs).
    match args.first().map(String::as_str) {
        Some("status") => {
            let config = arg_value(&args, "--config");
            let home = arg_value(&args, "--home");
            std::process::exit(status::cmd_status(
                !args.iter().any(|a| a == "--no-start"),
                config.as_deref(),
                home.as_deref(),
            ));
        }
        Some("stop") => std::process::exit(status::cmd_stop()),
        _ => {}
    }

    // Установка и удаление плагина AiPanel для TUI opencode (см. install.rs).
    let plugin_cmd = match args.first().map(String::as_str) {
        Some("install-opencode-plugin") => Some(install::install()),
        Some("uninstall-opencode-plugin") => Some(install::uninstall()),
        _ => None,
    };
    if let Some(result) = plugin_cmd {
        match result {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    claude::scrub_env();
    let explicit = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let cfg = match Config::load(explicit) {
        Ok(cfg) => cfg,
        Err(e) => {
            util::log(&format!("config error: {e}"));
            std::process::exit(2);
        }
    };
    // Два агента на одной машине вытесняли бы друг друга на сервере.
    if let Some(other) = status::running() {
        eprintln!("agent is already running (pid {}); use `aipanel-agent status` or `stop`", other.pid);
        // В лог не пишется: автозапуск вызывает status каждые несколько минут.
        std::process::exit(3);
    }
    platform::set_terminal_profile(&cfg.terminal_profile);
    if cfg.server.starts_with("ws://") && !cfg.server.contains("://localhost") && !cfg.server.contains("://127.0.0.1") {
        util::log("warning: ws:// is unencrypted, the agent token is sent in clear text; use wss:// outside localhost");
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(run(cfg));
}
