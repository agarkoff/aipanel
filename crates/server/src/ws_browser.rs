//! Подключение браузера: команды панели уходят агенту, ответы агента
//! возвращаются тому браузеру, который их ждёт.

use std::{sync::Arc, time::Instant};

use aipanel_proto::{
    PushSub, ServerMsg, MAX_AUDIO_BYTES, MAX_DRAFT_TEXT, MAX_FAVORITE_TEXT, MAX_ITEM_ID, MAX_PROJECT_KEY,
    MAX_PUSH_SUBS,
};
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

use crate::{
    auth::{header_str, origin_ok},
    push,
    ws_agent::safe_id,
    App, BrowserEntry, Inner, Pending, DEAD_AFTER, PING_EVERY,
};

pub(crate) const MAX_TEXT: usize = 200_000;
pub(crate) const MAX_PROMPTS: usize = 50;
pub(crate) const MAX_COMMANDS: usize = 200;
pub(crate) const MAX_MODELS: usize = 600;
pub(crate) const MAX_DIRS: usize = 2000;
pub(crate) const MAX_ANSWER: usize = 64_000;
// ---------------------------------------------------------------- браузер

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub(crate) enum BrowserMsg {
    Watch { agent: String, session: String },
    Unwatch,
    /// Догрузить более ранние сообщения ленты: агент ответит `older`.
    Older { agent: String, session: String },
    /// Открыть ленту подагента `sub` вместо основной ленты сессии; агент будет
    /// присылать её, пока вкладка открыта.
    WatchSub { agent: String, session: String, sub: String },
    /// Лента подагента больше не нужна: ушли в основную ленту или закрыли вкладку.
    UnwatchSub { agent: String, session: String, sub: String },
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
    /// Переместить сессию в другой каталог: у opencode меняется её проект и
    /// `location.directory`, Claude Code ответит ошибкой.
    Move { id: String, agent: String, session: String, cwd: String },
    /// Попросить у агента команды сессии для списка, который открывается по `/`.
    Commands { agent: String, session: String },
    /// Попросить у агента модели для окна «Новая сессия».
    ToolModels { id: String, agent: String, tool: String, cwd: String },
    /// Попросить у агента содержимое каталога для окна «Новая сессия».
    /// Пустой `path` — корни дерева.
    Dirs { id: String, agent: String, path: String },
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
        /// Фрагмент: из какого сообщения выделен текст.
        #[serde(default)]
        of: String,
    },
    /// Подписать это устройство на push-уведомления: подписка из браузера.
    PushSubscribe { id: String, sub: PushSub },
    /// Отписать устройство с адресом подписки `endpoint`.
    PushUnsubscribe { id: String, endpoint: String },
    /// Сохранить недописанный текст сессии: он живёт у агента, поэтому черновик
    /// один на все устройства. Пустой `text` — черновика нет.
    Draft { agent: String, session: String, text: String },
    /// Закрепить сессию (`on`) или открепить её. Список хранится у агента.
    PinSession { agent: String, session: String, on: bool },
    /// Свернуть проект (`on`) или раскрыть его. Ключ проекта считает панель:
    /// `repo:<адрес>` или `dir:<путь>`.
    CollapseProject { agent: String, project: String, on: bool },
    /// Распознать голосовую запись `audio` (base64). Распознаёт агент на машине,
    /// где идёт сессия: сервер только передаёт запись, ничего не считая.
    Transcribe { id: String, agent: String, audio: String },
}

