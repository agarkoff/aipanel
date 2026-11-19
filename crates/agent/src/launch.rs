//! Недавние модели окна «Новая сессия»: чем запускали сессии каждого
//! инструмента. Хранятся в каталоге агента (`launch.json`), а не в браузере:
//! список opencode зависит от машины и провайдеров, у Claude Code это алиасы
//! CLI, и на телефоне должны подниматься те же модели, что на компьютере.

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

    /// Модель использовали — запустили новую сессию или переключили у открытой.
    /// Свежая встаёт в начало, повтор не дублируется, хвост обрезается: это
    /// подсказка, а не история.
    pub fn note(&mut self, tool: &str, model: &str) -> Result<(), String> {
        if tool.is_empty() || model.is_empty() {
            return Ok(());
        }
        let list = self.file.recents.entry(tool.to_owned()).or_default();
        list.retain(|v| v != model);
        list.insert(0, model.to_owned());
        list.truncate(MAX_MODEL_RECENTS);
        self.save()
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
