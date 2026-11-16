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
    /// Модель последнего ответа (`claude-opus-…`, `anthropic/claude-sonnet-…`); пусто, если неизвестна.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Токены текущего хода по данным транскрипта; нет, если их нечем подтвердить.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnUsage>,
}

/// Что транскрипт сообщает о текущем ходе: от последнего запроса пользователя
/// до конца записанного. Только записанное, без оценок: часть ещё не дописанного
/// ответа сюда не попадает.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnUsage {
    /// Время запроса пользователя, мс с эпохи Unix.
    pub started_at: i64,
    /// Контекст последнего ответа: входные токены вместе с кешем; 0 — ответов ещё нет.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub input: u64,
    /// Выходные токены всех ответов хода; 0 — ответов ещё нет.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub output: u64,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
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

/// Остаток одного окна лимита подписки (сессия, неделя, месяц).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meter {
    /// Подпись окна для панели: `5 ч`, `неделя`, `месяц`, `Fable`.
    pub label: String,
    /// Сколько процентов лимита ещё доступно, 0–100.
    pub left: u8,
    /// Время сброса, мс с эпохи Unix; 0 — неизвестно.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resets_at: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Лимиты подписки, которые агент проверяет на своей машине.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    /// `claude` или `opencode`.
    pub tool: String,
    /// Заголовок для панели: `Claude Code` или `OpenCode Go`.
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meters: Vec<Meter>,
    /// Когда лимиты проверены последний раз, мс с эпохи Unix.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub updated_at: i64,
}

