//! Протокол между агентом и сервером панели (JSON поверх WebSocket).

use serde::{Deserialize, Serialize};

/// Описание одной сессии Claude Code или opencode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Уникальный ключ в пределах агента: `claude:<id>` или `opencode:<id>`.
    pub key: String,
    /// `claude` или `opencode`.
    pub tool: String,
    pub title: String,
    pub cwd: String,
    /// `busy`, `idle`, `waiting`, `stopped` и т.п.
    pub status: String,
    /// Сессия сейчас открыта (есть живой процесс или сервис).
    pub live: bool,
    /// Время последнего изменения, мс с эпохи Unix.
    pub updated_at: i64,
    /// Как будет доставлено сообщение: `console`, `headless`, `api`, `none`.
    pub delivery: String,
}

/// Элемент ленты сессии. Элемент с уже известным `id` заменяет прежний.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    /// мс с эпохи Unix, 0 если неизвестно.
    pub ts: i64,
    /// `user`, `assistant`, `system`.
    pub role: String,
    /// `text`, `thinking`, `tool_use`, `tool_result`, `diff`, `error`.
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

/// Сообщения от агента серверу.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum AgentMsg {
    Hello {
        name: String,
        version: String,
        os: String,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    History {
        session: String,
        /// `true` — клиент должен очистить ленту перед добавлением.
        reset: bool,
        items: Vec<Item>,
    },
    Result {
        id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// Ключ созданной сессии (`opencode:<id>`), если команда его вернула.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
}

/// Сообщения от сервера агенту.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Watch { session: String },
    Unwatch { session: String },
    Send { id: String, session: String, text: String },
    Interrupt { id: String, session: String },
    /// Новая сессия: `tool` — `claude` или `opencode`, `cwd` — абсолютный путь.
    Start { id: String, tool: String, cwd: String },
    /// Продолжить сессию `session` в другом инструменте `tool`.
    Handoff { id: String, session: String, tool: String },
}
