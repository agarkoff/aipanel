//! Вход в панель: пользователи из `PANEL_USERS`, cookie с подписью и
//! проверка, что WebSocket пришёл со своей страницы.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;

use crate::{ws_agent::valid_name, App, User};

pub(crate) const COOKIE: &str = "aipanel_session";
/// Сколько живёт вход в браузере. Столько же стоит Max-Age cookie.
pub(crate) const SESSION_LIFETIME: u64 = 30 * 24 * 60 * 60;
/// Допустимый разбег часов при проверке времени выдачи входа.
pub(crate) const SESSION_SKEW: u64 = 300;
/// Сравнение без раннего выхода, чтобы время ответа не выдавало совпавший префикс.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

pub(crate) fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// WebSocket из браузера принимается только со своей страницы.
pub(crate) fn origin_ok(headers: &HeaderMap) -> bool {
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
pub(crate) fn session_key(user: &User) -> Vec<u8> {
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
pub(crate) fn session_cookie(user: &User, name: &str, issued: u64) -> String {
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

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub(crate) fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}
#[derive(Deserialize)]
pub(crate) struct Login {
    password: String,
}

pub(crate) async fn login(State(app): State<Arc<App>>, headers: HeaderMap, Json(body): Json<Login>) -> Response {
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

pub(crate) async fn logout() -> Response {
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"))],
    )
        .into_response()
}

/// Версия выпуска для показа в панели: `major.minor` из версии воркспейса
/// (`Cargo.toml`, `[workspace.package]`). Патч-разряд не используется и не показывается.
pub(crate) fn release_version(full: &str) -> String {
    let mut parts = full.split('.');
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) => format!("{major}.{minor}"),
        _ => full.to_string(),
    }
}

pub(crate) async fn me(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    match app.authed(&headers) {
        Some(user) => {
            Json(json!({ "user": user, "version": release_version(env!("CARGO_PKG_VERSION")) }))
                .into_response()
        }
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}
#[derive(Deserialize)]
pub(crate) struct UserConfig {
    password: String,
    token: String,
}

/// Разбирает `PANEL_USERS`: JSON-объект «имя -> пароль и токен агента».
pub(crate) fn parse_users(raw: &str) -> Result<BTreeMap<String, User>, String> {
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

pub(crate) fn load_users() -> BTreeMap<String, User> {
    match parse_users(&std::env::var("PANEL_USERS").unwrap_or_default()) {
        Ok(users) => users,
        Err(e) => {
            eprintln!("{e}; refusing to start");
            std::process::exit(2);
        }
    }
}
