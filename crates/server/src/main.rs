//! Сервер панели: принимает исходящие подключения агентов, отдаёт веб-интерфейс
//! и пересылает сообщения между браузерами и агентами. Ленты сессий не хранит —
//! в памяти только списки сессий и активные подключения.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use aipanel_proto::{
    AgentMsg, Limits, ServerMsg, SessionInfo, IMAGE_MIMES, MAX_FAVORITES, MAX_FAVORITE_TEXT, MAX_IMAGE_BYTES, MAX_ITEM_ID,
    MAX_SCREEN_BYTES, MAX_SCREENS,
};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use tokio::sync::{mpsc, oneshot};

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const MD_JS: &str = include_str!("../static/md.js");
const HL_JS: &str = include_str!("../static/hl.js");
const SUGGEST_JS: &str = include_str!("../static/suggest.js");
const STYLE_CSS: &str = include_str!("../static/style.css");
const ICON_SVG: &str = include_str!("../static/icon.svg");
const MANIFEST: &str = include_str!("../static/app.webmanifest");
const FAVICON_ICO: &[u8] = include_bytes!("../static/favicon.ico");
const APPLE_TOUCH_ICON: &[u8] = include_bytes!("../static/apple-touch-icon.png");
const ICON_192: &[u8] = include_bytes!("../static/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../static/icon-512.png");
const ICON_MASKABLE_512: &[u8] = include_bytes!("../static/icon-maskable-512.png");

const COOKIE: &str = "aipanel_session";
/// Сколько живёт вход в браузере. Столько же стоит Max-Age cookie.
const SESSION_LIFETIME: u64 = 30 * 24 * 60 * 60;
/// Допустимый разбег часов при проверке времени выдачи входа.
const SESSION_SKEW: u64 = 300;
const MAX_SESSIONS_PER_AGENT: usize = 500;
const MAX_TEXT: usize = 200_000;
const MAX_PROMPTS: usize = 50;
const MAX_COMMANDS: usize = 200;
const MAX_MODELS: usize = 600;
const MAX_ANSWER: usize = 64_000;
const PING_EVERY: Duration = Duration::from_secs(25);
const DEAD_AFTER: Duration = Duration::from_secs(90);
/// Сколько ждать картинку от агента. Старый агент команду не знает и не ответит вовсе.
const IMAGE_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_IMAGE_WAITS: usize = 32;

struct AgentEntry {
    conn: u64,
    tx: Option<mpsc::Sender<ServerMsg>>,
    os: String,
    version: String,
    sessions: Vec<SessionInfo>,
    /// Последние проверенные лимиты подписок на машине агента.
    limits: Vec<Limits>,
}

struct BrowserEntry {
    tx: mpsc::Sender<String>,
    /// Пользователь, вошедший в этом браузере.
    owner: String,
    /// (агент, сессия), ленту которой сейчас смотрит браузер.
    watching: Option<(String, String)>,
}

/// Команда браузера, ожидающая ответа агента.
struct Pending {
    browser: u64,
    /// id команды в браузере.
    id: String,
    owner: String,
    agent: String,
}

/// Картинка, которую HTTP-запрос браузера ждёт от агента.
struct ImageWait {
    owner: String,
    agent: String,
    tx: oneshot::Sender<Result<(String, Vec<u8>), String>>,
}

#[derive(Default)]
struct Inner {
    /// id запроса картинки -> HTTP-запрос, который её ждёт.
    images: HashMap<String, ImageWait>,
    /// Ключ — (пользователь, имя агента): одинаковые имена у разных пользователей не пересекаются.
    agents: BTreeMap<(String, String), AgentEntry>,
    browsers: HashMap<u64, BrowserEntry>,
    /// id команды на сервере -> ожидающая её команда браузера.
    pending: HashMap<String, Pending>,
    next_id: u64,
    fail_window: Option<Instant>,
    fail_count: u32,
}

impl Inner {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn agents_json(&self, owner: &str) -> String {
        let agents: Vec<_> = self
            .agents
            .iter()
            .filter(|((o, _), _)| o == owner)
            .map(|((_, name), a)| {
                json!({
                    "name": name,
                    "online": a.tx.is_some(),
                    "os": a.os,
                    "version": a.version,
                    "sessions": a.sessions,
                    "limits": a.limits,
                })
            })
            .collect();
        json!({ "t": "agents", "agents": agents }).to_string()
    }

    fn broadcast_agents(&self) {
        // Список агентов у каждого пользователя свой, поэтому JSON собирается на владельца.
        let mut sent: HashMap<&str, String> = HashMap::new();
        for b in self.browsers.values() {
            let msg = sent
                .entry(b.owner.as_str())
                .or_insert_with(|| self.agents_json(&b.owner));
            let _ = b.tx.try_send(msg.clone());
        }
    }

    fn watchers(&self, owner: &str, agent: &str, session: &str) -> usize {
        self.browsers
            .values()
            .filter(|b| {
                b.owner == owner && matches!(&b.watching, Some((a, s)) if a == agent && s == session)
            })
            .count()
    }

    /// Браузеру можно работать только с агентами своего пользователя.
    fn browser_owns(&self, browser: u64, agent: &str) -> bool {
        self.browsers
            .get(&browser)
            .is_some_and(|b| self.agents.contains_key(&(b.owner.clone(), agent.to_owned())))
    }

    fn to_agent(&self, owner: &str, agent: &str, msg: ServerMsg) -> bool {
        match self
            .agents
            .get(&(owner.to_owned(), agent.to_owned()))
            .and_then(|a| a.tx.as_ref())
        {
            Some(tx) => tx.try_send(msg).is_ok(),
            None => false,
        }
    }

    fn unwatch(&mut self, browser: u64) {
        let Some(b) = self.browsers.get_mut(&browser) else {
            return;
        };
        let owner = b.owner.clone();
        let Some((agent, session)) = b.watching.take() else {
            return;
        };
        if self.watchers(&owner, &agent, &session) == 0 {
            self.to_agent(&owner, &agent, ServerMsg::Unwatch { session });
        }
    }
}

struct User {
    password: String,
    /// Токен, с которым подключаются агенты этого пользователя.
    token: String,
}

struct App {
    inner: Mutex<Inner>,
    users: BTreeMap<String, User>,
}

impl App {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Пользователь по cookie браузера.
    fn authed(&self, headers: &HeaderMap) -> Option<String> {
        self.user_for_session(cookie(headers, COOKIE)?)
    }

