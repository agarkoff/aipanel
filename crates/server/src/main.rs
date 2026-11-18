//! Сервер панели: принимает исходящие подключения агентов, отдаёт веб-интерфейс
//! и пересылает сообщения между браузерами и агентами. Ленты сессий не хранит —
//! в памяти только списки сессий и активные подключения.

mod assets;
mod auth;
mod image;
mod push;
mod ws_agent;
mod ws_browser;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use aipanel_proto::{Limits, PushSub, ServerMsg, SessionInfo};
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
    /// Закреплённые сессии и свёрнутые проекты: состояние списка хранится у
    /// агента, сервер только передаёт его браузерам.
    pins: Vec<String>,
    closed: Vec<String>,
}

struct BrowserEntry {
    tx: mpsc::Sender<String>,
    /// Пользователь, вошедший в этом браузере.
    owner: String,
    /// (агент, сессия), ленту которой сейчас смотрит браузер.
    watching: Option<(String, String)>,
    /// Подагент, лента которого открыта вкладкой; `None` — показана основная.
    sub: Option<String>,
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
    /// Подписки устройств на push-уведомления по пользователям. Хранят их
    /// агенты, сервер собирает их при подключении агентов.
    push: HashMap<String, Vec<PushSub>>,
    /// (пользователь, агент, сессия), о которых устройствам уже отправлено
    /// уведомление: когда сессию откроют, эти плашки нужно убрать.
    notified: HashSet<(String, String, String)>,
    /// Адрес панели (`https://…`) по заголовку Origin браузера: контакт для
    /// VAPID, если `PANEL_VAPID_SUB` не задан.
    origin: Option<String>,
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
                    "pins": a.pins,
                    "closed": a.closed,
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

