//! Общий интерфейс инструмента: агент и панель работают с сессией, не зная,
//! чья она — Claude Code или opencode. Каждый инструмент реализует трейт у себя,
//! поэтому третий добавляется одной строкой в сборке бэкендов (`run`).
//!
//! Методы возвращают `Fut`, а не `async fn`: трейт должен быть пригоден для
//! `dyn Backend`, а асинхронный метод в трейте объектным не является. Работы
//! это не добавляет — команда из панели и так идёт через `await`.

use std::{future::Future, pin::Pin};

use aipanel_proto::{AgentMsg, Command, Item, Limits, ModelInfo, SessionInfo};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

/// Будущее метода инструмента. Живёт не дольше аргументов.
pub type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Backend: Send + Sync {
    /// Имя инструмента в ключе сессии (`tool:id`) и в `agent.toml`.
    fn tool(&self) -> &'static str;
    /// Включён ли инструмент в `agent.toml`.
    fn enabled(&self) -> bool;

    /// Сессии этой машины для списка в панели.
    fn scan(&self) -> Fut<'_, Vec<SessionInfo>>;
    /// Остаток лимитов подписки, если инструмент умеет его узнавать.
    fn limits(&self) -> Fut<'_, Option<Limits>>;

    /// Следит за лентой сессии, пока задачу наблюдения не отменят.
    fn watch(&self, key: String, id: String, out: UnboundedSender<AgentMsg>) -> Fut<'_, ()>;
    /// Следит за лентой подагента `sub` сессии `id`; `key` — ключ сессии.
    fn watch_sub(&self, key: String, id: String, sub: String, out: UnboundedSender<AgentMsg>) -> Fut<'_, ()>;
    /// Страница ранних сообщений; `more` — остались ли ещё более старые.
    fn older<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(Vec<Item>, bool), String>>;
    /// Сообщение в сессию. `Some` возвращает элемент, который панель показывает
    /// сразу: opencode принимает сообщение в очередь, и в ленте оно появится
    /// только после текущего ответа.
    fn send<'a>(
        &'a self,
        id: &'a str,
        text: &'a str,
        out: &'a UnboundedSender<AgentMsg>,
    ) -> Fut<'a, Result<Option<Item>, String>>;
    fn commands<'a>(&'a self, id: &'a str) -> Fut<'a, Vec<Command>>;
    fn models<'a>(&'a self, id: &'a str) -> Fut<'a, Vec<ModelInfo>>;
    /// Модели для окна новой сессии: сессии ещё нет, но каталог уже известен.
    fn launch_models<'a>(&'a self, cwd: &'a str) -> Fut<'a, Vec<ModelInfo>>;
    fn set_model<'a>(&'a self, id: &'a str, provider: &'a str, model: &'a str) -> Fut<'a, Result<(), String>>;
    fn interrupt<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(), String>>;
    fn rename<'a>(&'a self, id: &'a str, title: &'a str) -> Fut<'a, Result<(), String>>;
    fn delete<'a>(&'a self, id: &'a str, close: bool) -> Fut<'a, Result<(), String>>;
    fn close<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(), String>>;
    fn start<'a>(&'a self, cwd: &'a str, model: &'a str) -> Fut<'a, Result<Option<String>, String>>;
    fn start_with_prompt<'a>(
        &'a self,
        cwd: &'a str,
        prompt: Option<&'a str>,
        model: &'a str,
    ) -> Fut<'a, Result<Option<String>, String>>;
    /// Каталог и выжимка из переписки: с ними сессия продолжается в другом инструменте.
    fn handoff_source<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(String, String), String>>;
    /// Ответ на запрос инструмента: разрешение, форма или её отмена.
    #[allow(clippy::too_many_arguments)]
    fn reply_prompt<'a>(
        &'a self,
        id: &'a str,
        prompt: &'a str,
        kind: &'a str,
        decision: &'a str,
        answer: &'a Value,
        message: &'a str,
        cancel: bool,
    ) -> Fut<'a, Result<(), String>>;
    /// Убирает сообщение из очереди, пока оно не отправлено.
    fn cancel_queued<'a>(&'a self, id: &'a str, queue: &'a str) -> Fut<'a, Result<(), String>>;
    /// Сообщение принято в очередь: панель показывает его, не дожидаясь ленты.
    fn note_optimistic(&self, _id: &str, _item: &str) {}
}