/// Команда, которую принимает сессия (`/init`, `/clear`). Панель показывает их
/// списком, когда в поле ввода набирают `/`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Command {
    /// Имя без слэша: `init`, `review`, `rea:trace_crash`.
    pub name: String,
    /// Что делает команда; пусто, если инструмент не сообщил описание.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// Модель, доступная в сессии opencode. Панель показывает их списком по `/models`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Провайдер: `opencode-go`, `openrouter`, `ollama`.
    pub provider: String,
    /// Идентификатор модели у провайдера: `deepseek-v4.1-flash`.
    pub id: String,
    /// Название для панели: `DeepSeek V4.1 Flash`.
    pub name: String,
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
    /// Текущий остаток лимитов подписок на машине агента. Полный список: он
    /// заменяет показанный ранее.
    Limits {
        limits: Vec<Limits>,
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
        /// Есть ли перед `items` более ранние элементы, которые догружаются по
        /// `ServerMsg::Older`. Заполняется в первой ленте (`reset`): по нему
        /// панель решает, показывать ли кнопку догрузки.
        #[serde(default, skip_serializing_if = "is_false")]
        more: bool,
    },
    /// Более ранние элементы ленты сессии, догруженные по `ServerMsg::Older`.
    /// В отличие от `History` их ставят в начало ленты, а не в конец.
    Older {
        session: String,
        /// Элементы в порядке ленты: от старых к новым.
        items: Vec<Item>,
        /// Есть ли ещё более ранние элементы: по нему панель прячет кнопку.
        more: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Запросы opencode, ожидающие ответа в этой сессии. Полный список: клиент
    /// заменяет им показанные ранее запросы.
    Prompts {
        session: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        prompts: Vec<Prompt>,
    },
    /// Команды, доступные в сессии. Приходит весь список, в том числе пустой:
    /// панель заменяет им показанный ранее.
    Commands {
        session: String,
        commands: Vec<Command>,
    },
    /// Модели, доступные в сессии opencode; весь список, как и у команд.
    Models {
        session: String,
        models: Vec<ModelInfo>,
    },
    /// Запрос моделей для окна «Новая сессия»: сессии ещё нет, есть только
    /// инструмент и каталог.
    ToolModels {
        id: String,
        models: Vec<ModelInfo>,
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
    /// Ответ на `ReadImage`: картинка (`mime` и байты в base64) или ошибка.
    Image {
        id: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        mime: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Один экран машины агента на запрос `ServerMsg::Screens`. Снимков приходит
    /// столько, сколько экранов: каждый отдельным сообщением, чтобы не упереться
    /// в предел размера сообщения WebSocket.
    Screen {
        id: String,
        /// Номер экрана: 0 — первый.
        index: u32,
        /// Сколько всего экранов на машине.
        count: u32,
        /// Подпись для панели: `Экран 1 · основной`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        label: String,
        /// Размер картинки в пикселях — после возможного уменьшения.
        width: u32,
        height: u32,
        /// PNG в base64.
        data: String,
    },
    /// Экраны сняты: `Screen` этой серии больше не будет. `error` — если снять
    /// не удалось вовсе (нет экрана, платформа не поддерживается).
    Screens {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// Наибольший размер картинки, которую агент отдаёт по `ReadImage`. В base64 она
/// вырастает на треть и должна поместиться в сообщение WebSocket (8 МБ).
pub const MAX_IMAGE_BYTES: u64 = 5 << 20;

/// Наибольший размер PNG одного экрана в `AgentMsg::Screen`. В base64 картинка
/// вырастает на треть, и сообщение WebSocket (8 МБ) должно остаться целым:
/// агент уменьшает снимок, пока PNG не влезет в этот предел.
pub const MAX_SCREEN_BYTES: usize = 4 << 20;

/// Сколько экранов панель принимает за один снимок: защита от агента,
/// который пришлёт бесконечную серию.
pub const MAX_SCREENS: u32 = 16;

/// Типы картинок, которые агент отдаёт и сервер пропускает в браузер.
pub const IMAGE_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/avif",
    "image/bmp",
    "image/x-icon",
    "image/svg+xml",
];

/// Base64 (стандартный алфавит, с `=`) для картинок в `AgentMsg::Image`.
pub mod b64 {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub fn decode(text: &str) -> Option<Vec<u8>> {
        let s = text.as_bytes();
        if s.len() % 4 != 0 {
            return None;
        }
        let val = |c: u8| -> Option<u32> {
            Some(match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return None,
            } as u32)
        };
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        for (k, q) in s.chunks(4).enumerate() {
            let last = k + 1 == s.len() / 4;
            let pad = q.iter().rev().take_while(|&&c| c == b'=').count();
            if pad > 2 || (pad > 0 && !last) {
                return None;
            }
            let mut n = 0u32;
            for &c in &q[..4 - pad] {
                n = (n << 6) | val(c)?;
            }
            n <<= 6 * pad as u32;
            let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
            out.extend_from_slice(&bytes[..3 - pad]);
        }
        Some(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn round_trip() {
            for len in 0..40 {
                let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
                assert_eq!(decode(&encode(&data)).as_deref(), Some(&data[..]), "len {len}");
            }
            assert_eq!(encode(b"Man"), "TWFu");
            assert_eq!(encode(b"Ma"), "TWE=");
            assert_eq!(encode(b"M"), "TQ==");
            assert_eq!(decode("TQ==").unwrap(), b"M");
        }

        #[test]
        fn rejects_garbage() {
            assert!(decode("TQ=").is_none());
            assert!(decode("T===").is_none());
            assert!(decode("TQ==TWFu").is_none());
            assert!(decode("TW*u").is_none());
        }
    }
}

/// Сообщения от сервера агенту.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Watch { session: String },
    Unwatch { session: String },
    /// Догрузить более ранние элементы ленты `session`: агент присылает их
    /// ответом `AgentMsg::Older`.
    Older { session: String },
    Send { id: String, session: String, text: String },
    Interrupt { id: String, session: String },
    /// Новая сессия: `tool` — `claude` или `opencode`, `cwd` — абсолютный путь,
    /// `model` — выбранная модель (пусто — по умолчанию). `create` разрешает
    /// создать каталог, которого нет: панель спрашивает об этом после ответа
    /// «каталог не найден» и повторяет запуск с этим флагом.
    Start {
        id: String,
        tool: String,
        cwd: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        create: bool,
    },
    /// Продолжить сессию `session` в другом инструменте `tool`.
    Handoff { id: String, session: String, tool: String },
    /// Убрать сообщение `queue` из очереди opencode-сессии `session`.
    CancelQueued { id: String, session: String, queue: String },
    /// Удалить сессию `session`: транскрипт Claude Code или сессию opencode.
    /// `close` — если сессия открыта или выполняется, сначала закрыть её
    /// (завершить процесс или окно), иначе удаление открытой сессии отклоняется.
    Delete {
        id: String,
        session: String,
        #[serde(default)]
        close: bool,
    },
    /// Закрыть сессию `session`: завершить её окно консоли или фоновый процесс,
    /// ничего не удаляя. Переписка остаётся в списке и продолжается по `--resume`.
    Close { id: String, session: String },
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
    /// Прислать команды сессии `session`: их список зависит от инструмента и каталога.
    Commands { session: String },
    /// Прочитать картинку `path` с машины агента: путь из ленты сессии `session`,
    /// относительный считается от её каталога. Ответ — `AgentMsg::Image`.
    ReadImage { id: String, session: String, path: String },
    /// Прислать модели, доступные сессии opencode `session`.
    Models { session: String },
    /// Прислать модели для запуска новой сессии `tool` в каталоге `cwd`.
    ToolModels { id: String, tool: String, cwd: String },
    /// Переключить сессию `session` на модель `provider`/`model`.
    SetModel {
        id: String,
        session: String,
        provider: String,
        model: String,
    },
    /// Снять экраны машины агента (все мониторы). Ответ — `AgentMsg::Screen` на
    /// каждый экран, затем `AgentMsg::Screens`.
    Screens { id: String },
}