    /// Сколько браузеров смотрят ленту одного подагента.
    fn sub_watchers(&self, owner: &str, agent: &str, session: &str, sub: &str) -> usize {
        self.browsers
            .values()
            .filter(|b| {
                b.owner == owner
                    && matches!(&b.watching, Some((a, s)) if a == agent && s == session)
                    && b.sub.as_deref() == Some(sub)
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

    /// Сохранить или удалить подписку у всех агентов пользователя в сети.
    fn push_to_agents(&self, owner: &str, sub: &PushSub, on: bool) -> usize {
        let names: Vec<String> = self
            .agents
            .iter()
            .filter(|((o, _), a)| o == owner && a.tx.is_some())
            .map(|((_, name), _)| name.clone())
            .collect();
        names
            .iter()
            .filter(|name| self.to_agent(owner, name, ServerMsg::PushSub { sub: sub.clone(), on }))
            .count()
    }

    /// Забыть подписки, которых больше нет у push-службы.
    fn drop_push(&mut self, owner: &str, endpoints: &[String]) {
        let gone: Vec<PushSub> = match self.push.get_mut(owner) {
            Some(list) => {
                let (gone, keep) = list.drain(..).partition(|s| endpoints.contains(&s.endpoint));
                *list = keep;
                gone
            }
            None => return,
        };
        for sub in gone {
            self.push_to_agents(owner, &sub, false);
        }
    }

    /// Запомнить, что об уведомлении сессии устройствам уже сообщили.
    fn mark_notified(&mut self, owner: &str, agent: &str, session: &str) {
        self.notified
            .insert((owner.to_owned(), agent.to_owned(), session.to_owned()));
    }

    /// Сессию открыли: `true`, если её уведомление ещё висит на устройствах и
    /// его пора убрать. Повторно та же сессия отметки уже не даст.
    fn take_notified(&mut self, owner: &str, agent: &str, session: &str) -> bool {
        self.notified
            .remove(&(owner.to_owned(), agent.to_owned(), session.to_owned()))
    }

    fn unwatch(&mut self, browser: u64) {
        let Some(b) = self.browsers.get_mut(&browser) else {
            return;
        };
        let owner = b.owner.clone();
        let sub = b.sub.take();
        let Some((agent, session)) = b.watching.take() else {
            return;
        };
        // Вкладку подагента закрыли (или ушли из сессии): слежение за его лентой
        // больше не нужно, если её не смотрит никто другой.
        if let Some(sub) = sub {
            if self.sub_watchers(&owner, &agent, &session, &sub) == 0 {
                self.to_agent(&owner, &agent, ServerMsg::UnwatchSub { session: session.clone(), sub });
            }
        }
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
    /// Ключ VAPID из `PANEL_VAPID_KEY`; без него push-уведомлений нет.
    vapid: Option<push::Vapid>,
    /// Контакт для VAPID из `PANEL_VAPID_SUB` (`mailto:…` или `https://…`).
    vapid_contact: Option<String>,
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
    // `aipanel-server vapid-key` — новый ключ для PANEL_VAPID_KEY.
    if std::env::args().nth(1).as_deref() == Some("vapid-key") {
        let (private, public) = push::Vapid::generate();
        println!("PANEL_VAPID_KEY={private}");
        eprintln!("public key: {public}");
        return;
    }
    let app = Arc::new(App {
        inner: Mutex::new(Inner::default()),
        users: auth::load_users(),
        vapid: push::load_vapid(),
        vapid_contact: std::env::var("PANEL_VAPID_SUB").ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()),
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
        App { inner: Mutex::new(Inner::default()), users: parse_users(raw).unwrap(), vapid: None, vapid_contact: None }
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
            pins: Vec::new(),
            closed: Vec::new(),
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
    fn session_list_state_reaches_browsers() {
        let mut inner = Inner::default();
        inner.agents.insert(("alice".into(), "pc".into()), entry());
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };
        on_agent_msg(
            &app,
            "alice",
            "pc",
            AgentMsg::SessionList { pins: vec!["claude:a".into()], closed: vec!["dir:c:\\api".into()] },
        );
        let json: serde_json::Value = serde_json::from_str(&app.lock().agents_json("alice")).unwrap();
        assert_eq!(json["agents"][0]["pins"][0], "claude:a");
        assert_eq!(json["agents"][0]["closed"][0], "dir:c:\\api");
    }

    #[test]
    fn pin_and_collapse_apply_before_agent_answers() {
        let (tx, mut rx) = mpsc::channel::<String>(8);
        let mut inner = Inner::default();
        inner.agents.insert(("alice".into(), "pc".into()), AgentEntry { tx: None, ..entry() });
        inner.browsers.insert(1, BrowserEntry { tx, owner: "alice".into(), watching: None, sub: None });
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };
        let key = ("alice".to_string(), "pc".to_string());

        on_browser_msg(
            &app,
            1,
            BrowserMsg::PinSession { agent: "pc".into(), session: "claude:s1".into(), on: true },
        );
        // Панель видит закреп сразу: рассылка ушла раньше, чем ответил агент,
        // иначе следующий скан со старым состоянием снял бы закреп на глазах.
        let sent = rx.try_recv().expect("нет рассылки браузерам");
        assert!(sent.contains("claude:s1"), "{sent}");
        assert_eq!(app.lock().agents[&key].pins, ["claude:s1"]);

        on_browser_msg(
            &app,
            1,
            BrowserMsg::PinSession { agent: "pc".into(), session: "claude:s1".into(), on: false },
        );
        assert!(app.lock().agents[&key].pins.is_empty());

        on_browser_msg(
            &app,
            1,
            BrowserMsg::CollapseProject { agent: "pc".into(), project: "dir:c:\\api".into(), on: true },
        );
        assert_eq!(app.lock().agents[&key].closed, ["dir:c:\\api"]);
        on_browser_msg(
            &app,
            1,
            BrowserMsg::CollapseProject { agent: "pc".into(), project: "dir:c:\\api".into(), on: false },
        );
        assert!(app.lock().agents[&key].closed.is_empty());
    }

    #[test]
    fn browser_owns_only_its_agents() {
        let mut inner = Inner::default();
        inner.agents.insert(("alice".into(), "alice-pc".into()), entry());
        inner.agents.insert(("bob".into(), "bob-pc".into()), entry());
        let (tx, _rx) = mpsc::channel::<String>(1);
        inner.browsers.insert(
            1,
            BrowserEntry { tx, owner: "alice".into(), watching: None, sub: None },
        );

        assert!(inner.browser_owns(1, "alice-pc"));
        assert!(!inner.browser_owns(1, "bob-pc"));
        assert!(!inner.browser_owns(2, "alice-pc"));
        // Одинаковое имя агента у разных пользователей не считается чужим.
        inner.agents.insert(("bob".into(), "alice-pc".into()), entry());
        assert!(inner.browser_owns(1, "alice-pc"));
    }

    fn session(key: &str, status: &str) -> SessionInfo {
        serde_json::from_value(json!({
            "key": key, "tool": "claude", "title": format!("t-{key}"), "cwd": "/p",
            "status": status, "live": true, "updated_at": 0, "delivery": "console",
        }))
        .unwrap()
    }

    #[test]
    fn push_events_on_waiting_and_turn_end() {
        let old = [session("a", "busy"), session("b", "idle"), session("c", "busy"), session("d", "busy")];
        let new = [
            session("a", "idle"),     // ход кончился
            session("b", "waiting"),  // просит выбор
            session("c", "busy"),     // без изменений
            session("d", "stopped"),  // окно закрыли — не повод звать
            session("e", "waiting"),  // новая сессия: прошлого статуса нет
        ];
        let got: Vec<_> = crate::push::status_events(&old, &new).into_iter().map(|e| (e.session, e.waiting)).collect();
        assert_eq!(got, [("a".to_string(), false), ("b".to_string(), true)]);
        let payload: serde_json::Value =
            serde_json::from_str(&crate::push::payload("pc", &crate::push::status_events(&old, &new)[1])).unwrap();
        assert_eq!(payload["title"], "t-b");
        assert_eq!(payload["body"], "pc · сессия ждёт ответа");
        assert_eq!(payload["tag"], "aipanel:pc:b");
        assert_eq!(payload["session"], "b");
    }

    /// Открытие сессии снимает её уведомление с устройств ровно один раз:
    /// повторный `watch` той же сессии (переподключение, вторая вкладка) молчит.
    #[test]
    fn opening_a_session_takes_its_notice_once() {
        let mut inner = Inner::default();
        inner.mark_notified("alice", "pc", "opencode:s1");
        assert!(inner.take_notified("alice", "pc", "opencode:s1"));
        assert!(!inner.take_notified("alice", "pc", "opencode:s1"));
        // Чужая сессия и чужой пользователь отметку не снимают.
        inner.mark_notified("alice", "pc", "opencode:s1");
        assert!(!inner.take_notified("bob", "pc", "opencode:s1"));
        assert!(!inner.take_notified("alice", "pc", "opencode:s2"));
        assert!(inner.take_notified("alice", "pc", "opencode:s1"));
    }

    /// Просьба убрать плашку адресована тому же уведомлению: тег совпадает.
    #[test]
    fn dismiss_names_the_same_notice() {
        let e = crate::push::status_events(
            &[session("opencode:s1", "busy")],
            &[session("opencode:s1", "idle")],
        )
        .remove(0);
        let shown: serde_json::Value = serde_json::from_str(&crate::push::payload("pc", &e)).unwrap();
        let closed: serde_json::Value =
            serde_json::from_str(&crate::push::dismiss_payload("pc", "opencode:s1")).unwrap();
        assert_eq!(shown["tag"], closed["tag"]);
        assert_eq!(closed["close"], true);
    }

    fn push_sub(id: &str) -> PushSub {
        let mut key = vec![4u8];
        key.extend([5u8; 64]);
        PushSub {
            endpoint: format!("https://fcm.googleapis.com/fcm/send/{id}"),
            p256dh: aipanel_proto::b64url::encode(&key),
            auth: aipanel_proto::b64url::encode(&[3u8; 16]),
        }
    }

    /// Подписки собираются от всех агентов, а агенту, который был не в сети,
    /// уходят те, которых у него нет.
    #[test]
    fn push_subs_merge_across_agents() {
        let (tx, mut rx) = mpsc::channel::<ServerMsg>(8);
        let mut inner = Inner::default();
        inner.agents.insert(("alice".into(), "pc".into()), AgentEntry { tx: Some(tx), ..entry() });
        inner.push.insert("alice".into(), vec![push_sub("phone")]);
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };

        let mut bad = push_sub("evil");
        bad.endpoint = "https://example.com/x".into();
        on_agent_msg(&app, "alice", "pc", AgentMsg::PushSubs { subs: vec![push_sub("laptop"), bad] });
        let endpoints: Vec<_> = app.lock().push["alice"].iter().map(|s| s.endpoint.clone()).collect();
        assert_eq!(endpoints, [push_sub("phone").endpoint, push_sub("laptop").endpoint]);
        match rx.try_recv().unwrap() {
            ServerMsg::PushSub { sub, on } => assert!(on && sub == push_sub("phone")),
            other => panic!("{other:?}"),
        }
        assert!(rx.try_recv().is_err());
        // Без ключа VAPID смена статуса уведомлений не порождает.
        assert!(on_agent_msg(&app, "alice", "pc", AgentMsg::Sessions { sessions: vec![session("a", "busy")] }).is_none());
        assert!(on_agent_msg(&app, "alice", "pc", AgentMsg::Sessions { sessions: vec![session("a", "idle")] }).is_none());
    }

    #[test]
    fn dead_push_subs_are_dropped_everywhere() {
        let (tx, mut rx) = mpsc::channel::<ServerMsg>(8);
        let mut inner = Inner::default();
        inner.agents.insert(("alice".into(), "pc".into()), AgentEntry { tx: Some(tx), ..entry() });
        inner.push.insert("alice".into(), vec![push_sub("phone"), push_sub("old")]);
        inner.drop_push("alice", &[push_sub("old").endpoint]);
        assert_eq!(inner.push["alice"], [push_sub("phone")]);
        match rx.try_recv().unwrap() {
            ServerMsg::PushSub { sub, on } => assert!(!on && sub.endpoint == push_sub("old").endpoint),
            other => panic!("{other:?}"),
        }
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
                sub: None,
            },
        );
        inner.browsers.insert(
            2,
            BrowserEntry {
                tx: other,
                owner: "alice".into(),
                watching: Some(("pc".into(), "opencode:s2".into())),
                sub: None,
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };

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
                sub: None,
            },
        );
        inner.browsers.insert(
            2,
            BrowserEntry {
                tx: other,
                owner: "alice".into(),
                watching: Some(("pc".into(), "opencode:s2".into())),
                sub: None,
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };

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
                sub: None,
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };

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
                sub: None,
            },
        );
        inner.browsers.insert(
            2,
            BrowserEntry {
                tx: other,
                owner: "alice".into(),
                watching: Some(("pc".into(), "opencode:s2".into())),
                sub: None,
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };

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
    fn sub_history_reaches_only_the_open_subagent_tab() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let (main, mut main_rx) = mpsc::channel::<String>(4);
        let (other, mut other_rx) = mpsc::channel::<String>(4);
        let mut inner = Inner::default();
        inner.browsers.insert(
            1,
            BrowserEntry {
                tx,
                owner: "alice".into(),
                watching: Some(("pc".into(), "claude:s1".into())),
                sub: Some("a1".into()),
            },
        );
        // Та же сессия, но открыта основная лента: чужая вкладка сюда не идёт.
        inner.browsers.insert(
            2,
            BrowserEntry {
                tx: main,
                owner: "alice".into(),
                watching: Some(("pc".into(), "claude:s1".into())),
                sub: None,
            },
        );
        // Тот же id подагента, но другая сессия.
        inner.browsers.insert(
            3,
            BrowserEntry {
                tx: other,
                owner: "alice".into(),
                watching: Some(("pc".into(), "claude:s2".into())),
                sub: Some("a1".into()),
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };

        on_agent_msg(
            &app,
            "alice",
            "pc",
            AgentMsg::SubHistory {
                session: "claude:s1".into(),
                sub: "a1".into(),
                reset: true,
                items: vec![aipanel_proto::Item {
                    id: "s1:0".into(),
                    ts: 1,
                    role: "assistant".into(),
                    kind: "text".into(),
                    text: "итог подагента".into(),
                    tool: None,
                }],
                more: false,
                error: None,
            },
        );
        let msg = rx.try_recv().unwrap();
        assert!(msg.contains("\"t\":\"sub_history\"") && msg.contains("итог подагента"), "{msg}");
        assert!(msg.contains("\"sub\":\"a1\""), "{msg}");
        assert!(main_rx.try_recv().is_err(), "лента подагента ушла в основную вкладку");
        assert!(other_rx.try_recv().is_err(), "лента подагента ушла в чужую сессию");
    }

    #[test]
    fn watch_sub_is_forwarded_and_replaces_the_previous_tab() {
        let (tx, mut rx) = mpsc::channel::<ServerMsg>(4);
        let (browser, _browser_rx) = mpsc::channel::<String>(1);
        let mut inner = Inner::default();
        inner.agents.insert(("alice".into(), "pc".into()), AgentEntry { tx: Some(tx), ..entry() });
        inner.browsers.insert(
            1,
            BrowserEntry {
                tx: browser,
                owner: "alice".into(),
                watching: Some(("pc".into(), "claude:s1".into())),
                sub: None,
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };

        on_browser_msg(
            &app,
            1,
            BrowserMsg::WatchSub { agent: "pc".into(), session: "claude:s1".into(), sub: "a1".into() },
        );
        assert!(
            matches!(rx.try_recv().unwrap(), ServerMsg::WatchSub { sub, .. } if sub == "a1"),
            "агент не получил подписку на ленту подагента"
        );
        assert_eq!(app.lock().browsers.get(&1).and_then(|b| b.sub.clone()).as_deref(), Some("a1"));

        // Вкладку сменили: прежняя отписка уходит агенту до новой подписки.
        on_browser_msg(
            &app,
            1,
            BrowserMsg::WatchSub { agent: "pc".into(), session: "claude:s1".into(), sub: "a2".into() },
        );
        assert!(matches!(rx.try_recv().unwrap(), ServerMsg::UnwatchSub { sub, .. } if sub == "a1"));
        assert!(matches!(rx.try_recv().unwrap(), ServerMsg::WatchSub { sub, .. } if sub == "a2"));

        // Отмена вкладки, которая уже не открыта, ничего не трогает.
        on_browser_msg(
            &app,
            1,
            BrowserMsg::UnwatchSub { agent: "pc".into(), session: "claude:s1".into(), sub: "a1".into() },
        );
        assert!(rx.try_recv().is_err());

        // Ложная отмена текущей вкладки снимает подписку.
        on_browser_msg(
            &app,
            1,
            BrowserMsg::UnwatchSub { agent: "pc".into(), session: "claude:s1".into(), sub: "a2".into() },
        );
        assert!(matches!(rx.try_recv().unwrap(), ServerMsg::UnwatchSub { sub, .. } if sub == "a2"));
        assert!(app.lock().browsers.get(&1).is_some_and(|b| b.sub.is_none()));
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
                sub: None,
            },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };

        on_browser_msg(
            &app,
            1,
            BrowserMsg::Older { agent: "pc".into(), session: "opencode:s1".into() },
        );
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerMsg::Older { session } if session == "opencode:s1"), "агент не получил запрос");
    }

    #[test]
    fn move_is_forwarded_and_checks_cwd() {
        let (tx, mut rx) = mpsc::channel::<ServerMsg>(4);
        let (browser, mut browser_rx) = mpsc::channel::<String>(4);
        let mut inner = Inner::default();
        inner.agents.insert(("alice".into(), "pc".into()), AgentEntry { tx: Some(tx), ..entry() });
        inner.browsers.insert(
            1,
            BrowserEntry { tx: browser, owner: "alice".into(), watching: None, sub: None },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };

        // Пустой каталог до агента не доходит: панель сразу получает ошибку.
        on_browser_msg(
            &app,
            1,
            BrowserMsg::Move { id: "c1".into(), agent: "pc".into(), session: "opencode:s1".into(), cwd: "  ".into() },
        );
        assert!(rx.try_recv().is_err(), "пустой каталог ушёл агенту");
        let answer = browser_rx.try_recv().unwrap();
        assert!(answer.contains("\"ok\":false") && answer.contains("некорректный каталог"), "{answer}");

        // Каталог доходит до агента без пробелов вокруг: он его и проверяет на диске.
        on_browser_msg(
            &app,
            1,
            BrowserMsg::Move {
                id: "c2".into(),
                agent: "pc".into(),
                session: "opencode:s1".into(),
                cwd: "  C:\\work\\docs  ".into(),
            },
        );
        match rx.try_recv().unwrap() {
            ServerMsg::Move { session, cwd, .. } => {
                assert_eq!((session.as_str(), cwd.as_str()), ("opencode:s1", "C:\\work\\docs"))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn agent_cannot_answer_foreign_command() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let mut inner = Inner::default();
        inner.browsers.insert(
            1,
            BrowserEntry { tx, owner: "alice".into(), watching: None, sub: None },
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
            vapid: None,
            vapid_contact: None,
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
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };
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

    /// Распознанный текст — ответ на команду одного браузера: другому агенту и
    /// другому браузеру он достаться не должен, иначе диктовка одного человека
    /// оказалась бы в поле ввода другого.
    #[test]
    fn transcript_reaches_only_the_browser_that_asked() {
        let (tx, mut rx) = mpsc::channel::<String>(4);
        let mut inner = Inner::default();
        inner.browsers.insert(1, BrowserEntry { tx, owner: "alice".into(), watching: None, sub: None });
        inner.pending.insert(
            "srv:7".into(),
            Pending { browser: 1, id: "v".into(), owner: "alice".into(), agent: "pc".into() },
        );
        let app = App { inner: Mutex::new(inner), users: BTreeMap::new(), vapid: None, vapid_contact: None };

        // Тот же владелец, но не та машина: текст не пересылается.
        on_agent_msg(
            &app,
            "alice",
            "laptop",
            AgentMsg::Transcript { id: "srv:7".into(), text: "привет".into(), error: None },
        );
        assert!(rx.try_recv().is_err(), "текст от другого агента не пересылается");
        assert_eq!(app.lock().pending.len(), 1, "ответ другого агента не снимает ожидание");

        on_agent_msg(
            &app,
            "alice",
            "pc",
            AgentMsg::Transcript { id: "srv:7".into(), text: "запусти карго тест".into(), error: None },
        );
        let sent = rx.try_recv().unwrap();
        assert!(sent.contains("\"t\":\"transcript\""), "{sent}");
        assert!(sent.contains("\"id\":\"v\""), "{sent}");
        assert!(sent.contains("запусти карго тест"), "{sent}");
        assert!(app.lock().pending.is_empty(), "ожидание снято ответом агента");
    }
}