    /// Пользователь по значению cookie. Подпись каждый раз проверяется по
    /// текущему `PANEL_USERS`, поэтому вход не зависит от памяти процесса:
    /// перезапуск и передеплой его не сбрасывают, а изменяемый диск не нужен.
    fn user_for_session(&self, value: &str) -> Option<String> {
        let mut parts = value.split('.');
        let name_hex = parts.next()?;
        let issued = parts.next()?.parse::<u64>().ok()?;
        parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        let now = unix_now();
        if issued > now.saturating_add(SESSION_SKEW) || now.saturating_sub(issued) > SESSION_LIFETIME {
            return None;
        }
        let name = String::from_utf8(hex_decode(name_hex)?).ok()?;
        let expected = session_cookie(self.users.get(&name)?, &name, issued);
        ct_eq(expected.as_bytes(), value.as_bytes()).then_some(name)
    }

    /// Пользователь по токену агента.
    fn user_for_token(&self, token: &str) -> Option<String> {
        let mut found = None;
        for (name, user) in &self.users {
            if ct_eq(user.token.as_bytes(), token.as_bytes()) {
                found = Some(name.clone());
            }
        }
        found
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

/// Ключ подписи входа: пароль и токен агента этого пользователя. По ним сервер
/// и узнаёт вход после перезапуска, поэтому менять их на ходу нельзя — смена
/// любого из двух завершает прежние входы пользователя.
fn session_key(user: &User) -> Vec<u8> {
    let mut key = Vec::with_capacity(user.password.len() + user.token.len() + 24);
    key.extend_from_slice(b"aipanel-session-v1\0");
    key.extend_from_slice(user.password.as_bytes());
    key.push(0);
    key.extend_from_slice(user.token.as_bytes());
    key
}

/// Значение cookie входа: `<hex(имя)>.<время выдачи>.<подпись>`. Ничего не
/// хранится на сервере, поэтому cookie остаётся действительной после
/// перезапуска и передеплоя. Срок жизни проверяется по времени выдачи.
fn session_cookie(user: &User, name: &str, issued: u64) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&session_key(user)).expect("HMAC accepts keys of any size");
    mac.update(name.as_bytes());
    mac.update(b"\0");
    mac.update(issued.to_string().as_bytes());
    format!(
        "{}.{issued}.{}",
        hex_encode(name.as_bytes()),
        hex_encode(&mac.finalize().into_bytes())
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    asset_bytes(content_type, body.as_bytes())
}

fn asset_bytes(content_type: &'static str, body: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (
                header::CONTENT_SECURITY_POLICY,
                // Картинки по ссылкам из ленты лежат на чужих сайтах, поэтому
                // img-src шире остального. Грузятся они только по клику на ссылку.
                "default-src 'self'; img-src 'self' data: blob: https: http:; frame-ancestors 'none'; base-uri 'none'",
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
    // Вход по одному полю: пользователь определяется по паролю. Перебираем всех,
    // чтобы время ответа не зависело от порядка пользователей в конфиге.
    let mut user = None;
    for (name, u) in &app.users {
        if ct_eq(body.password.as_bytes(), u.password.as_bytes()) {
            user = Some(name.clone());
        }
    }
    let Some(user) = user else {
        app.lock().fail_count += 1;
        tokio::time::sleep(Duration::from_millis(700)).await;
        return (StatusCode::UNAUTHORIZED, "неверный пароль").into_response();
    };
    let token = session_cookie(&app.users[&user], &user, unix_now());
    let secure = header_str(&headers, "x-forwarded-proto").is_some_and(|p| p.eq_ignore_ascii_case("https"));
    let cookie = format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_LIFETIME}{}",
        if secure { "; Secure" } else { "" }
    );
    let mut resp = StatusCode::NO_CONTENT.into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

async fn logout() -> Response {
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"))],
    )
        .into_response()
}

/// Версия выпуска для показа в панели: `major.minor` из версии воркспейса
/// (`Cargo.toml`, `[workspace.package]`). Патч-разряд не используется и не показывается.
fn release_version(full: &str) -> String {
    let mut parts = full.split('.');
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) => format!("{major}.{minor}"),
        _ => full.to_string(),
    }
}

