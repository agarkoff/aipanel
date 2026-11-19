//! Локальный агент AiPanel: сам подключается к серверу панели (исходящий WebSocket),
//! сообщает о сессиях Claude Code и opencode и выполняет команды из панели.

mod ask;
mod backend;
mod claude;
mod config;
mod dirs;
mod drafts;
mod favorites;
mod liststate;
mod handoff;
mod image;
mod install;
mod opencode;
mod platform;
mod push;
mod repo;
mod screen;
mod scroll;
mod status;
mod tasks;
mod util;
mod voice;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicUsize, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use aipanel_proto::{AgentMsg, Favorite, Limits, ServerMsg, SessionInfo, MAX_DRAFT_TEXT, MAX_ITEM_ID, MAX_PROJECT_KEY};
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

use crate::{backend::Backend, claude::Claude, config::Config, opencode::OpenCode};

struct Agent {
    cfg: Config,
    /// Инструменты, сессии которых показывает агент. Порядок — порядок в
    /// `agent.toml`: он же и в списке сессий.
    backends: Vec<Box<dyn Backend>>,
    out: mpsc::UnboundedSender<AgentMsg>,
    rescan: Arc<Notify>,
    /// Будит проверку лимитов: сразу после подключения к серверу.
    limits_now: Arc<Notify>,
    force_limits: AtomicBool,
    online: AtomicBool,
    force: AtomicBool,
    /// Наблюдение за лентой сессии: ключ — `tool:id`.
    watchers: Mutex<HashMap<String, JoinHandle<()>>>,
    /// Наблюдение за лентой подагента: ключ — (сессия, подагент).
    sub_watchers: Mutex<HashMap<(String, String), JoinHandle<()>>>,
    /// Каталог каждой известной сессии: от него считаются относительные пути картинок.
    cwds: Mutex<HashMap<String, String>>,
    /// Избранные сообщения сессий: файл `favorites.json` в каталоге агента.
    favorites: Mutex<favorites::Store>,
    /// Недописанные сообщения сессий: файл `drafts.json` в каталоге агента.
    /// Хранятся у агента, поэтому черновик общий для всех устройств.
    drafts: Mutex<drafts::Store>,
    /// Закреплённые сессии и свёрнутые проекты: файл `list.json` в каталоге
    /// агента. Тоже общие для всех устройств.
    liststate: Mutex<liststate::Store>,
    /// Подписки устройств на push-уведомления: файл `push.json` в каталоге агента.
    push: Mutex<push::Store>,
    /// Место чтения: положение прокрутки списка сессий и ленты каждой сессии.
    /// Файл `scroll.json` в каталоге агента. Тоже общее для всех устройств.
    scroll: Mutex<scroll::Store>,
    /// Голосовой ввод: распознавание записи на этой машине.
    voice: voice::Voice,
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

/// Что можно положить в избранное: сообщение человека или ответ агента и его
/// идентификатор без управляющих символов. id элемента ленты содержит `:`,
/// поэтому проверяется не как `safe_id`.
fn valid_favorite(id: &str, role: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= MAX_ITEM_ID
        && !id.chars().any(char::is_control)
        && matches!(role, "user" | "assistant")
}

impl Agent {
    fn handle(self: &Arc<Self>, msg: ServerMsg) {
        match msg {
            ServerMsg::Watch { session } => {
                let me = Arc::clone(self);
                let key = session.clone();
                let task = tokio::spawn(async move {
                    let out = me.out.clone();
                    if let Ok((backend, id)) = me.session(&key) {
                        backend.watch(key.clone(), id.to_owned(), out).await;
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
            ServerMsg::WatchSub { session, sub } => {
                if !safe_id(&sub) {
                    return;
                }
                let me = Arc::clone(self);
                let key = session.clone();
                let sub_key = (session, sub.clone());
                let task = tokio::spawn(async move {
                    let out = me.out.clone();
                    if let Ok((backend, id)) = me.session(&key) {
                        backend.watch_sub(key.clone(), id.to_owned(), sub, out).await;
                    }
                });
                if let Some(old) = self.sub_watchers.lock().unwrap().insert(sub_key, task) {
                    old.abort();
                }
            }
            ServerMsg::UnwatchSub { session, sub } => {
                if let Some(task) = self.sub_watchers.lock().unwrap().remove(&(session, sub)) {
                    task.abort();
                }
            }
            ServerMsg::Older { session } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result = match me.session(&session) {
                        Ok((backend, id)) => backend.older(id).await,
                        Err(e) => Err(e),
                    };
                    let msg = match result {
                        Ok((items, more)) => AgentMsg::Older { session, items, more, error: None },
                        Err(e) => AgentMsg::Older { session, items: Vec::new(), more: false, error: Some(e) },
                    };
                    let _ = me.out.send(msg);
                });
            }
            ServerMsg::PushSub { sub, on } => {
                let endpoint = sub.endpoint.clone();
                if let Err(e) = self.push.lock().unwrap().set(sub, on) {
                    util::log(&format!("push: {e}"));
                } else {
                    util::log(&format!("push: подписка {} {endpoint}", if on { "сохранена" } else { "удалена" }));
                }
            }
            ServerMsg::Favorites { session } => {
                let items = self.favorites.lock().unwrap().list(&session);
                let _ = self.out.send(AgentMsg::Favorites { session, items });
            }
            ServerMsg::Favorite { session, item, on, ts, role, text, of } => {
                // В избранном — только сообщения переписки: вызовы инструментов
                // и служебные строки туда не попадают.
                if !valid_favorite(&item, &role) || !(of.is_empty() || valid_favorite(&of, &role)) {
                    util::log(&format!("favorite: некорректное сообщение «{item}» ({role})"));
                    return;
                }
                let (items, error) = {
                    let mut store = self.favorites.lock().unwrap();
                    let error = store.set(&session, Favorite { id: item, ts, role, text, of }, on).err();
                    (store.list(&session), error)
                };
                if let Some(e) = error {
                    util::log(&format!("favorite: {e}"));
                }
                // Список уходит и при ошибке записи: в памяти он уже изменён, а
                // расходиться с показанными звёздами панели ему нельзя.
                let _ = self.out.send(AgentMsg::Favorites { session, items });
            }
            ServerMsg::Drafts { session } => {
                let text = self.drafts.lock().unwrap().get(&session);
                let _ = self.out.send(AgentMsg::Draft { session, text });
            }
            ServerMsg::Draft { session, text } => {
                // Ключ сессии попадает в файл черновиков: принимаются только
                // известные инструменты и безопасные идентификаторы.
                if split_key(&session).is_err() {
                    util::log(&format!("draft: некорректный ключ сессии {session}"));
                    return;
                }
                let text = if text.chars().count() > MAX_DRAFT_TEXT {
                    text.chars().take(MAX_DRAFT_TEXT).collect()
                } else {
                    text
                };
                let error = self.drafts.lock().unwrap().set(&session, text.clone(), util::now_ms()).err();
                if let Some(e) = error {
                    util::log(&format!("draft: {e}"));
                }
                // Текст уходит обратно всем наблюдателям, включая отправителя:
                // так все устройства показывают один и тот же черновик.
                let _ = self.out.send(AgentMsg::Draft { session, text });
            }
            ServerMsg::Scrolls { id, session } => {
                // Файл маленький и уже прочитан при запуске: отвечаем сразу,
                // чтобы место чтения пришло раньше самой ленты.
                let pos = self.scroll.lock().unwrap().session(&session);
                let _ = self.out.send(AgentMsg::Scroll { id, session, pos });
            }
            ServerMsg::ScrollSet { session, pos } => {
                // Ключ сессии попадает в файл: принимаются только известные
                // инструменты и безопасные идентификаторы.
                if split_key(&session).is_err() {
                    util::log(&format!("scroll: некорректный ключ сессии {session}"));
                    return;
                }
                let Some(pos) = pos.clean() else {
                    util::log(&format!("scroll: некорректное место чтения сессии {session}"));
                    return;
                };
                if let Err(e) = self.scroll.lock().unwrap().set_session(&session, pos, util::now_ms()) {
                    util::log(&format!("scroll: {e}"));
                }
            }
            ServerMsg::ListScroll { pos, ts } => {
                let Some(pos) = pos.clean() else {
                    util::log("scroll: некорректное место списка сессий");
                    return;
                };
                if let Err(e) = self.scroll.lock().unwrap().set_list(pos, ts) {
                    util::log(&format!("scroll: {e}"));
                }
            }
            ServerMsg::SessionList => {
                let (pins, closed) = self.liststate.lock().unwrap().list();
                let _ = self.out.send(AgentMsg::SessionList { pins, closed });
            }
            ServerMsg::PinSession { session, on } => {
                // Ключ сессии попадает в файл состояния: принимаются только
                // известные инструменты и безопасные идентификаторы.
                if split_key(&session).is_err() {
                    util::log(&format!("pin: некорректный ключ сессии {session}"));
                    return;
                }
                if let Err(e) = self.liststate.lock().unwrap().pin(&session, on) {
                    util::log(&format!("pin: {e}"));
                }
                // Состояние уходит сразу: в памяти оно уже изменено, и
                // расходиться с тем, что нажали в панели, ему нельзя.
                let (pins, closed) = self.liststate.lock().unwrap().list();
                let _ = self.out.send(AgentMsg::SessionList { pins, closed });
            }
            ServerMsg::CollapseProject { project, on } => {
                let bad = project.is_empty()
                    || project.chars().count() > MAX_PROJECT_KEY
                    || project.chars().any(char::is_control);
                if bad {
                    util::log(&format!("collapse: некорректный проект {project:?}"));
                    return;
                }
                if let Err(e) = self.liststate.lock().unwrap().collapse(&project, on) {
                    util::log(&format!("collapse: {e}"));
                }
                let (pins, closed) = self.liststate.lock().unwrap().list();
                let _ = self.out.send(AgentMsg::SessionList { pins, closed });
            }
            ServerMsg::Send { id, session, text } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let (result, queued) = match me.session(&session) {
                        Ok((backend, sid)) => {
                            let sent = backend.send(sid, &text, &me.out).await;
                            let queued = sent.as_ref().ok().and_then(|item| item.clone());
                            (sent.map(|_| None), queued)
                        }
                        Err(e) => (Err(e), None),
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
                            more: false,
                        });
                        // Только после отправки клиенту: иначе наблюдение могло бы убрать
                        // сообщение из ленты раньше, чем оно туда попало.
                        if let Ok((backend, sid)) = me.session(&session) {
                            backend.note_optimistic(sid, &item_id);
                        }
                    }
                    me.finish(id, result);
                });
            }
            ServerMsg::Commands { session } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let commands = match me.session(&session) {
                        Ok((backend, id)) => backend.commands(id).await,
                        Err(_) => Vec::new(),
                    };
                    let _ = me.out.send(AgentMsg::Commands { session, commands });
                });
            }
            ServerMsg::Models { session } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let models = match me.session(&session) {
                        Ok((backend, id)) => backend.models(id).await,
                        Err(_) => Vec::new(),
                    };
                    let _ = me.out.send(AgentMsg::Models { session, models });
                });
            }
            ServerMsg::SetModel { id, session, provider, model } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result = match me.session(&session) {
                        Ok((backend, sid)) => backend.set_model(sid, &provider, &model).await,
                        Err(e) => Err(e),
                    };
                    me.finish(id, result.map(|_| None));
                });
            }
            ServerMsg::Interrupt { id, session } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result = match me.session(&session) {
                        Ok((backend, sid)) => backend.interrupt(sid).await,
                        Err(e) => Err(e),
                    };
                    me.finish(id, result.map(|_| None));
                });
            }
            ServerMsg::ToolModels { id, tool, cwd } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let models = match me.tool(&tool) {
                        Ok(backend) => backend.launch_models(&cwd).await,
                        Err(_) => Vec::new(),
                    };
                    let _ = me.out.send(AgentMsg::ToolModels { id, models });
                });
            }
            ServerMsg::Dirs { id, path } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    // Каталог читается с диска: на сетевом диске это может
                    // занять секунды, и ждать этого в общем потоке нельзя.
                    let asked = path.clone();
                    let result = tokio::task::spawn_blocking(move || crate::dirs::list(&asked)).await;
                    let (entries, error) = match result {
                        Ok(Ok(entries)) => (entries, None),
                        Ok(Err(e)) => (Vec::new(), Some(e)),
                        Err(_) => (Vec::new(), Some("не удалось прочитать каталог".into())),
                    };
                    let _ = me.out.send(AgentMsg::Dirs { id, path, entries, error });
                });
            }
            ServerMsg::Start { id, tool, cwd, model, create } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    // Каталог проверяем здесь: `create` приходит только после
                    // того, как человек разрешил создать отсутствующий каталог.
                    let result = match crate::util::session_dir_with(&cwd, create) {
                        Err(e) => Err(e),
                        Ok(_) => match me.tool(&tool) {
                            Ok(backend) => backend.start(&cwd, &model).await,
                            Err(e) => Err(e),
                        },
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
                    let result = match me.session(&session) {
                        Ok((backend, sid)) => backend.delete(sid, close).await,
                        Err(e) => Err(e),
                    };
                    if result.is_ok() {
                        // Сессии больше нет: её избранное и черновик не к чему относить.
                        me.favorites.lock().unwrap().remove_session(&session);
                        me.drafts.lock().unwrap().remove_session(&session);
                        me.scroll.lock().unwrap().remove_session(&session);
                    }
                    me.finish(id, result.map(|_| None));
                });
            }
            ServerMsg::Close { id, session } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result = match me.session(&session) {
                        Ok((backend, sid)) => backend.close(sid).await,
                        Err(e) => Err(e),
                    };
                    me.finish(id, result.map(|_| None));
                });
            }
            ServerMsg::Rename { id, session, title } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let result = match me.session(&session) {
                        Ok((backend, sid)) => backend.rename(sid, &title).await,
                        Err(e) => Err(e),
                    };
                    me.finish(id, result.map(|_| None));
                });
            }
            ServerMsg::Move { id, session, cwd } => {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    // Каталог проверяем на машине агента: панель знает только
                    // каталоги из списка сессий, и он мог устареть.
                    let result = match crate::util::session_dir_with(&cwd, false) {
                        Err(e) => Err(e),
                        Ok(_) => match me.session(&session) {
                            Ok((backend, sid)) => backend.move_to(sid, &cwd).await,
                            Err(e) => Err(e),
                        },
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
            ServerMsg::Transcribe { id, audio } => {
                // Распознавание занимает секунды (а первый раз — ещё и скачивание
                // модели), поэтому уходит в отдельную задачу: лента и остальные
                // команды панели продолжают работать.
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    let msg = match me.voice.transcribe(&audio).await {
                        Ok(text) => AgentMsg::Transcript { id, text, error: None },
                        Err(e) => {
                            util::log(&format!("voice: {e}"));
                            AgentMsg::Transcript { id, text: String::new(), error: Some(e) }
                        }
                    };
                    let _ = me.out.send(msg);
                });
            }
            ServerMsg::Screens { id } => {
                // Съёмка и сжатие занимают заметное время: держим их вне цикла
                // сообщений, иначе панель не получит ленту, пока идёт снимок.
                let out = self.out.clone();
                tokio::task::spawn_blocking(move || match screen::capture() {
                    Ok(shots) => {
                        for shot in shots {
                            let msg = AgentMsg::Screen {
                                id: id.clone(),
                                index: shot.index,
                                count: shot.count,
                                label: shot.label,
                                width: shot.width,
                                height: shot.height,
                                data: aipanel_proto::b64::encode(&shot.png),
                            };
                            let _ = out.send(msg);
                        }
                        let _ = out.send(AgentMsg::Screens { id, error: None });
                    }
                    Err(e) => {
                        util::log(&format!("screenshot failed: {e}"));
                        let _ = out.send(AgentMsg::Screens { id, error: Some(e) });
                    }
                });
            }
        }
    }

    /// Инструмент сессии по её ключу `tool:id`.
    fn session<'a, 'k>(&'a self, key: &'k str) -> Result<(&'a dyn Backend, &'k str), String> {
        let (tool, id) = split_key(key)?;
        Ok((self.tool(tool)?, id))
    }

    /// Инструмент по имени: так панель обращается к нему и до появления сессии
    /// (модели для окна «Новая сессия», запуск).
    fn tool(&self, tool: &str) -> Result<&dyn Backend, String> {
        self.backends
            .iter()
            .map(|backend| backend.as_ref())
            .find(|backend| backend.tool() == tool)
            .ok_or_else(|| format!("неизвестный тип сессии: {tool}"))
    }

    /// Отвечает на запрос инструмента: разрешение, форма или её отмена.
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
        let (backend, id) = self.session(session)?;
        backend.reply_prompt(id, prompt, kind, decision, answer, message, cancel).await
    }

    /// Убирает сообщение из очереди opencode, пока оно не отправлено.
    async fn cancel_queued(&self, session: &str, queue: &str) -> Result<(), String> {
        if !safe_id(queue) {
            return Err("некорректный идентификатор очереди".into());
        }
        let (backend, id) = self.session(session)?;
        backend.cancel_queued(id, queue).await
    }

    /// Продолжает сессию в другом инструменте: собирает выжимку из её переписки
    /// и открывает новую сессию в том же каталоге.
    async fn handoff(&self, session: &str, tool: &str) -> Result<Option<String>, String> {
        let (source, id) = self.session(session)?;
        if source.tool() == tool {
            return Err("сессия уже открыта в этом инструменте".into());
        }
        let (cwd, text) = source.handoff_source(id).await?;
        let target = self.tool(tool)?;
        if !target.enabled() {
            return Err(format!("инструмент {tool} выключен в agent.toml"));
        }
        target.start_with_prompt(&cwd, Some(&text), "").await
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
        for (_, task) in self.sub_watchers.lock().unwrap().drain() {
            task.abort();
        }
    }
}

