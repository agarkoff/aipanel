//! Сервер панели: принимает исходящие подключения агентов, отдаёт веб-интерфейс
//! и пересылает сообщения между браузерами и агентами. Ленты сессий не хранит —
//! в памяти только списки сессий и активные подключения.

mod assets;
mod auth;
mod image;
mod ws_agent;
mod ws_browser;

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use aipanel_proto::{Limits, ServerMsg, SessionInfo};
use axum::{
    http::HeaderMap,
    routing::{get, post},
    Router,
};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use crate::auth::{
    cookie, ct_eq, hex_decode, session_cookie, unix_now, COOKIE, SESSION_LIFETIME, SESSION_SKEW,
};

const PING_EVERY: Duration = Duration::from_secs(25);
const DEAD_AFTER: Duration = Duration::from_secs(90);

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
        users: auth::load_users(),
    });
    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());

    let router = Router::new()
        .merge(assets::router())
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/login", post(auth::login))
        .route("/api/logout", post(auth::logout))
        .route("/api/me", get(auth::me))
        .route("/api/image", get(image::image))
        .route("/ws", get(ws_browser::browser_ws))
        .route("/agent", get(ws_agent::agent_ws))
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

    use aipanel_proto::{AgentMsg, MAX_FAVORITE_TEXT};

    use crate::auth::{hex_decode, hex_encode, parse_users, release_version, session_cookie};
    use crate::ws_agent::on_agent_msg;
    use crate::ws_browser::{on_browser_msg, BrowserMsg};

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
                    of: String::new(),
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
            of: String::new(),
        };
        // Служебная строка ленты в избранное не попадает: агенту ничего не уходит.
        on_browser_msg(&app, 1, toggle("tool_use", "вызов"));
        assert!(agent_rx.try_recv().is_err(), "служебное сообщение ушло агенту");
        // Фрагмент со ссылкой на сообщение с управляющими символами — тоже нет.
        let mut part = toggle("user", "кусок");
        if let BrowserMsg::Favorite { of, .. } = &mut part {
            *of = "m\n1".into();
        }
        on_browser_msg(&app, 1, part);
        assert!(agent_rx.try_recv().is_err(), "фрагмент с плохой ссылкой ушёл агенту");
        let mut part = toggle("assistant", "кусок");
        if let BrowserMsg::Favorite { of, .. } = &mut part {
            *of = "m1".into();
        }
        on_browser_msg(&app, 1, part);
        match agent_rx.try_recv().unwrap() {
            ServerMsg::Favorite { of, text, .. } => assert_eq!((of.as_str(), text.as_str()), ("m1", "кусок")),
            other => panic!("агенту ушло не избранное: {other:?}"),
        }

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