pub(crate) async fn browser_ws(State(app): State<Arc<App>>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let Some(owner) = app.authed(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !origin_ok(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // Адрес панели — контакт VAPID по умолчанию: Apple требует настоящий.
    if let Some(origin) = header_str(&headers, "origin").filter(|o| o.starts_with("https://") && o.len() <= 200) {
        app.lock().origin = Some(origin.to_owned());
    }
    ws.max_message_size(1 << 20)
        .on_upgrade(move |socket| browser_conn(app, owner, socket))
}

pub(crate) async fn browser_conn(app: Arc<App>, owner: String, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<String>(512);
    let id = {
        let mut inner = app.lock();
        let id = inner.id();
        let _ = tx.try_send(inner.agents_json(&owner));
        inner.browsers.insert(id, BrowserEntry { tx, owner, watching: None, sub: None });
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

pub(crate) fn on_browser_msg(app: &App, browser: u64, msg: BrowserMsg) {
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
            inner.to_agent(&owner, &agent, ServerMsg::Watch { session: session.clone() });
            // Черновик хранится у агента: при открытии сессии он приходит оттуда,
            // поэтому набранное на другом устройстве видно сразу.
            inner.to_agent(&owner, &agent, ServerMsg::Drafts { session: session.clone() });
            // Сессию открыли — с остальных устройств её уведомление пора убрать:
            // плашка в шторке телефона про уже прочитанное только мешает.
            if inner.take_notified(&owner, &agent, &session) {
                let subs = inner.push.get(&owner).cloned().unwrap_or_default();
                let contact = app.vapid_contact.clone().or_else(|| inner.origin.clone());
                push::dismiss(app.vapid.clone(), subs, contact, &agent, &session);
            }
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
        BrowserMsg::WatchSub { agent, session, sub } => {
            if !inner.browser_owns(browser, &agent) || !safe_id(&sub) {
                return;
            }
            // Подагента открывают у той сессии, лента которой показана.
            let watching_now = inner
                .browsers
                .get(&browser)
                .is_some_and(|b| matches!(&b.watching, Some((a, s)) if a == &agent && s == &session));
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            if !watching_now {
                return;
            }
            // У браузера открыт один подагент: прежний закрывается вместе с вкладкой.
            let old = inner.browsers.get_mut(&browser).and_then(|b| b.sub.replace(sub.clone()));
            if let Some(old) = old.filter(|o| *o != sub) {
                if inner.sub_watchers(&owner, &agent, &session, &old) == 0 {
                    inner.to_agent(&owner, &agent, ServerMsg::UnwatchSub { session: session.clone(), sub: old });
                }
            }
            inner.to_agent(&owner, &agent, ServerMsg::WatchSub { session, sub });
        }
        BrowserMsg::UnwatchSub { agent, session, sub } => {
            if !inner.browser_owns(browser, &agent) {
                return;
            }
            // Отмена относится только к той ленте, которая сейчас открыта: вкладку
            // могли сменить раньше, чем пришло это сообщение.
            let current = inner.browsers.get(&browser).is_some_and(|b| {
                matches!(&b.watching, Some((a, s)) if a == &agent && s == &session)
                    && b.sub.as_deref() == Some(sub.as_str())
            });
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            if !current {
                return;
            }
            if let Some(b) = inner.browsers.get_mut(&browser) {
                b.sub = None;
            }
            if inner.sub_watchers(&owner, &agent, &session, &sub) == 0 {
                inner.to_agent(&owner, &agent, ServerMsg::UnwatchSub { session, sub });
            }
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
        BrowserMsg::Dirs { id, agent, path } => {
            let path = path.trim().to_owned();
            if path.len() > 4096 || path.chars().any(char::is_control) {
                reply(&inner, browser, &id, "некорректный каталог");
                return;
            }
            let cmd = format!("{browser}:{}", inner.id());
            command(&mut inner, browser, id, &agent, cmd.clone(), ServerMsg::Dirs { id: cmd, path });
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
        BrowserMsg::Move { id, agent, session, cwd } => {
            let cwd = cwd.trim().to_owned();
            if cwd.is_empty() || cwd.len() > 4096 || cwd.chars().any(char::is_control) {
                reply(&inner, browser, &id, "некорректный каталог");
                return;
            }
            let cmd = format!("{browser}:{}", inner.id());
            command(&mut inner, browser, id, &agent, cmd.clone(), ServerMsg::Move { id: cmd, session, cwd });
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
        BrowserMsg::Favorite { agent, session, item, on, ts, role, text, of } => {
            if !inner.browser_owns(browser, &agent) {
                return;
            }
            let bad_id = |id: &str| id.is_empty() || id.chars().count() > MAX_ITEM_ID || id.chars().any(char::is_control);
            if bad_id(&item) || (!of.is_empty() && bad_id(&of)) || !matches!(role.as_str(), "user" | "assistant") {
                return;
            }
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            let text = truncate(text, MAX_FAVORITE_TEXT);
            inner.to_agent(
                &owner,
                &agent,
                ServerMsg::Favorite { session, item, on, ts: ts.max(0), role, text, of },
            );
        }
        BrowserMsg::Draft { agent, session, text } => {
            if !inner.browser_owns(browser, &agent) {
                return;
            }
            // Черновик не больше самого сообщения; обрезаем до отправки агенту,
            // чтобы длинный текст не раздувал его файл.
            let text = truncate(text, MAX_DRAFT_TEXT);
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            inner.to_agent(&owner, &agent, ServerMsg::Draft { session, text });
        }
        BrowserMsg::PinSession { agent, session, on } => {
            if !inner.browser_owns(browser, &agent) {
                return;
            }
            if session.is_empty() {
                return;
            }
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            // Список у сервера меняем сразу: сканы агента (каждые две секунды)
            // рассылают состояние всем браузерам, и без этого нажатие откатывалось
            // бы следующим же обновлением — со стороны это выглядело так, будто
            // новый закреп снимает прежний. Ответ агента придёт следом и поправит
            // сервер, если записать закреп не удалось.
            {
                let key = (owner.clone(), agent.clone());
                if let Some(entry) = inner.agents.get_mut(&key) {
                    let at = entry.pins.iter().position(|s| s == &session);
                    if on {
                        if let Some(i) = at {
                            entry.pins.remove(i);
                        }
                        entry.pins.insert(0, session.clone());
                    } else if let Some(i) = at {
                        entry.pins.remove(i);
                    }
                }
            }
            inner.broadcast_agents();
            // Ответ — состояние списка от агента, его получат все браузеры.
            inner.to_agent(&owner, &agent, ServerMsg::PinSession { session, on });
        }
        BrowserMsg::CollapseProject { agent, project, on } => {
            if !inner.browser_owns(browser, &agent) {
                return;
            }
            if project.is_empty()
                || project.chars().count() > MAX_PROJECT_KEY
                || project.chars().any(char::is_control)
            {
                return;
            }
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            // Свёрнутость тоже применяем сразу — по той же причине, что и закреп.
            {
                let key = (owner.clone(), agent.clone());
                if let Some(entry) = inner.agents.get_mut(&key) {
                    let at = entry.closed.iter().position(|p| p == &project);
                    if on {
                        if at.is_none() {
                            entry.closed.push(project.clone());
                        }
                    } else if let Some(i) = at {
                        entry.closed.remove(i);
                    }
                }
            }
            inner.broadcast_agents();
            inner.to_agent(&owner, &agent, ServerMsg::CollapseProject { project, on });
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
        BrowserMsg::Transcribe { id, agent, audio } => {
            // В base64 запись вырастает на треть: больше предела — это уже не
            // диктовка, а что-то, чего агент не ждёт.
            if audio.len() > MAX_AUDIO_BYTES * 4 / 3 + 8 {
                reply(&inner, browser, &id, "запись слишком длинная");
                return;
            }
            if audio.is_empty() {
                reply(&inner, browser, &id, "пустая запись");
                return;
            }
            let cmd = format!("{browser}:{}", inner.id());
            command(&mut inner, browser, id, &agent, cmd.clone(), ServerMsg::Transcribe { id: cmd, audio });
        }
        BrowserMsg::PushSubscribe { id, sub } => {
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            if app.vapid.is_none() {
                reply(&inner, browser, &id, "уведомления не настроены на сервере (нет PANEL_VAPID_KEY)");
                return;
            }
            if !sub.valid() {
                reply(&inner, browser, &id, "подписка не от известной push-службы");
                return;
            }
            // Подписку хранит агент: без агента в сети она пропала бы при перезапуске сервера.
            if inner.push_to_agents(&owner, &sub, true) == 0 {
                reply(&inner, browser, &id, "нет агента в сети: подписку негде сохранить");
                return;
            }
            let list = inner.push.entry(owner).or_default();
            match list.iter_mut().find(|s| s.endpoint == sub.endpoint) {
                Some(s) => *s = sub,
                None => list.push(sub),
            }
            if list.len() > MAX_PUSH_SUBS {
                let extra = list.len() - MAX_PUSH_SUBS;
                list.drain(..extra);
            }
            reply_ok(&inner, browser, &id);
        }
        BrowserMsg::PushUnsubscribe { id, endpoint } => {
            let Some(owner) = inner.browsers.get(&browser).map(|b| b.owner.clone()) else {
                return;
            };
            if let Some(list) = inner.push.get_mut(&owner) {
                list.retain(|s| s.endpoint != endpoint);
            }
            // Ключи для удаления не нужны: агент узнаёт подписку по адресу.
            let sub = PushSub { endpoint, p256dh: String::new(), auth: String::new() };
            inner.push_to_agents(&owner, &sub, false);
            reply_ok(&inner, browser, &id);
        }
    }
}

fn reply_ok(inner: &Inner, browser: u64, id: &str) {
    if let Some(b) = inner.browsers.get(&browser) {
        let _ = b.tx.try_send(json!({ "t": "result", "id": id, "ok": true }).to_string());
    }
}

pub(crate) fn command(inner: &mut Inner, browser: u64, orig_id: String, agent: &str, cmd: String, msg: ServerMsg) {
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

pub(crate) fn reply(inner: &Inner, browser: u64, id: &str, error: &str) {
    if let Some(b) = inner.browsers.get(&browser) {
        let _ = b.tx.try_send(json!({ "t": "result", "id": id, "ok": false, "error": error }).to_string());
    }
}

/// Строка от агента не должна занимать сообщение целиком: подпись экрана
/// обрезается по числу символов, а не байтов, чтобы не разрезать UTF-8.
pub(crate) fn truncate(text: String, max: usize) -> String {
    if text.chars().count() <= max {
        text
    } else {
        text.chars().take(max).collect()
    }
}