async fn scanner(agent: Arc<Agent>) {
    let mut last: Vec<SessionInfo> = Vec::new();
    loop {
        if agent.online.load(Ordering::Relaxed) {
            let mut list = Vec::new();
            for backend in &agent.backends {
                if backend.enabled() {
                    list.extend(backend.scan().await);
                }
            }
            // Репозитории читаются с диска (`.git` каждого каталога сессии):
            // такая работа уходит в отдельный поток, чтобы медленный диск не
            // останавливал ни пинги серверу, ни файл состояния.
            list = tokio::task::spawn_blocking(move || {
                for s in &mut list {
                    s.repo = repo::info(&s.cwd);
                }
                list
            })
            .await
            .unwrap_or_default();
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

/// Обычный интервал опроса лимитов.
const LIMITS_EVERY: Duration = Duration::from_secs(180);
/// Предел отступа после неудачных опросов.
const LIMITS_BACKOFF_MAX: Duration = Duration::from_secs(30 * 60);
/// Сколько держать последний удачный ответ, если источник перестал отвечать.
const LIMITS_KEEP_MS: i64 = 3 * 60 * 60 * 1000;

/// Один источник лимитов (Claude или OpenCode Go): последний удачный ответ и
/// когда спрашивать снова.
struct LimitSource {
    last: Option<Limits>,
    next: Instant,
    backoff: Duration,
}

impl LimitSource {
    fn new() -> Self {
        Self { last: None, next: Instant::now(), backoff: LIMITS_EVERY }
    }

    fn due(&self) -> bool {
        Instant::now() >= self.next
    }

    /// Учитывает результат опроса. Эндпоинт лимитов Anthropic часто отвечает
    /// 429, если спрашивать его подряд, поэтому после отказа интервал растёт
    /// вдвое (до получаса), а после успеха возвращается к обычному.
    fn record(&mut self, got: Option<Limits>) {
        match got {
            Some(l) => {
                self.last = Some(l);
                self.backoff = LIMITS_EVERY;
            }
            None => self.backoff = (self.backoff * 2).min(LIMITS_BACKOFF_MAX),
        }
        self.next = Instant::now() + self.backoff;
    }

    /// Что показать сейчас: последний удачный ответ, пока он не слишком стар.
    fn current(&self, now: i64) -> Option<Limits> {
        let l = self.last.as_ref().filter(|l| now - l.updated_at < LIMITS_KEEP_MS)?;
        Some(refresh_expired(l.clone(), now))
    }
}

/// Окно, чей сброс уже прошёл, снова полное: старый процент для него неверен,
/// а свежего ответа может не быть (источник не отвечает).
fn refresh_expired(mut l: Limits, now: i64) -> Limits {
    for m in &mut l.meters {
        if m.resets_at != 0 && m.resets_at <= now {
            m.left = 100;
            m.resets_at = 0;
        }
    }
    l
}

/// Раз в несколько минут проверяет остаток лимитов подписок на этой машине
/// и отправляет серверу, если он изменился.
///
/// Неудачный опрос не стирает известные лимиты: в панели остаётся последний
/// удачный ответ с его временем (`updated_at`), и панель показывает, что он
/// не свежий. Раньше отказ одного источника (например, 429 от Anthropic)
/// убирал его строку из списка, и панель писала «нет данных».
async fn limits_watcher(agent: Arc<Agent>) {
    // Свой последний удачный ответ и свой отступ — у каждого инструмента.
    let mut sources: HashMap<&'static str, LimitSource> = HashMap::new();
    let mut last: Vec<Limits> = Vec::new();
    loop {
        if agent.online.load(Ordering::Relaxed) {
            for backend in &agent.backends {
                if !backend.enabled() {
                    continue;
                }
                let source = sources.entry(backend.tool()).or_insert_with(LimitSource::new);
                if source.due() {
                    source.record(backend.limits().await);
                }
            }
            let now = util::now_ms();
            let list: Vec<Limits> = agent
                .backends
                .iter()
                .filter(|backend| backend.enabled())
                .filter_map(|backend| sources.get(backend.tool()).and_then(|source| source.current(now)))
                .collect();
            // После подключения сервер ничего не знает о лимитах: отправляем
            // список, даже если он не изменился.
            if agent.force_limits.swap(false, Ordering::Relaxed) || list != last {
                let _ = agent.out.send(AgentMsg::Limits { limits: list.clone() });
                last = list;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
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
    // Подписки на push-уведомления: сервер их не хранит и после перезапуска
    // узнаёт от агентов.
    let subs = AgentMsg::PushSubs { subs: agent.push.lock().unwrap().list() };
    if sink.send(text_msg(&subs)).await.is_err() {
        return;
    }
    // Состояние списка сессий (закреплённые и свёрнутые проекты) тоже живёт
    // у агента: так его видят все браузеры и устройства.
    let (pins, closed) = agent.liststate.lock().unwrap().list();
    if sink.send(text_msg(&AgentMsg::SessionList { pins, closed })).await.is_err() {
        return;
    }
    // Место прокрутки списка сессий тоже живёт у агента: после перезапуска
    // сервера он узнаёт его отсюда, а панель открывает список там же, где его
    // оставили.
    // Место прокрутки списка: сначала снимаем значение, и только потом шлём.
    // Держать блокировку через `await` нельзя: пока ответ не ушёл, на ней
    // встанут все задачи, которым нужен тот же мьютекс.
    let list_scroll = agent.scroll.lock().unwrap().list();
    if let Some((pos, ts)) = list_scroll {
        if sink.send(text_msg(&AgentMsg::ListScroll { pos, ts })).await.is_err() {
            return;
        }
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

/// Отметка жизни рантайма: её раз в секунду увеличивает `heartbeat`.
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);
/// Аргументы запуска: по ним сторож поднимает замену зависшему агенту.
static ARGS: OnceLock<Vec<String>> = OnceLock::new();

/// Отмечается раз в секунду. Если все потоки рантайма встали (например, на
/// залипшем диске или на мьютексе, который держит залипшая задача), отметок не
/// будет — это замечает `watchdog`.
async fn heartbeat() {
    loop {
        HEARTBEAT.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Сторож — обычный поток ОС, а не задача рантайма: он продолжает работать и
/// тогда, когда стоят все потоки рантайма. Зависший агент панели всё равно
/// бесполезен, поэтому сторож снимает дамп (по нему видно, на чём именно стоят
/// потоки) и поднимает вместо себя новый экземпляр.
fn watchdog() {
    const CHECK: Duration = Duration::from_secs(5);
    /// Девять проверок по 5 секунд — 45 секунд без отметок.
    const STALLED: u32 = 9;
    let mut seen = HEARTBEAT.load(Ordering::Relaxed);
    let mut stalled = 0u32;
    let mut last_check = Instant::now();
    loop {
        std::thread::sleep(CHECK);
        // Сторож сам не работал — значит, спала машина, а не агент.
        if last_check.elapsed() > CHECK * 3 {
            seen = HEARTBEAT.load(Ordering::Relaxed);
            stalled = 0;
            last_check = Instant::now();
            continue;
        }
        last_check = Instant::now();
        let beats = HEARTBEAT.load(Ordering::Relaxed);
        if beats != seen {
            seen = beats;
            stalled = 0;
            continue;
        }
        stalled += 1;
        if stalled < STALLED {
            continue;
        }
        let path = util::aipanel_dir().join("hang.dmp");
        match platform::dump_self(&path) {
            Ok(()) => util::log(&format!("сторож: агент не отвечал 45 с, дамп {}", path.display())),
            Err(e) => util::log(&format!("сторож: агент не отвечал 45 с, дамп не снят: {e}")),
        }
        restart_self();
        std::process::exit(4);
    }
}

/// Запускает новый экземпляр агента с теми же аргументами и выходит. Файл
/// состояния от зависшего процесса выглядит свежим, поэтому его нужно убрать:
/// иначе новый экземпляр решит, что агент уже работает, и завершится.
fn restart_self() {
    let (Ok(exe), Some(args)) = (std::env::current_exe(), ARGS.get()) else {
        return;
    };
    let _ = std::fs::remove_file(util::aipanel_dir().join("agent.json"));
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    platform::detach(&mut cmd);
    if let Err(e) = cmd.spawn() {
        util::log(&format!("сторож: не удалось запустить замену: {e}"));
    }
}

async fn run(cfg: Config) {
    let (out, mut rx) = mpsc::unbounded_channel();
    let rescan = Arc::new(Notify::new());
    let limits_now = Arc::new(Notify::new());
    let agent = Arc::new(Agent {
        backends: vec![
            Box::new(Arc::new(Claude::new(cfg.claude.clone(), Arc::clone(&rescan)))) as Box<dyn Backend>,
            Box::new(Arc::new(OpenCode::new(cfg.opencode.clone()))) as Box<dyn Backend>,
        ],
        voice: voice::Voice::new(&cfg.voice),
        cfg,
        out,
        rescan,
        limits_now,
        force_limits: AtomicBool::new(false),
        online: AtomicBool::new(false),
        force: AtomicBool::new(false),
        watchers: Mutex::new(HashMap::new()),
        sub_watchers: Mutex::new(HashMap::new()),
        cwds: Mutex::new(HashMap::new()),
        favorites: Mutex::new(favorites::Store::open()),
        drafts: Mutex::new(drafts::Store::open()),
        liststate: Mutex::new(liststate::Store::open()),
        push: Mutex::new(push::Store::open()),
        scroll: Mutex::new(scroll::Store::open()),
        started: util::now_ms(),
        connected_since: AtomicI64::new(0),
        sessions: AtomicUsize::new(0),
        last_error: Mutex::new(String::new()),
    });
    tokio::spawn(scanner(Arc::clone(&agent)));
    tokio::spawn(limits_watcher(Arc::clone(&agent)));
    tokio::spawn(status_writer(Arc::clone(&agent)));
    tokio::spawn(heartbeat());

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

    // Снимок экрана одинаков на всех режимах: координаты и размеры мониторов
    // нужны настоящие, а не уменьшенные масштабом Windows.
    screen::enable_dpi_awareness();

    // Второй экземпляр агента (например, для удалённой панели) живёт в своём
    // каталоге состояния: так его лог, pid-файл и статус не пересекаются с
    // основным. Задаётся флагом `--home` или переменной AIPANEL_HOME.
    if let Some(home) = arg_value(&args, "--home") {
        util::set_aipanel_dir(PathBuf::from(home));
    }

    // Агент запущен без окна: паника уходит в невидимый stderr и пропадает
    // вместе с задачей, которая упала. Пишем её в `agent.log`, иначе причину
    // «выключившегося» агента потом не найти.
    std::panic::set_hook(Box::new(|info| util::log(&format!("panic: {info}"))));

    // Внутренний режим: ввод в консоль другого процесса (см. platform.rs).
    if args.first().map(String::as_str) == Some("inject") {
        let Some(pid) = args.get(1).and_then(|p| p.parse::<u32>().ok()) else {
            eprintln!("usage: aipanel-agent inject <pid> [--esc | --keys]");
            std::process::exit(2);
        };
        let mode = if args.iter().any(|a| a == "--esc") {
            platform::InjectMode::Esc
        } else if args.iter().any(|a| a == "--keys") {
            platform::InjectMode::Keys
        } else {
            platform::InjectMode::Text
        };
        if let Err(e) = platform::inject_main(pid, mode) {
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
    platform::set_terminal_window(cfg.terminal_new_window());
    if cfg.server.starts_with("ws://") && !cfg.server.contains("://localhost") && !cfg.server.contains("://127.0.0.1") {
        util::log("warning: ws:// is unencrypted, the agent token is sent in clear text; use wss:// outside localhost");
    }

    // Сторож живёт отдельным потоком ОС: задача рантайма зависла бы вместе с ним.
    let _ = ARGS.set(args.clone());
    if let Err(e) = std::thread::Builder::new().name("aipanel-watchdog".into()).spawn(watchdog) {
        util::log(&format!("сторож не запустился: {e}"));
    }

    // Потоков несколько: агент читает файлы сессий и каталоги репозиториев, а
    // такая работа на одном потоке останавливала и пинги серверу, и файл
    // состояния — сервер считал агента пропавшим через 90 секунд молчания.
    // Восемь — с запасом: наблюдение за каждой открытой лентой тоже читает
    // файлы, и несколько таких чтений не должны занимать все потоки.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .thread_name("aipanel-agent")
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(run(cfg));
}

#[cfg(test)]
mod tests {
    use super::*;
    use aipanel_proto::Meter;

    fn limits(updated_at: i64, resets_at: i64) -> Limits {
        Limits {
            tool: "claude".into(),
            title: "Claude Code".into(),
            meters: vec![Meter { label: "5 ч".into(), left: 0, resets_at }],
            updated_at,
        }
    }

    #[test]
    fn failed_poll_keeps_last_limits_and_backs_off() {
        let mut s = LimitSource::new();
        s.record(Some(limits(1_000, 0)));
        s.record(None);
        assert_eq!(s.backoff, LIMITS_EVERY * 2);
        s.record(None);
        s.record(None);
        s.record(None);
        assert_eq!(s.backoff, LIMITS_BACKOFF_MAX);
        assert_eq!(s.current(2_000).map(|l| l.meters[0].left), Some(0));
        s.record(Some(limits(3_000, 0)));
        assert_eq!(s.backoff, LIMITS_EVERY);
    }

    #[test]
    fn stale_limits_are_dropped() {
        let mut s = LimitSource::new();
        s.record(Some(limits(0, 0)));
        assert!(s.current(LIMITS_KEEP_MS - 1).is_some());
        assert!(s.current(LIMITS_KEEP_MS).is_none());
    }

    #[test]
    fn passed_reset_shows_full_window() {
        let l = refresh_expired(limits(0, 5_000), 5_000);
        assert_eq!((l.meters[0].left, l.meters[0].resets_at), (100, 0));
        let l = refresh_expired(limits(0, 5_000), 4_999);
        assert_eq!((l.meters[0].left, l.meters[0].resets_at), (0, 5_000));
    }
}