async fn me(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    match app.authed(&headers) {
        Some(user) => {
            Json(json!({ "user": user, "version": release_version(env!("CARGO_PKG_VERSION")) }))
                .into_response()
        }
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

// ---------------------------------------------------------------- агент

async fn agent_ws(State(app): State<Arc<App>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let owner = header_str(&headers, "authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|t| app.user_for_token(t));
    let Some(owner) = owner else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    ws.max_message_size(8 << 20)
        .max_frame_size(8 << 20)
        .on_upgrade(move |socket| agent_conn(app, owner, socket))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
}

/// Идентификатор попадает в URL API opencode, поэтому принимаются только безопасные символы.
fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn agent_conn(app: Arc<App>, owner: String, socket: WebSocket) {
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
    let key = (owner.clone(), name.clone());
    let conn = {
        let mut inner = app.lock();
        let conn = inner.id();
        // Браузеры могли открыть ленту, пока агент был не в сети: возобновляем подписки.
        let mut watched: Vec<String> = inner
            .browsers
            .values()
            .filter(|b| b.owner == owner)
            .filter_map(|b| b.watching.as_ref())
            .filter(|(a, _)| *a == name)
            .map(|(_, s)| s.clone())
            .collect();
        watched.sort();
        watched.dedup();
        for session in watched {
            // Панель могла открыть сессию, пока агент был не в сети: избранное
            // она тогда не получила, поэтому список берём при подключении.
            let _ = tx.try_send(ServerMsg::Favorites { session: session.clone() });
            let _ = tx.try_send(ServerMsg::Watch { session });
        }
        let (sessions, limits) = inner
            .agents
            .remove(&key)
            .map(|a| (a.sessions, a.limits))
            .unwrap_or_default();
        inner.agents.insert(
            key.clone(),
            AgentEntry { conn, tx: Some(tx), os, version, sessions, limits },
        );
        inner.broadcast_agents();
        conn
    };
    eprintln!("agent connected: {name} ({owner})");

    let mut ping = tokio::time::interval(PING_EVERY);
    let mut last_rx = Instant::now();
    loop {
        tokio::select! {
            m = stream.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    last_rx = Instant::now();
                    if let Ok(msg) = serde_json::from_str::<AgentMsg>(t.as_str()) {
                        on_agent_msg(&app, &owner, &name, msg);
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
    if let Some(a) = inner.agents.get_mut(&key) {
        if a.conn == conn {
            a.tx = None;
            inner.broadcast_agents();
            eprintln!("agent disconnected: {name} ({owner})");
        }
    }
}

fn on_agent_msg(app: &App, owner: &str, agent: &str, msg: AgentMsg) {
    let mut inner = app.lock();
    match msg {
        AgentMsg::Hello { .. } => {}
        AgentMsg::Sessions { mut sessions } => {
            sessions.truncate(MAX_SESSIONS_PER_AGENT);
            if let Some(a) = inner.agents.get_mut(&(owner.to_owned(), agent.to_owned())) {
                a.sessions = sessions;
            }
            inner.broadcast_agents();
        }
        AgentMsg::Limits { mut limits } => {
            limits.truncate(4);
            if let Some(a) = inner.agents.get_mut(&(owner.to_owned(), agent.to_owned())) {
                a.limits = limits;
            }
            inner.broadcast_agents();
        }
        AgentMsg::History { session, reset, items, drop, more } => {
            let text = json!({
                "t": "history", "agent": agent, "session": session, "reset": reset,
                "items": items, "drop": drop, "more": more,
            })
            .to_string();
            for b in inner.browsers.values() {
                if b.owner == owner
                    && matches!(&b.watching, Some((a, s)) if a == agent && *s == session)
                {
                    let _ = b.tx.try_send(text.clone());
                }
            }
        }
        AgentMsg::Older { session, items, more, error } => {
            let text = json!({
                "t": "older", "agent": agent, "session": session,
                "items": items, "more": more, "error": error,
            })
            .to_string();
            for b in inner.browsers.values() {
                if b.owner == owner
                    && matches!(&b.watching, Some((a, s)) if a == agent && *s == session)
                {
                    let _ = b.tx.try_send(text.clone());
                }
            }
        }
        AgentMsg::Favorites { session, mut items } => {
            items.truncate(MAX_FAVORITES);
            let text = json!({
                "t": "favorites", "agent": agent, "session": session, "items": items,
            })
            .to_string();
            for b in inner.browsers.values() {
                if b.owner == owner
                    && matches!(&b.watching, Some((a, s)) if a == agent && *s == session)
                {
                    let _ = b.tx.try_send(text.clone());
                }
            }
        }
        AgentMsg::Prompts { session, mut prompts } => {
            prompts.truncate(MAX_PROMPTS);
            let text = json!({
                "t": "prompts", "agent": agent, "session": session, "prompts": prompts,
            })
            .to_string();
            for b in inner.browsers.values() {
                if b.owner == owner
                    && matches!(&b.watching, Some((a, s)) if a == agent && *s == session)
                {
                    let _ = b.tx.try_send(text.clone());
                }
            }
        }
        AgentMsg::Commands { session, mut commands } => {
            commands.truncate(MAX_COMMANDS);
            let text = json!({
                "t": "commands", "agent": agent, "session": session, "commands": commands,
            })
            .to_string();
            for b in inner.browsers.values() {
                if b.owner == owner
                    && matches!(&b.watching, Some((a, s)) if a == agent && *s == session)
                {
                    let _ = b.tx.try_send(text.clone());
                }
            }
        }
        AgentMsg::Models { session, mut models } => {
            models.truncate(MAX_MODELS);
            let text = json!({
                "t": "models", "agent": agent, "session": session, "models": models,
            })
            .to_string();
            for b in inner.browsers.values() {
                if b.owner == owner
                    && matches!(&b.watching, Some((a, s)) if a == agent && *s == session)
                {
                    let _ = b.tx.try_send(text.clone());
                }
            }
        }
        AgentMsg::ToolModels { id, mut models } => {
            models.truncate(MAX_MODELS);
            // Ответ на конкретный запрос браузера, а не рассылка наблюдателям:
            // окно «Новая сессия» не связано ни с одной сессией.
            let allowed = inner
                .pending
                .get(&id)
                .is_some_and(|p| p.owner == owner && p.agent == agent);
            if allowed {
                if let Some(p) = inner.pending.remove(&id) {
                    if let Some(b) = inner.browsers.get(&p.browser) {
                        let _ = b
                            .tx
                            .try_send(json!({ "t": "tool_models", "id": p.id, "models": models }).to_string());
                    }
                }
            }
        }
        AgentMsg::Result { id, ok, error, session } => {
            // Отвечать на команду может только тот агент, которому она отправлена.
            let allowed = inner
                .pending
                .get(&id)
                .is_some_and(|p| p.owner == owner && p.agent == agent);
            if allowed {
                if let Some(p) = inner.pending.remove(&id) {
                    if let Some(b) = inner.browsers.get(&p.browser) {
                        let _ = b.tx.try_send(
                            json!({ "t": "result", "id": p.id, "ok": ok, "error": error, "session": session })
                                .to_string(),
                        );
                    }
                }
            }
        }
        AgentMsg::Image { id, mime, data, error } => {
            // Картинку принимаем только от того агента, у которого её просили.
            let allowed = inner.images.get(&id).is_some_and(|w| w.owner == owner && w.agent == agent);
            if !allowed {
                return;
            }
            let Some(wait) = inner.images.remove(&id) else {
                return;
            };
            let result = match error {
                Some(e) => Err(e),
                None if !IMAGE_MIMES.contains(&mime.as_str()) => Err("неизвестный тип картинки".into()),
                None => match aipanel_proto::b64::decode(&data) {
                    Some(bytes) if bytes.len() as u64 <= MAX_IMAGE_BYTES => Ok((mime, bytes)),
                    Some(_) => Err("картинка слишком большая".into()),
                    None => Err("агент прислал повреждённые данные".into()),
                },
            };
            let _ = wait.tx.send(result);
        }
        AgentMsg::Screen { id, index, count, label, width, height, data } => {
            // Снимок идёт тому браузеру, который его попросил, и только от того
            // агента, которому команда была отправлена.
            let Some(p) = inner.pending.get(&id) else {
                return;
            };
            if p.owner != owner || p.agent != agent {
                return;
            }
            // Кривой ответ агента не пересылаем: панель по нему строит страницы.
            if count == 0 || count > MAX_SCREENS || index >= count || width == 0 || height == 0 {
                return;
            }
            // В base64 картинка вырастает на треть; больше сообщения WebSocket
            // агент всё равно отправить не смог бы.
            if data.len() > MAX_SCREEN_BYTES * 4 / 3 + 8 {
                return;
            }
            let (browser, orig_id, label) = (p.browser, p.id.clone(), truncate(label, 64));
            if let Some(b) = inner.browsers.get(&browser) {
                let _ = b.tx.try_send(
                    json!({
                        "t": "screen", "id": orig_id, "index": index, "count": count,
                        "label": label, "width": width, "height": height, "data": data,
                    })
                    .to_string(),
                );
            }
        }
        AgentMsg::Screens { id, error } => {
            let allowed = inner
                .pending
                .get(&id)
                .is_some_and(|p| p.owner == owner && p.agent == agent);
            if !allowed {
                return;
            }
            if let Some(p) = inner.pending.remove(&id) {
                if let Some(b) = inner.browsers.get(&p.browser) {
                    let text = json!({ "t": "screens", "id": p.id, "error": error }).to_string();
                    let _ = b.tx.try_send(text);
                }
            }
        }
    }
}

// ---------------------------------------------------------------- картинки

#[derive(Deserialize)]
struct ImageQuery {
    agent: String,
    session: String,
    path: String,
}

/// Картинка с машины агента по пути из ленты сессии.
async fn image(State(app): State<Arc<App>>, headers: HeaderMap, Query(q): Query<ImageQuery>) -> Response {
    let Some(owner) = app.authed(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let path = q.path.trim().to_owned();
    if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) || q.session.len() > 256 {
        return image_error(StatusCode::BAD_REQUEST, "некорректный путь");
    }
    let (tx, rx) = oneshot::channel();
    let id = {
        let mut inner = app.lock();
        if inner.images.len() >= MAX_IMAGE_WAITS {
            return image_error(StatusCode::TOO_MANY_REQUESTS, "слишком много картинок загружается сразу");
        }
        let id = format!("img:{}", inner.id());
        let msg = ServerMsg::ReadImage { id: id.clone(), session: q.session, path };
        if !inner.to_agent(&owner, &q.agent, msg) {
            return image_error(StatusCode::BAD_GATEWAY, "агент не в сети");
        }
        inner.images.insert(id.clone(), ImageWait { owner, agent: q.agent, tx });
        id
    };
    let result = tokio::time::timeout(IMAGE_TIMEOUT, rx).await;
    app.lock().images.remove(&id);
    match result {
        Ok(Ok(Ok((mime, bytes)))) => image_response(&mime, bytes),
        Ok(Ok(Err(e))) => image_error(StatusCode::NOT_FOUND, &e),
        _ => image_error(
            StatusCode::GATEWAY_TIMEOUT,
            "агент не ответил: возможно, его нужно обновить до версии с показом картинок",
        ),
    }
}

fn image_response(mime: &str, bytes: Vec<u8>) -> Response {
    let mut resp = bytes.into_response();
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=300"));
    // SVG, открытый отдельной вкладкой, мог бы выполнить скрипт на домене панели:
    // sandbox и пустой default-src это запрещают.
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; sandbox"),
    );
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("inline"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    resp
}

fn image_error(status: StatusCode, text: &str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        text.to_owned(),
    )
        .into_response()
}

// ---------------------------------------------------------------- браузер

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum BrowserMsg {
    Watch { agent: String, session: String },
    Unwatch,
    /// Догрузить более ранние сообщения ленты: агент ответит `older`.
    Older { agent: String, session: String },
    Send { id: String, agent: String, session: String, text: String },
    Interrupt { id: String, agent: String, session: String },
    Start {
        id: String,
        agent: String,
        tool: String,
        cwd: String,
        #[serde(default)]
        model: String,
        /// Разрешение создать каталог, которого ещё нет на машине агента.
        #[serde(default)]
        create: bool,
    },
    Handoff { id: String, agent: String, session: String, tool: String },
    CancelQueued { id: String, agent: String, session: String, queue: String },
    Delete {
        id: String,
        agent: String,
        session: String,
        #[serde(default)]
        close: bool,
    },
    /// Закрыть окно консоли сессии, ничего не удаляя.
    Close { id: String, agent: String, session: String },
    /// Переименовать сессию: имя меняется у самого инструмента, а не только в панели.
    Rename { id: String, agent: String, session: String, title: String },
    /// Попросить у агента команды сессии для списка, который открывается по `/`.
    Commands { agent: String, session: String },
    /// Попросить у агента модели для окна «Новая сессия».
    ToolModels { id: String, agent: String, tool: String, cwd: String },
    /// Попросить у агента модели сессии для окна выбора в `/models`.
    Models { agent: String, session: String },
    /// Переключить сессию на модель `provider`/`model`.
    SetModel { id: String, agent: String, session: String, provider: String, model: String },
    PromptReply {
        id: String,
        agent: String,
        session: String,
        prompt: String,
        kind: String,
        #[serde(default)]
        decision: String,
        #[serde(default)]
        answer: serde_json::Value,
        #[serde(default)]
        message: String,
        #[serde(default)]
        cancel: bool,
    },
    /// Снять экраны машины агента: в ответ придут `screen` на каждый экран и
    /// `screens` в конце.
    Screens { id: String, agent: String },
    /// Попросить у агента избранные сообщения сессии: они хранятся у него.
    Favorites { agent: String, session: String },
    /// Добавить сообщение `item` в избранное сессии (`on`) или убрать оттуда.
    Favorite {
        agent: String,
        session: String,
        item: String,
        on: bool,
        #[serde(default)]
        ts: i64,
        role: String,
        #[serde(default)]
        text: String,
    },
}

async fn browser_ws(State(app): State<Arc<App>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let Some(owner) = app.authed(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !origin_ok(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(1 << 20)
        .on_upgrade(move |socket| browser_conn(app, owner, socket))
}

async fn browser_conn(app: Arc<App>, owner: String, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<String>(512);
    let id = {
        let mut inner = app.lock();
        let id = inner.id();
        let _ = tx.try_send(inner.agents_json(&owner));
        inner.browsers.insert(id, BrowserEntry { tx, owner, watching: None });
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
    inner.pending.retain(|_, p| p.browser != id);
}

fn on_browser_msg(app: &App, browser: u64, msg: BrowserMsg) {
    let mut inner = app.lock();
    match msg {
        BrowserMsg::Watch { agent, session } => {
            if !inner.browser_owns(browser, &agent) {
                return;
            }
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            inner.unwatch(browser);
            if let Some(b) = inner.browsers.get_mut(&browser) {
                b.watching = Some((agent.clone(), session.clone()));
            }
            // Отправляем всегда: агент в ответ пришлёт свежую ленту целиком.
            inner.to_agent(&owner, &agent, ServerMsg::Watch { session });
        }
        BrowserMsg::Unwatch => inner.unwatch(browser),
        BrowserMsg::Older { agent, session } => {
            if !inner.browser_owns(browser, &agent) {
                return;
            }
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            // Старые сообщения одинаковы для всех, кто смотрит сессию: ответ
            // агента уходит всем наблюдателям, как и лента.
            inner.to_agent(&owner, &agent, ServerMsg::Older { session });
        }
        BrowserMsg::Commands { agent, session } => {
            if !inner.browser_owns(browser, &agent) {
                return;
            }
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            // Ответ агент присылает всем, кто смотрит эту сессию: список общий.
            inner.to_agent(&owner, &agent, ServerMsg::Commands { session });
        }
        BrowserMsg::Models { agent, session } => {
            if !inner.browser_owns(browser, &agent) {
                return;
            }
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            inner.to_agent(&owner, &agent, ServerMsg::Models { session });
        }
        BrowserMsg::SetModel { id, agent, session, provider, model } => {
            let provider = provider.trim().to_owned();
            let model = model.trim().to_owned();
            if provider.is_empty() || model.is_empty() || provider.len() > 200 || model.len() > 200 {
                reply(&inner, browser, &id, "некорректная модель");
                return;
            }
            let cmd = format!("{browser}:{}", inner.id());
            command(
                &mut inner,
                browser,
                id,
                &agent,
                cmd.clone(),
                ServerMsg::SetModel { id: cmd, session, provider, model },
            );
        }
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
        BrowserMsg::Start { id, agent, tool, cwd, model, create } => {
            let cwd = cwd.trim().to_owned();
            if !matches!(tool.as_str(), "claude" | "opencode") {
                reply(&inner, browser, &id, "неизвестный тип сессии");
                return;
            }
            if cwd.is_empty() || cwd.len() > 4096 || cwd.chars().any(char::is_control) {
                reply(&inner, browser, &id, "некорректный каталог");
                return;
            }
            let model = model.trim().to_owned();
            if model.len() > 200 || model.chars().any(char::is_control) {
                reply(&inner, browser, &id, "некорректная модель");
                return;
            }
            let cmd = format!("{browser}:{}", inner.id());
            command(
                &mut inner,
                browser,
                id,
                &agent,
                cmd.clone(),
                ServerMsg::Start { id: cmd, tool, cwd, model, create },
            );
        }
        BrowserMsg::ToolModels { id, agent, tool, cwd } => {
            if !matches!(tool.as_str(), "claude" | "opencode") {
                reply(&inner, browser, &id, "неизвестный тип сессии");
                return;
            }
            let cwd = cwd.trim().to_owned();
            if cwd.len() > 4096 || cwd.chars().any(char::is_control) {
                reply(&inner, browser, &id, "некорректный каталог");
                return;
            }
            let cmd = format!("{browser}:{}", inner.id());
            command(&mut inner, browser, id, &agent, cmd.clone(), ServerMsg::ToolModels { id: cmd, tool, cwd });
        }
        BrowserMsg::Handoff { id, agent, session, tool } => {
            if !matches!(tool.as_str(), "claude" | "opencode") {
                reply(&inner, browser, &id, "неизвестный тип сессии");
                return;
            }
            let cmd = format!("{browser}:{}", inner.id());
            command(&mut inner, browser, id, &agent, cmd.clone(), ServerMsg::Handoff { id: cmd, session, tool });
        }
        BrowserMsg::CancelQueued { id, agent, session, queue } => {
            if !safe_id(&queue) {
                reply(&inner, browser, &id, "некорректный идентификатор очереди");
                return;
            }
            let cmd = format!("{browser}:{}", inner.id());
            command(
                &mut inner,
                browser,
                id,
                &agent,
                cmd.clone(),
                ServerMsg::CancelQueued { id: cmd, session, queue },
            );
        }
        BrowserMsg::Delete { id, agent, session, close } => {
            let cmd = format!("{browser}:{}", inner.id());
            command(&mut inner, browser, id, &agent, cmd.clone(), ServerMsg::Delete { id: cmd, session, close });
        }
        BrowserMsg::Close { id, agent, session } => {
            let cmd = format!("{browser}:{}", inner.id());
            command(&mut inner, browser, id, &agent, cmd.clone(), ServerMsg::Close { id: cmd, session });
        }
        BrowserMsg::Rename { id, agent, session, title } => {
            let title = title.trim().to_owned();
            if title.is_empty() || title.chars().count() > aipanel_proto::MAX_TITLE {
                reply(&inner, browser, &id, "название сессии пустое или слишком длинное");
                return;
            }
            if title.chars().any(char::is_control) {
                reply(&inner, browser, &id, "в названии есть управляющие символы");
                return;
            }
            let cmd = format!("{browser}:{}", inner.id());
            command(&mut inner, browser, id, &agent, cmd.clone(), ServerMsg::Rename { id: cmd, session, title });
        }
        BrowserMsg::Screens { id, agent } => {
            let cmd = format!("{browser}:{}", inner.id());
            command(&mut inner, browser, id, &agent, cmd.clone(), ServerMsg::Screens { id: cmd });
        }
        BrowserMsg::Favorites { agent, session } => {
            if !inner.browser_owns(browser, &agent) {
                return;
            }
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            // Список общий для всех, кто смотрит сессию: ответ агент рассылает.
            inner.to_agent(&owner, &agent, ServerMsg::Favorites { session });
        }
        BrowserMsg::Favorite { agent, session, item, on, ts, role, text } => {
            if !inner.browser_owns(browser, &agent) {
                return;
            }
            if item.is_empty()
                || item.chars().count() > MAX_ITEM_ID
                || item.chars().any(char::is_control)
                || !matches!(role.as_str(), "user" | "assistant")
            {
                return;
            }
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            let text = truncate(text, MAX_FAVORITE_TEXT);
            inner.to_agent(
                &owner,
                &agent,
                ServerMsg::Favorite { session, item, on, ts: ts.max(0), role, text },
            );
        }
        BrowserMsg::PromptReply { id, agent, session, prompt, kind, decision, answer, message, cancel } => {
            if !safe_id(&prompt) {
                reply(&inner, browser, &id, "некорректный идентификатор запроса");
                return;
            }
            if !matches!(kind.as_str(), "permission" | "form") {
                reply(&inner, browser, &id, "неизвестный тип запроса");
                return;
            }
            if kind == "permission" && !matches!(decision.as_str(), "once" | "always" | "reject") {
                reply(&inner, browser, &id, "неизвестный ответ на разрешение");
                return;
            }
            if kind == "form" && !cancel && !answer.is_object() {
                reply(&inner, browser, &id, "ответ формы должен быть объектом");
                return;
            }
            if serde_json::to_string(&answer).map(|s| s.len()).unwrap_or(0) > MAX_ANSWER {
                reply(&inner, browser, &id, "слишком большой ответ формы");
                return;
            }
            if message.chars().count() > 4000 {
                reply(&inner, browser, &id, "слишком длинное сообщение к запросу");
                return;
            }
            let cmd = format!("{browser}:{}", inner.id());
            command(
                &mut inner,
                browser,
                id,
                &agent,
                cmd.clone(),
                ServerMsg::PromptReply { id: cmd, session, prompt, kind, decision, answer, message, cancel },
            );
        }
    }
}

fn command(inner: &mut Inner, browser: u64, orig_id: String, agent: &str, cmd: String, msg: ServerMsg) {
    if !inner.browser_owns(browser, agent) {
        reply(inner, browser, &orig_id, "нет доступа к этому агенту");
        return;
    }
    let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
        reply(inner, browser, &orig_id, "соединение с браузером потеряно");
        return;
    };
    if inner.to_agent(&owner, agent, msg) {
        inner.pending.insert(cmd, Pending { browser, id: orig_id, owner, agent: agent.to_owned() });
    } else {
        reply(inner, browser, &orig_id, "агент не в сети");
    }
}

fn reply(inner: &Inner, browser: u64, id: &str, error: &str) {
    if let Some(b) = inner.browsers.get(&browser) {
        let _ = b.tx.try_send(json!({ "t": "result", "id": id, "ok": false, "error": error }).to_string());
    }
}

/// Строка от агента не должна занимать сообщение целиком: подпись экрана
/// обрезается по числу символов, а не байтов, чтобы не разрезать UTF-8.
fn truncate(text: String, max: usize) -> String {
    if text.chars().count() <= max {
        text
    } else {
        text.chars().take(max).collect()
    }
}

// ---------------------------------------------------------------- запуск

#[derive(Deserialize)]
struct UserConfig {
    password: String,
    token: String,
}

/// Разбирает `PANEL_USERS`: JSON-объект «имя -> пароль и токен агента».
fn parse_users(raw: &str) -> Result<BTreeMap<String, User>, String> {
    if raw.trim().is_empty() {
        return Err(
            "PANEL_USERS is not set; expecting JSON like \
             {\"alice\":{\"password\":\"...\",\"token\":\"...\"}}"
                .into(),
        );
    }
    let parsed: BTreeMap<String, UserConfig> =
        serde_json::from_str(raw).map_err(|e| format!("PANEL_USERS is not valid JSON: {e}"))?;
    if parsed.is_empty() {
        return Err("PANEL_USERS has no users".into());
    }
    let mut users: BTreeMap<String, User> = BTreeMap::new();
    let mut tokens: HashMap<String, String> = HashMap::new();
    for (name, cfg) in parsed {
        if !valid_name(&name) {
            return Err(format!("PANEL_USERS: invalid user name {name:?}"));
        }
        let password = cfg.password.trim().to_owned();
        let token = cfg.token.trim().to_owned();
        if password.len() < 8 {
            return Err(format!("PANEL_USERS: password of {name} is shorter than 8 characters"));
        }
        if token.len() < 8 {
            return Err(format!("PANEL_USERS: token of {name} is shorter than 8 characters"));
        }
        // Вход идёт по одному паролю, поэтому по нему должен находиться ровно один пользователь.
        if let Some((other, _)) = users.iter().find(|(_, u)| u.password == password) {
            return Err(format!("PANEL_USERS: {name} and {other} have the same password"));
        }
        if let Some(other) = tokens.insert(token.clone(), name.clone()) {
            return Err(format!("PANEL_USERS: {name} and {other} have the same agent token"));
        }
        users.insert(name, User { password, token });
    }
    Ok(users)
}

fn load_users() -> BTreeMap<String, User> {
    match parse_users(&std::env::var("PANEL_USERS").unwrap_or_default()) {
        Ok(users) => users,
        Err(e) => {
            eprintln!("{e}; refusing to start");
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
        users: load_users(),
    });
    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());

    let router = Router::new()
        .route("/", get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }))
        .route("/app.js", get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }))
        .route("/md.js", get(|| async { asset("text/javascript; charset=utf-8", MD_JS) }))
        .route("/hl.js", get(|| async { asset("text/javascript; charset=utf-8", HL_JS) }))
        .route("/suggest.js", get(|| async { asset("text/javascript; charset=utf-8", SUGGEST_JS) }))
        .route("/style.css", get(|| async { asset("text/css; charset=utf-8", STYLE_CSS) }))
        .route("/icon.svg", get(|| async { asset("image/svg+xml", ICON_SVG) }))
        .route("/favicon.ico", get(|| async { asset_bytes("image/x-icon", FAVICON_ICO) }))
        .route("/apple-touch-icon.png", get(|| async { asset_bytes("image/png", APPLE_TOUCH_ICON) }))
        .route("/icon-192.png", get(|| async { asset_bytes("image/png", ICON_192) }))
        .route("/icon-512.png", get(|| async { asset_bytes("image/png", ICON_512) }))
        .route("/icon-maskable-512.png", get(|| async { asset_bytes("image/png", ICON_MASKABLE_512) }))
        .route("/app.webmanifest", get(|| async { asset("application/manifest+json", MANIFEST) }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/image", get(image))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_version_is_major_minor() {
        assert_eq!(release_version("1.0.0"), "1.0");
        assert_eq!(release_version("2.13.0"), "2.13");
        assert_eq!(release_version("3"), "3");
    }

    #[test]
    fn parses_users() {
        let raw = r#"{
            "alice": {"password": "alice-pass", "token": "alice-token"},
            "bob": {"password": "bob-pass", "token": "bob-token"}
        }"#;
        let users = parse_users(raw).unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users["alice"].password, "alice-pass");
        assert_eq!(users["bob"].token, "bob-token");
    }

    #[test]
    fn rejects_invalid_config() {
        assert!(parse_users("").is_err());
        assert!(parse_users("[]").is_err());
        assert!(parse_users("{}").is_err());
        assert!(parse_users(r#"{"": {"password": "long-enough", "token": "0123456789"}}"#).is_err());
        assert!(parse_users(r#"{"a": {"password": "short", "token": "0123456789"}}"#).is_err());
        assert!(parse_users(r#"{"a": {"password": "long-enough", "token": "short"}}"#).is_err());
    }

    #[test]
    fn rejects_duplicate_password_or_token() {
        let same_password = r#"{
            "alice": {"password": "same-pass", "token": "alice-token"},
            "bob": {"password": "same-pass", "token": "bob-token"}
        }"#;
        assert!(parse_users(same_password).is_err());

        let same_token = r#"{
            "alice": {"password": "alice-pass", "token": "same-token"},
            "bob": {"password": "bob-pass", "token": "same-token"}
        }"#;
        assert!(parse_users(same_token).is_err());
    }

    #[test]
    fn hex_roundtrip() {
        for s in ["alice", "имя", "a.b-c_d"] {
            assert_eq!(hex_decode(&hex_encode(s.as_bytes())).unwrap(), s.as_bytes());
        }
        assert_eq!(hex_decode("abc"), None);
        assert_eq!(hex_decode("zz"), None);
    }

    const SESSION_USERS: &str = r#"{
        "alice": {"password": "alice-pass", "token": "alice-token"},
        "bob": {"password": "bob-pass", "token": "bob-token"}
    }"#;

    fn app_with(raw: &str) -> App {
        App { inner: Mutex::new(Inner::default()), users: parse_users(raw).unwrap() }
    }

    #[test]
    fn session_survives_restart() {
        let cookie = {
            let app = app_with(SESSION_USERS);
            session_cookie(&app.users["alice"], "alice", unix_now())
        };
        // Новый экземпляр приложения — это и есть перезапуск после передеплоя.
        let restarted = app_with(SESSION_USERS);
        assert_eq!(restarted.user_for_session(&cookie).as_deref(), Some("alice"));
    }

    #[test]
    fn session_rejects_forgery_and_expiry() {
        let app = app_with(SESSION_USERS);
        let now = unix_now();
        let good = session_cookie(&app.users["alice"], "alice", now);
        assert_eq!(app.user_for_session(&good).as_deref(), Some("alice"));

        // Имя подменили, подпись осталась от alice.
        let forged = good.replacen(&hex_encode(b"alice"), &hex_encode(b"bob"), 1);
        assert_eq!(app.user_for_session(&forged), None, "принята подпись от другого имени");
        assert_eq!(app.user_for_session(&format!("{good}0")), None, "принята подправленная подпись");
        assert_eq!(app.user_for_session("мусор"), None);
        assert_eq!(
            app.user_for_session(&session_cookie(&app.users["alice"], "ghost", now)),
            None,
            "принят вход неизвестного пользователя"
        );

        let old = session_cookie(&app.users["alice"], "alice", now - SESSION_LIFETIME - 1);
        assert_eq!(app.user_for_session(&old), None, "принят просроченный вход");
        let future = session_cookie(&app.users["alice"], "alice", now + SESSION_SKEW + 60);
        assert_eq!(app.user_for_session(&future), None, "принят вход из будущего");
    }

    #[test]
    fn session_dies_with_rotated_credentials() {
        let now = unix_now();
        let cookie = {
            let app = app_with(SESSION_USERS);
            session_cookie(&app.users["alice"], "alice", now)
        };
        let changed = SESSION_USERS.replace("alice-pass", "alice-pass-2");
        assert_eq!(app_with(&changed).user_for_session(&cookie), None, "вход пережил смену пароля");
        let changed = SESSION_USERS.replace("alice-token", "alice-token-2");
        assert_eq!(app_with(&changed).user_for_session(&cookie), None, "вход пережил смену токена");
    }

    fn entry() -> AgentEntry {
        AgentEntry {
            conn: 1,
            tx: None,
            os: "linux".into(),
            version: "0.1".into(),
            sessions: Vec::new(),
            limits: Vec::new(),
        }
    }

    #[test]
    fn agents_json_is_scoped_to_owner() {
        let mut inner = Inner::default();
        inner.agents.insert(("alice".into(), "pc".into()), entry());
        inner.agents.insert(("bob".into(), "pc".into()), entry());

        let json: serde_json::Value = serde_json::from_str(&inner.agents_json("alice")).unwrap();
        let agents = json["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["name"], "pc");
    }

    #[test]
    fn browser_owns_only_its_agents() {
        let mut inner = Inner::default();
        inner.agents.insert(("alice".into(), "alice-pc".into()), entry());
        inner.agents.insert(("bob".into(), "bob-pc".into()), entry());
        let (tx, _rx) = mpsc::channel::<String>(1);
        inner.browsers.insert(
            1,
            BrowserEntry { tx, owner: "alice".into(), watching: None },
        );

        assert!(inner.browser_owns(1, "alice-pc"));
        assert!(!inner.browser_owns(1, "bob-pc"));
        assert!(!inner.browser_owns(2, "alice-pc"));
        // Одинаковое имя агента у разных пользователей не считается чужим.
        inner.agents.insert(("bob".into(), "alice-pc".into()), entry());
        assert!(inner.browser_owns(1, "alice-pc"));
    }

    #[test]
    fn commands_reach_only_watchers_of_that_session() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let (other, mut other_rx) = mpsc::channel::<String>(4);
        let mut inner = Inner::default();
        inner.browsers.insert(
            1,
            BrowserEntry {
                tx,
                owner: "alice".into(),
                watching: Some(("pc".into(), "opencode:s1".into())),
            },
        );
        inner.browsers.insert(
            2,
            BrowserEntry {
                tx: other,
                owner: "alice".into(),
                watching: Some(("pc".into(), "opencode:s2".into())),
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new() };

        on_agent_msg(
            &app,
            "alice",
            "pc",
            AgentMsg::Commands {
                session: "opencode:s1".into(),
                commands: vec![aipanel_proto::Command {
                    name: "init".into(),
                    description: "guided setup".into(),
                }],
            },
        );
        let msg = rx.try_recv().unwrap();
        assert!(msg.contains("\"t\":\"commands\"") && msg.contains("init"), "{msg}");
        assert!(other_rx.try_recv().is_err(), "список ушёл в чужую сессию");
    }

    #[test]
    fn favorites_reach_only_watchers_of_that_session() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let (other, mut other_rx) = mpsc::channel::<String>(4);
        let mut inner = Inner::default();
        inner.browsers.insert(
            1,
            BrowserEntry {
                tx,
                owner: "alice".into(),
                watching: Some(("pc".into(), "opencode:s1".into())),
            },
        );
        inner.browsers.insert(
            2,
            BrowserEntry {
                tx: other,
                owner: "alice".into(),
                watching: Some(("pc".into(), "opencode:s2".into())),
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new() };

        on_agent_msg(
            &app,
            "alice",
            "pc",
            AgentMsg::Favorites {
                session: "opencode:s1".into(),
                items: vec![aipanel_proto::Favorite {
                    id: "m1:part:0".into(),
                    ts: 5,
                    role: "assistant".into(),
                    text: "ответ в избранном".into(),
                }],
            },
        );
        let msg = rx.try_recv().unwrap();
        assert!(msg.contains("\"t\":\"favorites\"") && msg.contains("ответ в избранном"), "{msg}");
        assert!(other_rx.try_recv().is_err(), "избранное ушло в чужую сессию");
    }

    #[test]
    fn favorite_toggle_checks_role_and_truncates_text() {
        let (agent_tx, mut agent_rx) = mpsc::channel::<ServerMsg>(4);
        let (btx, _brx) = mpsc::channel::<String>(4);
        let mut inner = Inner::default();
        let mut agent = entry();
        agent.tx = Some(agent_tx);
        inner.agents.insert(("alice".into(), "pc".into()), agent);
        inner.browsers.insert(
            1,
            BrowserEntry {
                tx: btx,
                owner: "alice".into(),
                watching: Some(("pc".into(), "opencode:s1".into())),
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new() };

        let toggle = |role: &str, text: &str| BrowserMsg::Favorite {
            agent: "pc".into(),
            session: "opencode:s1".into(),
            item: "m1".into(),
            on: true,
            ts: -5,
            role: role.into(),
            text: text.into(),
        };
        // Служебная строка ленты в избранное не попадает: агенту ничего не уходит.
        on_browser_msg(&app, 1, toggle("tool_use", "вызов"));
        assert!(agent_rx.try_recv().is_err(), "служебное сообщение ушло агенту");

        on_browser_msg(&app, 1, toggle("user", &"я".repeat(MAX_FAVORITE_TEXT + 50)));
        match agent_rx.try_recv().unwrap() {
            ServerMsg::Favorite { item, on, ts, role, text, .. } => {
                assert_eq!(item, "m1");
                assert!(on);
                assert_eq!(ts, 0, "время раньше эпохи");
                assert_eq!(role, "user");
                assert_eq!(text.chars().count(), MAX_FAVORITE_TEXT);
            }
            other => panic!("агенту ушло не избранное: {other:?}"),
        }
    }

    #[test]
    fn older_messages_reach_only_watchers_of_that_session() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let (other, mut other_rx) = mpsc::channel::<String>(4);
        let mut inner = Inner::default();
        inner.browsers.insert(
            1,
            BrowserEntry {
                tx,
                owner: "alice".into(),
                watching: Some(("pc".into(), "opencode:s1".into())),
            },
        );
        inner.browsers.insert(
            2,
            BrowserEntry {
                tx: other,
                owner: "alice".into(),
                watching: Some(("pc".into(), "opencode:s2".into())),
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new() };

        on_agent_msg(
            &app,
            "alice",
            "pc",
            AgentMsg::Older {
                session: "opencode:s1".into(),
                items: vec![aipanel_proto::Item {
                    id: "m1:0".into(),
                    ts: 1,
                    role: "user".into(),
                    kind: "text".into(),
                    text: "первый".into(),
                    tool: None,
                }],
                more: true,
                error: None,
            },
        );
        let msg = rx.try_recv().unwrap();
        assert!(msg.contains("\"t\":\"older\"") && msg.contains("\"more\":true"), "{msg}");
        assert!(msg.contains("первый"), "{msg}");
        assert!(other_rx.try_recv().is_err(), "ранние сообщения ушли в чужую сессию");
    }

    #[test]
    fn older_request_goes_to_the_agent() {
        let (tx, mut rx) = mpsc::channel::<ServerMsg>(4);
        let (browser, _browser_rx) = mpsc::channel::<String>(1);
        let mut inner = Inner::default();
        inner.agents.insert(
            ("alice".into(), "pc".into()),
            AgentEntry { tx: Some(tx), ..entry() },
        );
        inner.browsers.insert(
            1,
            BrowserEntry {
                tx: browser,
                owner: "alice".into(),
                watching: Some(("pc".into(), "opencode:s1".into())),
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new() };

        on_browser_msg(
            &app,
            1,
            BrowserMsg::Older { agent: "pc".into(), session: "opencode:s1".into() },
        );
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMsg::Older { session } if session == "opencode:s1"), "агент не получил запрос");
    }

    #[test]
    fn agent_cannot_answer_foreign_command() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let mut inner = Inner::default();
        inner.browsers.insert(
            1,
            BrowserEntry { tx, owner: "alice".into(), watching: None },
        );
        inner.pending.insert(
            "srv:1".into(),
            Pending {
                browser: 1,
                id: "x".into(),
                owner: "alice".into(),
                agent: "pc".into(),
            },
        );
        let app = App {
            inner: Mutex::new(inner),
            users: BTreeMap::new(),
        };

        on_agent_msg(
            &app,
            "bob",
            "pc",
            AgentMsg::Result { id: "srv:1".into(), ok: true, error: None, session: None },
        );
        assert!(rx.try_recv().is_err(), "чужой агент получил ответ браузеру");
        assert_eq!(app.lock().pending.len(), 1, "ответ чужого агента снял ожидание");

        on_agent_msg(
            &app,
            "alice",
            "pc",
            AgentMsg::Result { id: "srv:1".into(), ok: true, error: None, session: None },
        );
        assert!(rx.try_recv().unwrap().contains("\"id\":\"x\""));
        assert!(app.lock().pending.is_empty());
    }

    #[test]
    fn image_answer_only_from_asked_agent_and_known_type() {
        let (tx, mut rx) = oneshot::channel();
        let mut inner = Inner::default();
        inner.images.insert("img:1".into(), ImageWait { owner: "alice".into(), agent: "pc".into(), tx });
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new() };
        let png = AgentMsg::Image {
            id: "img:1".into(),
            mime: "image/png".into(),
            data: aipanel_proto::b64::encode(b"\x89PNG"),
            error: None,
        };

        on_agent_msg(&app, "bob", "pc", png.clone());
        assert!(rx.try_recv().is_err(), "картинку принял чужой агент");
        on_agent_msg(&app, "alice", "pc", png);
        assert_eq!(rx.try_recv().unwrap(), Ok(("image/png".to_string(), b"\x89PNG".to_vec())));

        let (tx, mut rx) = oneshot::channel();
        app.lock().images.insert("img:2".into(), ImageWait { owner: "alice".into(), agent: "pc".into(), tx });
        on_agent_msg(
            &app,
            "alice",
            "pc",
            AgentMsg::Image { id: "img:2".into(), mime: "text/html".into(), data: "PGI+".into(), error: None },
        );
        assert!(rx.try_recv().unwrap().is_err(), "пропущен не-картиночный тип");
    }
}
