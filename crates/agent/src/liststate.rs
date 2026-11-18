//! Состояние списка сессий: закреплённые сессии и свёрнутые проекты. Хранится на
//! машине агента (`list.json` в каталоге данных), а не в браузере, поэтому
//! список выглядит одинаково во всех браузерах и на всех устройствах и
//! переживает перезапуск и панели, и агента.

use std::path::PathBuf;

use aipanel_proto::{MAX_CLOSED_PROJECTS, MAX_PINS};
use serde::{Deserialize, Serialize};

/// Файл состояния списка в каталоге данных агента (`~/.aipanel`; у второго
/// экземпляра с `--home` — в его каталоге).
pub fn path() -> PathBuf {
    crate::util::aipanel_dir().join("list.json")
}

#[derive(Default, Deserialize, Serialize)]
struct File {
    /// Ключи `tool:id` закреплённых сессий в порядке закрепления, свежие сверху.
    #[serde(default)]
    pins: Vec<String>,
    /// Ключи свёрнутых проектов: `repo:<адрес>` или `dir:<путь>`.
    #[serde(default)]
    closed: Vec<String>,
}

pub struct Store {
    path: PathBuf,
    file: File,
}

impl Store {
    /// Читает состояние списка. Битый или отсутствующий файл — это пустое
    /// состояние: из-за него агент не должен перестать запускаться.
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

    /// Закреплённые сессии и свёрнутые проекты — как они лежат в файле.
    pub fn list(&self) -> (Vec<String>, Vec<String>) {
        (self.file.pins.clone(), self.file.closed.clone())
    }

    /// Закрепляет сессию или открепляет её. Свежий закреп встаёт в начало:
    /// порядок закреплённых — порядок закрепления.
    pub fn pin(&mut self, session: &str, on: bool) -> Result<(), String> {
        let at = self.file.pins.iter().position(|s| s == session);
        match (on, at) {
            // Уже закреплена: снимаем со старого места и ставим в начало.
            (true, Some(i)) => {
                self.file.pins.remove(i);
                self.file.pins.insert(0, session.to_owned());
            }
            (true, None) => {
                if self.file.pins.len() >= MAX_PINS {
                    self.file.pins.pop();
                }
                self.file.pins.insert(0, session.to_owned());
            }
            (false, Some(i)) => {
                self.file.pins.remove(i);
            }
            (false, None) => return Ok(()),
        }
        self.save()
    }

    /// Сворачивает проект или раскрывает его.
    pub fn collapse(&mut self, project: &str, on: bool) -> Result<(), String> {
        let at = self.file.closed.iter().position(|p| p == project);
        match (on, at) {
            (true, Some(_)) => return Ok(()),
            (true, None) => {
                if self.file.closed.len() >= MAX_CLOSED_PROJECTS {
                    self.file.closed.remove(0);
                }
                self.file.closed.push(project.to_owned());
            }
            (false, Some(i)) => {
                self.file.closed.remove(i);
            }
            (false, None) => return Ok(()),
        }
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("не удалось создать {}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(&self.file).map_err(|e| e.to_string())?;
        // Через временный файл: оборванная запись не оставит половину состояния.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("aipanel-list-{name}-{}-{n}.json", std::process::id()))
    }

    #[test]
    fn pins_newest_first_and_survives_restart() {
        let path = tmp("pins");
        let mut store = Store::at(path.clone());
        store.pin("claude:a", true).unwrap();
        store.pin("opencode:b", true).unwrap();
        // Свежий закреп встаёт в начало, повторный — не дублируется.
        assert_eq!(store.list().0, vec!["opencode:b".to_string(), "claude:a".to_string()]);
        store.pin("claude:a", true).unwrap();
        assert_eq!(store.list().0, vec!["claude:a".to_string(), "opencode:b".to_string()]);
        // Файл читается заново: закреп переживает перезапуск агента.
        assert_eq!(Store::at(path.clone()).list().0[0], "claude:a");
        store.pin("claude:a", false).unwrap();
        assert_eq!(Store::at(path.clone()).list().0, vec!["opencode:b".to_string()]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn collapses_projects_without_duplicates() {
        let path = tmp("closed");
        let mut store = Store::at(path.clone());
        store.collapse("dir:c:\\work\\api", true).unwrap();
        store.collapse("dir:c:\\work\\api", true).unwrap();
        store.collapse("repo:git.example.ru/root/ai-panel", true).unwrap();
        assert_eq!(
            Store::at(path.clone()).list().1,
            vec!["dir:c:\\work\\api".to_string(), "repo:git.example.ru/root/ai-panel".to_string()]
        );
        store.collapse("dir:c:\\work\\api", false).unwrap();
        assert_eq!(Store::at(path.clone()).list().1, vec!["repo:git.example.ru/root/ai-panel".to_string()]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn limits_counts() {
        let path = tmp("caps");
        let mut store = Store::at(path.clone());
        for i in 0..MAX_PINS + 5 {
            store.pin(&format!("claude:{i}"), true).unwrap();
        }
        assert_eq!(store.list().0.len(), MAX_PINS);
        for i in 0..MAX_CLOSED_PROJECTS + 5 {
            store.collapse(&format!("dir:{i}"), true).unwrap();
        }
        assert_eq!(store.list().1.len(), MAX_CLOSED_PROJECTS);
        let _ = std::fs::remove_file(&path);
    }
}
