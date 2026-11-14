//! Сервер панели: принимает исходящие подключения агентов, отдаёт веб-интерфейс
//! и пересылает сообщения между браузерами и агентами. Ленты сессий не хранит —
//! в памяти только списки сессий и активные подключения.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use aipanel_proto::{AgentMsg, ServerMsg, SessionInfo};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const MD_JS: &str = include_str!("../static/md.js");
const STYLE_CSS: &str = include_str!("../static/style.css");

const COOKIE: &str = "aipanel_session";
const MAX_TOKENS: usize = 64;
const MAX_SESSIONS_PER_AGENT: usize = 500;
const MAX_TEXT: usize = 200_000;
const PING_EVERY: Duration = Duration::from_secs(25);
const DEAD_AFTER: Duration = Duration::from_secs(90);

struct AgentEntry {
    conn: u64,
    tx: Option<mpsc::Sender<ServerMsg>>,
    os: String,
    version: String,
    sessions: Vec<SessionInfo>,
}

struct BrowserEntry {
    tx: mpsc::Sender<String>,
    /// (агент, сессия), ленту которой сейчас смотрит браузер.
    watching: Option<(String, String)>,
}

#[derive(Default)]
struct Inner {
    agents: BTreeMap<String, AgentEntry>,
    browsers: HashMap<u64, BrowserEntry>,
    /// id команды на сервере -> (браузер, исходный id команды).
    pending: HashMap<String, (u64, String)>,
    tokens: VecDeque<String>,
    next_id: u64,
    fail_window: Option<Instant>,
    fail_count: u32,
}

impl Inner {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn agents_json(&self) -> String {
        let agents: Vec<_> = self
            .agents
            .iter()
            .map(|(name, a)| {
                json!({
                    "name": name,
                    "online": a.tx.is_some(),
                    "os": a.os,
                    "version": a.version,
                    "sessions": a.sessions,
                })
            })
            .collect();
        json!({ "t": "agents", "agents": agents }).to_string()
    }

    fn broadcast_agents(&self) {
        let msg = self.agents_json();
        for b in self.browsers.values() {
            let _ = b.tx.try_send(msg.clone());
        }
    }

    fn watchers(&self, agent: &str, session: &str) -> usize {
        self.browsers
            .values()
            .filter(|b| matches!(&b.watching, Some((a, s)) if a == agent && s == session))
            .count()
    }

    fn to_agent(&self, agent: &str, msg: ServerMsg) -> bool {
        match self.agents.get(agent).and_then(|a| a.tx.as_ref()) {
            Some(tx) => tx.try_send(msg).is_ok(),
            None => false,
        }
    }

    fn unwatch(&mut self, browser: u64) {
        let prev = self.browsers.get_mut(&browser).and_then(|b| b.watching.take());
        if let Some((agent, session)) = prev {
            if self.watchers(&agent, &session) == 0 {
                self.to_agent(&agent, ServerMsg::Unwatch { session });
            }
        }
    }
}

struct App {
    inner: Mutex<Inner>,
    password: String,
    agent_token: String,
}

impl App {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn authed(&self, headers: &HeaderMap) -> bool {
        match cookie(headers, COOKIE) {
            Some(tok) => self.lock().tokens.iter().any(|t| ct_eq(t.as_bytes(), tok.as_bytes())),
            None => false,
        }
    }
}

/// Сравнение без раннего выхода, чтобы время ответа не выдавало совпавший префикс.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// WebSocket из браузера принимается только со своей страницы.
fn origin_ok(headers: &HeaderMap) -> bool {
    let Some(origin) = header_str(headers, "origin") else {
        return true; // не браузер; cookie всё равно обязательна
    };
    let origin_host = origin.split_once("://").map(|(_, h)| h).unwrap_or(origin);
    [header_str(headers, "host"), header_str(headers, "x-forwarded-host")]
        .into_iter()
        .flatten()
        .any(|h| h.eq_ignore_ascii_case(origin_host))
}

fn random_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("system random source");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; frame-ancestors 'none'; base-uri 'none'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        body,
    )
        .into_response()
}

#[derive(Deserialize)]
struct Login {
    password: String,
}

async fn login(State(app): State<Arc<App>>, headers: HeaderMap, Json(body): Json<Login>) -> Response {
    {
        let mut inner = app.lock();
        let now = Instant::now();
        if inner.fail_window.is_none_or(|t| now.duration_since(t) > Duration::from_secs(60)) {
            inner.fail_window = Some(now);
            inner.fail_count = 0;
        }
        if inner.fail_count >= 10 {
            return (StatusCode::TOO_MANY_REQUESTS, "слишком много попыток, подождите минуту").into_response();
        }
    }
    if !ct_eq(body.password.as_bytes(), app.password.as_bytes()) {
        app.lock().fail_count += 1;
        tokio::time::sleep(Duration::from_millis(700)).await;
        return (StatusCode::UNAUTHORIZED, "неверный пароль").into_response();
    }
    let token = random_token();
    {
        let mut inner = app.lock();
        inner.tokens.push_back(token.clone());
        while inner.tokens.len() > MAX_TOKENS {
            inner.tokens.pop_front();
        }
    }
    let secure = header_str(&headers, "x-forwarded-proto").is_some_and(|p| p.eq_ignore_ascii_case("https"));
    let cookie = format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000{}",
        if secure { "; Secure" } else { "" }
    );
    let mut resp = StatusCode::NO_CONTENT.into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

