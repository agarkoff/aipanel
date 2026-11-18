//! Черновики сообщений: недописанный текст, который панель сохраняет на машине
//! агента, а не в браузере. Поэтому черновик один на все устройства — набрали
//! на компьютере, открыли на телефоне и продолжили. Файл `drafts.json` лежит
//! рядом с избранным (`~/.aipanel`; у второго экземпляра с `--home` — в его
//! каталоге) и переживает перезапуск и агента, и сервера.

use std::{collections::BTreeMap, path::PathBuf};

use aipanel_proto::{MAX_DRAFTS, MAX_DRAFT_TEXT};
use serde::{Deserialize, Serialize};

/// Файл черновиков в каталоге данных агента.
pub fn path() -> PathBuf {
    crate::util::aipanel_dir().join("drafts.json")
}

#[derive(Default, Deserialize, Serialize)]
struct File {
    /// Ключ — `tool:id` сессии.
    #[serde(default)]
    sessions: BTreeMap<String, Draft>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Draft {
    text: String,
    /// Когда черновик последний раз меняли: по нему вытесняются давние.
    #[serde(default)]
    ts: i64,
}

pub struct Store {
    path: PathBuf,
    file: File,
}

impl Store {
    /// Битый или отсутствующий файл — это пустые черновики: из-за него агент не
    /// должен перестать запускаться.
    pub fn open() -> Self {
        Self::at(path())
    }

    fn at(path: PathBuf) -> Self {
        let mut file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<File>(&text).ok())
            .unwrap_or_default();
        // Пустой черновик в файле — это отсутствие черновика.
        file.sessions.retain(|_, d| !d.text.is_empty());
        Self { path, file }
    }

    /// Текст черновика сессии; пустая строка — черновика нет.
    pub fn get(&self, session: &str) -> String {
        self.file.sessions.get(session).map(|d| d.text.clone()).unwrap_or_default()
    }

    /// Запоминает черновик сессии (пустой текст стирает его) и сохраняет файл.
    /// `ts` — время правки, мс с эпохи Unix.
    pub fn set(&mut self, session: &str, text: String, ts: i64) -> Result<(), String> {
        if text.is_empty() {
            if self.file.sessions.remove(session).is_none() {
                return Ok(());
            }
            return self.save();
        }
        let text = limit(text);
        self.file.sessions.insert(session.to_owned(), Draft { text, ts: ts.max(0) });
        // Давно не тронутые черновики вытесняются: панель открывает всё новые
        // сессии, а файл не должен расти бесконечно.
        while self.file.sessions.len() > MAX_DRAFTS {
            let Some(oldest) = self
                .file
                .sessions
                .iter()
                .min_by_key(|(_, d)| d.ts)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.file.sessions.remove(&oldest);
        }
        self.save()
    }

    /// Сессию удалили: её черновику больше некуда возвращаться.
    pub fn remove_session(&mut self, session: &str) {
        if self.file.sessions.remove(session).is_some() {
            let _ = self.save();
        }
    }

    fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("не удалось создать {}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(&self.file).map_err(|e| e.to_string())?;
        // Через временный файл: оборванная запись не оставит половину черновиков.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

/// Слишком длинный черновик обрезается по символам, а не по байтам, чтобы не
/// разрезать UTF-8.
fn limit(text: String) -> String {
    if text.chars().count() <= MAX_DRAFT_TEXT {
        text
    } else {
        text.chars().take(MAX_DRAFT_TEXT).collect()
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
        std::env::temp_dir().join(format!("aipanel-draft-{name}-{}-{n}.json", std::process::id()))
    }

    #[test]
    fn keeps_per_session_and_survives_restart() {
        let path = tmp("perm");
        let mut store = Store::at(path.clone());
        assert_eq!(store.get("claude:a"), "");
        store.set("claude:a", "черновик".into(), 1).unwrap();
        store.set("opencode:b", "другой".into(), 2).unwrap();
        // Черновики сессий не смешиваются.
        assert_eq!(store.get("claude:a"), "черновик");
        assert_eq!(store.get("opencode:b"), "другой");
        // Файл читается заново: черновик переживает перезапуск агента.
        assert_eq!(Store::at(path.clone()).get("claude:a"), "черновик");
        // Пустой текст стирает черновик.
        let mut again = Store::at(path.clone());
        again.set("claude:a", String::new(), 3).unwrap();
        assert_eq!(Store::at(path.clone()).get("claude:a"), "");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn removes_session() {
        let path = tmp("remove");
        let mut store = Store::at(path.clone());
        store.set("claude:a", "текст".into(), 1).unwrap();
        store.remove_session("claude:a");
        assert_eq!(Store::at(path.clone()).get("claude:a"), "");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn limits_text_and_count() {
        let path = tmp("caps");
        let mut store = Store::at(path.clone());
        let long = "я".repeat(MAX_DRAFT_TEXT + 10);
        store.set("s", long, 1).unwrap();
        assert_eq!(store.get("s").chars().count(), MAX_DRAFT_TEXT);
        // Больше MAX_DRAFTS сессий не хранится: вытесняются те, что правили раньше.
        for i in 0..MAX_DRAFTS + 5 {
            store.set(&format!("claude:{i}"), "x".into(), (i + 10) as i64).unwrap();
        }
        assert_eq!(store.get("s"), "");
        assert_eq!(store.get(&format!("claude:{}", MAX_DRAFTS + 4)), "x");
        let _ = std::fs::remove_file(&path);
    }
}
