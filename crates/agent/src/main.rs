//! Локальный агент AiPanel: сам подключается к серверу панели (исходящий WebSocket),
//! сообщает о сессиях Claude Code и opencode и выполняет команды из панели.

mod claude;
mod config;
mod install;
mod opencode;
mod platform;
mod util;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use aipanel_proto::{AgentMsg, ServerMsg, SessionInfo};
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
    online: AtomicBool,
    force: AtomicBool,
    watchers: Mutex<HashMap<String, JoinHandle<()>>>,
}

/// Идентификатор сессии попадает в имя файла, аргумент команды и URL,
/// поэтому от сервера принимаются только безопасные символы.
fn split_key(key: &str) -> Result<(&str, &str), String> {
    match key.split_once(':') {
        Some((tool, id))
            if !id.is_empty()
                && id.len() <= 128
                && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            Ok((tool, id))
        }
        _ => Err(format!("некорректный ключ сессии: {key}")),
    }
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
                    let result = match split_key(&session) {
                        Ok(("claude", sid)) => me.claude.send(sid, &text, &me.out).await,
                        Ok(("opencode", sid)) => me.opencode.send(sid, &text).await,
                        Ok(_) => Err("неизвестный тип сессии".into()),
                        Err(e) => Err(e),
                    };
                    me.finish(id, result);
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
                    me.finish(id, result);
                });
            }
        }
    }

    fn finish(&self, id: String, result: Result<(), String>) {
        if let Err(e) = &result {
            eprintln!("command failed: {e}");
        }
        let _ = self.out.send(AgentMsg::Result { id, ok: result.is_ok(), error: result.err() });
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
    agent.online.store(true, Ordering::Relaxed);
    agent.rescan.notify_one();

    let mut tick = tokio::time::interval(Duration::from_secs(30));
    let mut last_rx = Instant::now();
    loop {
        tokio::select! {
            m = stream.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    last_rx = Instant::now();
                    match serde_json::from_str::<ServerMsg>(t.as_str()) {
                        Ok(msg) => agent.handle(msg),
                        Err(e) => eprintln!("bad message from server: {e}"),
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
}

async fn run(cfg: Config) {
    let (out, mut rx) = mpsc::unbounded_channel();
    let rescan = Arc::new(Notify::new());
    let agent = Arc::new(Agent {
        claude: Arc::new(Claude::new(cfg.claude.clone(), Arc::clone(&rescan))),
        opencode: Arc::new(OpenCode::new(cfg.opencode.clone())),
        cfg,
        out,
        rescan,
        online: AtomicBool::new(false),
        force: AtomicBool::new(false),
        watchers: Mutex::new(HashMap::new()),
    });
    tokio::spawn(scanner(Arc::clone(&agent)));

    let mut backoff = 1;
    loop {
        match connect(&agent.cfg).await {
            Ok(ws) => {
                eprintln!("connected to {}", agent.cfg.server);
                backoff = 1;
                while rx.try_recv().is_ok() {} // сообщения прошлого подключения уже неактуальны
                session(&agent, ws, &mut rx).await;
                eprintln!("disconnected");
            }
            Err(e) => eprintln!("connect to {} failed: {e}", agent.cfg.server),
        }
        agent.stop_watchers();
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

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
            eprintln!("config error: {e}");
            std::process::exit(2);
        }
    };
    if cfg.server.starts_with("ws://") && !cfg.server.contains("://localhost") && !cfg.server.contains("://127.0.0.1") {
        eprintln!("warning: ws:// is unencrypted, the agent token is sent in clear text; use wss:// outside localhost");
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(run(cfg));
}
