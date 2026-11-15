//! Протокол между агентом и сервером панели (JSON поверх WebSocket).

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// Вариант ответа в запросе opencode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptOption {
    pub value: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// Условие показа поля формы: поле видно, если другое поле равно `value`
/// (`eq`) или не равно ему (`neq`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptWhen {
    pub key: String,
    /// `eq` или `neq`.
    pub op: String,
    pub value: Value,
}

/// Поле формы opencode: вопрос с вариантами, текст, число или флажок.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptField {
    pub key: String,
    /// `string`, `number`, `integer`, `boolean`, `multiselect` или `external`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    /// Поле скрыто, но его значение по умолчанию всё равно входит в ответ.
    #[serde(default)]
    pub hidden: bool,
    /// Разрешено ввести своё значение, кроме готовых вариантов.
    #[serde(default)]
    pub custom: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<PromptOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pattern: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub placeholder: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<PromptWhen>,
}

/// Запрос opencode, который ждёт ответа в окне программы: разрешение на действие
/// или форма с вопросом и вариантами ответа.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Prompt {
    /// `per_…` для разрешения, `frm_…` для формы.
    pub id: String,
    /// `permission` или `form`.
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<PromptField>,
    /// Правила, которые сохранит ответ «разрешить всегда».
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub save: Vec<String>,
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
        /// Идентификаторы элементов, которые нужно убрать из ленты (например,
        /// сообщение из очереди opencode отправлено и стало обычным сообщением).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        drop: Vec<String>,
    },
    /// Запросы opencode, ожидающие ответа в этой сессии. Полный список: клиент
    /// заменяет им показанные ранее запросы.
    Prompts {
        session: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prompts: Vec<Prompt>,
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
    /// Убрать сообщение `queue` из очереди opencode-сессии `session`.
    CancelQueued { id: String, session: String, queue: String },
    /// Ответить на запрос opencode `prompt`: разрешение (`decision`), форма
    /// (`answer`) или отмена формы (`cancel`).
    PromptReply {
        id: String,
        session: String,
        prompt: String,
        /// `permission` или `form`.
        kind: String,
        #[serde(default)]
        decision: String,
        #[serde(default)]
        answer: Value,
        #[serde(default)]
        message: String,
        #[serde(default)]
        cancel: bool,
    },
}