async fn logout(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(tok) = cookie(&headers, COOKIE) {
        app.lock().tokens.retain(|t| t != tok);
    }
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"))],
    )
        .into_response()
}

async fn me(State(app): State<Arc<App>>, headers: HeaderMap) -> StatusCode {
    if app.authed(&headers) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::UNAUTHORIZED
    }
}

// ---------------------------------------------------------------- агент

async fn agent_ws(State(app): State<Arc<App>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let ok = header_str(&headers, "authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| ct_eq(t.as_bytes(), app.agent_token.as_bytes()));
    if !ok {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.max_message_size(8 << 20)
        .max_frame_size(8 << 20)
        .on_upgrade(move |socket| agent_conn(app, socket))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
}

async fn agent_conn(app: Arc<App>, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();

    let hello = tokio::time::timeout(Duration::from_secs(15), stream.next()).await;
    let (name, version, os) = match hello {
        Ok(Some(Ok(Message::Text(t)))) => match serde_json::from_str::<AgentMsg>(t.as_str()) {
            Ok(AgentMsg::Hello { name, version, os }) if valid_name(&name) => (name, version, os),
            _ => return,
        },
        _ => return,
    };

    let (tx, mut rx) = mpsc::channel::<ServerMsg>(256);
    let conn = {
        let mut inner = app.lock();
        let conn = inner.id();
        // Браузеры могли открыть ленту, пока агент был не в сети: возобновляем подписки.
        let mut watched: Vec<String> = inner
            .browsers
            .values()
            .filter_map(|b| b.watching.as_ref())
            .filter(|(a, _)| *a == name)
            .map(|(_, s)| s.clone())
            .collect();
        watched.sort();
        watched.dedup();
        for session in watched {
            let _ = tx.try_send(ServerMsg::Watch { session });
        }
        let sessions = inner.agents.remove(&name).map(|a| a.sessions).unwrap_or_default();
        inner.agents.insert(
            name.clone(),
            AgentEntry { conn, tx: Some(tx), os, version, sessions },
        );
        inner.broadcast_agents();
        conn
    };
    eprintln!("agent connected: {name}");

    let mut ping = tokio::time::interval(PING_EVERY);
    let mut last_rx = Instant::now();
    loop {
        tokio::select! {
            m = stream.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    last_rx = Instant::now();
                    if let Ok(msg) = serde_json::from_str::<AgentMsg>(t.as_str()) {
                        on_agent_msg(&app, &name, msg);
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => last_rx = Instant::now(),
            },
            m = rx.recv() => match m {
                Some(msg) => {
                    let text = serde_json::to_string(&msg).unwrap_or_default();
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                None => break, // подключение заменено новым с тем же именем
            },
            _ = ping.tick() => {
                if last_rx.elapsed() > DEAD_AFTER || sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }

    let mut inner = app.lock();
    if let Some(a) = inner.agents.get_mut(&name) {
        if a.conn == conn {
            a.tx = None;
            inner.broadcast_agents();
            eprintln!("agent disconnected: {name}");
        }
    }
}

fn on_agent_msg(app: &App, agent: &str, msg: AgentMsg) {
    let mut inner = app.lock();
    match msg {
        AgentMsg::Hello { .. } => {}
        AgentMsg::Sessions { mut sessions } => {
            sessions.truncate(MAX_SESSIONS_PER_AGENT);
            if let Some(a) = inner.agents.get_mut(agent) {
                a.sessions = sessions;
            }
            inner.broadcast_agents();
        }
        AgentMsg::History { session, reset, items } => {
            let text = json!({
                "t": "history", "agent": agent, "session": session, "reset": reset, "items": items,
            })
            .to_string();
            for b in inner.browsers.values() {
                if matches!(&b.watching, Some((a, s)) if a == agent && *s == session) {
                    let _ = b.tx.try_send(text.clone());
                }
            }
        }
        AgentMsg::Result { id, ok, error } => {
            if let Some((browser, orig)) = inner.pending.remove(&id) {
                if let Some(b) = inner.browsers.get(&browser) {
                    let _ = b.tx.try_send(json!({ "t": "result", "id": orig, "ok": ok, "error": error }).to_string());
                }
            }
        }
    }
}

// ---------------------------------------------------------------- браузер

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum BrowserMsg {
    Watch { agent: String, session: String },
    Unwatch,
    Send { id: String, agent: String, session: String, text: String },
    Interrupt { id: String, agent: String, session: String },
}

async fn browser_ws(State(app): State<Arc<App>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if !app.authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !origin_ok(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(1 << 20)
        .on_upgrade(move |socket| browser_conn(app, socket))
}

async fn browser_conn(app: Arc<App>, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<String>(512);
    let id = {
        let mut inner = app.lock();
        let id = inner.id();
        let _ = tx.try_send(inner.agents_json());
        inner.browsers.insert(id, BrowserEntry { tx, watching: None });
        id
    };

    let mut ping = tokio::time::interval(PING_EVERY);
    let mut last_rx = Instant::now();
    loop {
        tokio::select! {
            m = stream.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    last_rx = Instant::now();
                    if let Ok(msg) = serde_json::from_str::<BrowserMsg>(t.as_str()) {
                        on_browser_msg(&app, id, msg);
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => last_rx = Instant::now(),
            },
            m = rx.recv() => match m {
                Some(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            _ = ping.tick() => {
                if last_rx.elapsed() > DEAD_AFTER || sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }

    let mut inner = app.lock();
    inner.unwatch(id);
    inner.browsers.remove(&id);
    inner.pending.retain(|_, (b, _)| *b != id);
}

fn on_browser_msg(app: &App, browser: u64, msg: BrowserMsg) {
    let mut inner = app.lock();
    match msg {
        BrowserMsg::Watch { agent, session } => {
            inner.unwatch(browser);
            if let Some(b) = inner.browsers.get_mut(&browser) {
                b.watching = Some((agent.clone(), session.clone()));
            }
            // Отправляем всегда: агент в ответ пришлёт свежую ленту целиком.
            inner.to_agent(&agent, ServerMsg::Watch { session });
        }
        BrowserMsg::Unwatch => inner.unwatch(browser),
        BrowserMsg::Send { id, agent, session, text } => {
            if text.trim().is_empty() || text.len() > MAX_TEXT {
                reply(&inner, browser, &id, "пустое или слишком длинное сообщение");
                return;
            }
            let cmd = format!("{browser}:{}", inner.id());
            command(&mut inner, browser, id, &agent, cmd.clone(), ServerMsg::Send { id: cmd, session, text });
        }
        BrowserMsg::Interrupt { id, agent, session } => {
            let cmd = format!("{browser}:{}", inner.id());
            command(&mut inner, browser, id, &agent, cmd.clone(), ServerMsg::Interrupt { id: cmd, session });
        }
    }
}

fn command(inner: &mut Inner, browser: u64, orig_id: String, agent: &str, cmd: String, msg: ServerMsg) {
    if inner.to_agent(agent, msg) {
        inner.pending.insert(cmd, (browser, orig_id));
    } else {
        reply(inner, browser, &orig_id, "агент не в сети");
    }
}

fn reply(inner: &Inner, browser: u64, id: &str, error: &str) {
    if let Some(b) = inner.browsers.get(&browser) {
        let _ = b.tx.try_send(json!({ "t": "result", "id": id, "ok": false, "error": error }).to_string());
    }
}

// ---------------------------------------------------------------- запуск

fn required_env(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if v.trim().len() >= 8 => v.trim().to_owned(),
        _ => {
            eprintln!("{name} is not set or shorter than 8 characters; refusing to start");
            std::process::exit(2);
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// `aipanel-server healthcheck` — проверка для Docker: в образе нет ни shell, ни curl.
fn healthcheck(bind: &str) -> bool {
    use std::io::{Read, Write};
    let port = bind.rsplit(':').next().unwrap_or("8080");
    let Ok(mut conn) = std::net::TcpStream::connect(format!("127.0.0.1:{port}")) else {
        return false;
    };
    let _ = conn.set_read_timeout(Some(Duration::from_secs(3)));
    let mut resp = [0u8; 12];
    conn.write_all(b"GET /healthz HTTP/1.0\r\n\r\n").is_ok()
        && conn.read_exact(&mut resp).is_ok()
        && resp.starts_with(b"HTTP/1.")
        && resp.ends_with(b" 200")
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
        std::process::exit(if healthcheck(&bind) { 0 } else { 1 });
    }
    let app = Arc::new(App {
        inner: Mutex::new(Inner::default()),
        password: required_env("PANEL_PASSWORD"),
        agent_token: required_env("AGENT_TOKEN"),
    });
    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());

    let router = Router::new()
        .route("/", get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }))
        .route("/app.js", get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }))
        .route("/md.js", get(|| async { asset("text/javascript; charset=utf-8", MD_JS) }))
        .route("/style.css", get(|| async { asset("text/css; charset=utf-8", STYLE_CSS) }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/ws", get(browser_ws))
        .route("/agent", get(agent_ws))
        .with_state(app);

    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot bind {bind}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("aipanel-server listening on {bind}");
    tokio::select! {
        r = axum::serve(listener, router) => {
            if let Err(e) = r {
                eprintln!("server error: {e}");
            }
        }
        _ = shutdown_signal() => {}
    }
}
