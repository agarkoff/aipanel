//! Параметры окна «Новая сессия»: последний выбранный инструмент, модель каждого
//! инструмента и недавние модели для подсказки. Хранятся в каталоге агента
//! (`launch.json`), а не в браузере: список opencode зависит от машины и
//! провайдеров, у Claude Code это алиасы CLI, и на телефоне должно подставляться
//! то же, что на компьютере, — открытой сессии, от которой можно взять
//! инструмент, там обычно нет.

use std::{collections::BTreeMap, path::PathBuf};

use aipanel_proto::MAX_MODEL_RECENTS;
use serde::{Deserialize, Serialize};

/// Файл в каталоге данных агента (`~/.aipanel`; у второго экземпляра с
/// `--home` — в его каталоге).
pub fn path() -> PathBuf {
    crate::util::aipanel_dir().join("launch.json")
}

#[derive(Default, Deserialize, Serialize)]
struct File {
    /// Последний выбранный инструмент: `claude` или `opencode`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    tool: String,
    /// Модель, которой запускали сессии этого инструмента: ключ — инструмент.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    models: BTreeMap<String, String>,
    /// Недавние модели: ключ — инструмент (`claude` или `opencode`), свежие
    /// впереди.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    recents: BTreeMap<String, Vec<String>>,
}

pub struct Store {
    path: PathBuf,
    file: File,
}

impl Store {
    /// Читает файл из каталога агента. Битый или отсутствующий — это пустые
    /// недавние: из-за него агент не должен перестать запускаться, а окно —
    /// открываться.
    pub fn open() -> Self {
        Self::at(path())
    }

    fn at(path: PathBuf) -> Self {
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<File>(&text).ok())
            .unwrap_or_default();
        Self { path, file }
    }

    /// Недавние модели инструмента, свежие впереди.
    pub fn recents(&self, tool: &str) -> Vec<String> {
        self.file.recents.get(tool).cloned().unwrap_or_default()
    }

    /// Последний выбранный инструмент; пусто — новых сессий ещё не запускали.
    pub fn tool(&self) -> &str {
        &self.file.tool
    }

    /// Модель, которой последний раз запускали сессию `tool`.
    pub fn model(&self, tool: &str) -> &str {
        self.file.models.get(tool).map(String::as_str).unwrap_or("")
    }

    /// Запоминает, чем запустили новую сессию: инструмент, его модель и место
    /// модели в недавних. Именно это окно подставит в следующий раз на любом
    /// устройстве. Модель без инструмента смысла не имеет, поэтому не пишется.
    pub fn remember(&mut self, tool: &str, model: &str) -> Result<(), String> {
        if tool.is_empty() {
            return Ok(());
        }
        self.file.tool = tool.to_owned();
        if !model.is_empty() {
            self.file.models.insert(tool.to_owned(), model.to_owned());
            self.note_recent(tool, model);
        }
        self.save()
    }

    /// Модель использовали, но сессию не запускали — переключили у открытой.
    /// «Так запускали» это не меняет, а в подсказке модель поднимается.
    pub fn note(&mut self, tool: &str, model: &str) -> Result<(), String> {
        if tool.is_empty() || model.is_empty() {
            return Ok(());
        }
        self.note_recent(tool, model);
        self.save()
    }

    /// Свежая модель встаёт в начало, повтор не дублируется, хвост обрезается:
    /// это подсказка, а не история.
    fn note_recent(&mut self, tool: &str, model: &str) {
        let list = self.file.recents.entry(tool.to_owned()).or_default();
        list.retain(|v| v != model);
        list.insert(0, model.to_owned());
        list.truncate(MAX_MODEL_RECENTS);
    }

    fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("не удалось создать {}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(&self.file).map_err(|e| e.to_string())?;
        // Через временный файл: оборванная запись не оставит половину файла.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Своё имя каждому тесту: общий путь двух параллельных тестов означал бы,
    /// что один удаляет файл другого.
    fn tmp(name: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("aipanel-launch-{name}-{}-{n}.json", std::process::id()))
    }

    #[test]
    fn empty_when_nothing_launched() {
        let store = Store::at(tmp("empty"));
        assert!(store.recents("opencode").is_empty());
        assert!(store.recents("claude").is_empty());
        assert_eq!(store.tool(), "");
        assert_eq!(store.model("claude"), "");
    }

    #[test]
    fn remembers_tool_and_model_per_tool() {
        let path = tmp("remembers");
        let mut store = Store::at(path.clone());
        store.remember("opencode", "openai/gpt-6").unwrap();
        store.remember("claude", "sonnet").unwrap();
        // Инструмент — последний запущенный, модели — свои у каждого.
        assert_eq!(store.tool(), "claude");
        assert_eq!(store.model("claude"), "sonnet");
        assert_eq!(store.model("opencode"), "openai/gpt-6");
        // Переживает перезапуск агента.
        let again = Store::at(path.clone());
        assert_eq!((again.tool(), again.model("opencode")), ("claude", "openai/gpt-6"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"openai/gpt-6\""), "{text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn launch_without_model_keeps_previous_one() {
        let path = tmp("no-model");
        let mut store = Store::at(path.clone());
        store.remember("claude", "opus").unwrap();
        store.remember("claude", "").unwrap();
        assert_eq!(store.model("claude"), "opus");
        // Пустой инструмент ничего не меняет.
        store.remember("", "haiku").unwrap();
        assert_eq!((store.tool(), store.model("claude")), ("claude", "opus"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn used_model_does_not_change_last_launch() {
        let path = tmp("used");
        let mut store = Store::at(path.clone());
        store.remember("opencode", "openai/gpt-6").unwrap();
        store.note("opencode", "openai/gpt-6-mini").unwrap();
        // Переключение у сессии — подсказка, а не «так запускали».
        assert_eq!(store.model("opencode"), "openai/gpt-6");
        assert_eq!(store.recents("opencode"), vec!["openai/gpt-6-mini", "openai/gpt-6"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fresh_first_without_duplicates() {
        let path = tmp("recents");
        let mut store = Store::at(path.clone());
        store.note("opencode", "openai/gpt-6").unwrap();
        store.note("opencode", "openai/gpt-5").unwrap();
        // Повтор поднимает модель, а не добавляет вторую строку.
        store.note("opencode", "openai/gpt-6").unwrap();
        assert_eq!(store.recents("opencode"), vec!["openai/gpt-6", "openai/gpt-5"]);
        // У каждого инструмента свои недавние.
        assert!(store.recents("claude").is_empty());
        // Переживают перезапуск агента.
        assert_eq!(Store::at(path.clone()).recents("opencode"), store.recents("opencode"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"openai/gpt-6\""), "{text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stops_at_limit() {
        let path = tmp("limit");
        let mut store = Store::at(path.clone());
        for i in 0..MAX_MODEL_RECENTS + 3 {
            store.note("claude", &format!("model-{i}")).unwrap();
        }
        let list = store.recents("claude");
        assert_eq!(list.len(), MAX_MODEL_RECENTS);
        assert_eq!(list[0], format!("model-{}", MAX_MODEL_RECENTS + 2));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_values_change_nothing() {
        let path = tmp("empty-values");
        let mut store = Store::at(path.clone());
        store.note("opencode", "openai/gpt-6").unwrap();
        store.note("", "openai/gpt-5").unwrap();
        store.note("opencode", "").unwrap();
        assert_eq!(store.recents("opencode"), vec!["openai/gpt-6"]);
        let _ = std::fs::remove_file(&path);
    }
}
