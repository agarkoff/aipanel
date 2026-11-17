//! Подключение агента: один WebSocket на машину, сообщения от агента
//! раздаются браузерам, которые смотрят эти сессии.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use aipanel_proto::{
    AgentMsg, ServerMsg, IMAGE_MIMES, MAX_FAVORITES, MAX_IMAGE_BYTES, MAX_SCREENS, MAX_SCREEN_BYTES,
};
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use crate::{
    auth::header_str,
    ws_browser::{truncate, MAX_COMMANDS, MAX_DIRS, MAX_MODELS, MAX_PROMPTS},
    AgentEntry, App, DEAD_AFTER, PING_EVERY,
};

pub(crate) const MAX_SESSIONS_PER_AGENT: usize = 500;
// ---------------------------------------------------------------- агент

pub(crate) async fn agent_ws(State(app): State<Arc<App>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
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

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
}

/// Идентификатор попадает в URL API opencode, поэтому принимаются только безопасные символы.
pub(crate) fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub(crate) async fn agent_conn(app: Arc<App>, owner: String, socket: WebSocket) {
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

pub(crate) fn on_agent_msg(app: &App, owner: &str, agent: &str, msg: AgentMsg) {
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
        AgentMsg::Dirs { id, path, mut entries, error } => {
            entries.truncate(MAX_DIRS);
            // Ответ только тому браузеру, который открыл окно выбора папки.
            let allowed = inner
                .pending
                .get(&id)
                .is_some_and(|p| p.owner == owner && p.agent == agent);
            if allowed {
                if let Some(p) = inner.pending.remove(&id) {
                    if let Some(b) = inner.browsers.get(&p.browser) {
                        let _ = b.tx.try_send(
                            json!({
                                "t": "dirs", "id": p.id, "path": path,
                                "entries": entries, "error": error,
                            })
                            .to_string(),
                        );
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
