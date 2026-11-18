//! Место чтения: где остановилась прокрутка списка сессий и ленты каждой
//! сессии. Хранится в каталоге агента (`scroll.json`), а не в браузере: тогда
//! сессия открывается там, где её читали, и по ссылке, и после перезапуска
//! панели, и с другого устройства.

use std::{collections::BTreeMap, path::PathBuf};

use aipanel_proto::{ScrollPos, MAX_SCROLLS};
use serde::{Deserialize, Serialize};

/// Файл в каталоге данных агента (`~/.aipanel`; у второго экземпляра с
/// `--home` — в его каталоге).
pub fn path() -> PathBuf {
    crate::util::aipanel_dir().join("scroll.json")
}

#[derive(Default, Deserialize, Serialize)]
struct File {
    /// Место в списке сессий; `ts` — когда его записали.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    list: Option<Saved>,
    /// Ключ — `tool:id` сессии.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    sessions: BTreeMap<String, Saved>,
}

/// Положение и время записи. В JSON лежат плоско, рядом с полями `ScrollPos`.
#[derive(Clone, Default, Deserialize, Serialize)]
struct Saved {
    #[serde(flatten)]
    pos: ScrollPos,
    #[serde(default)]
    ts: i64,
}

pub struct Store {
    path: PathBuf,
    file: File,
}

impl Store {
    /// Битый или отсутствующий файл — это пустое место чтения: из-за него агент
    /// не должен перестать запускаться, а сессия — открываться.
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

    /// Место в списке сессий и время его записи.
    pub fn list(&self) -> Option<(ScrollPos, i64)> {
        self.file.list.as_ref().map(|s| (s.pos.clone(), s.ts))
    }

    /// Место чтения ленты сессии; `None` — сессию ещё не читали.
    pub fn session(&self, session: &str) -> Option<ScrollPos> {
        self.file.sessions.get(session).map(|s| s.pos.clone())
    }

    /// Запоминает место в списке и сохраняет файл. Более старое место
    /// игнорируется: его мог прислать агент, который был не в сети, пока список
    /// прокручивали на другом устройстве.
    pub fn set_list(&mut self, pos: ScrollPos, ts: i64) -> Result<(), String> {
        if self.file.list.as_ref().is_some_and(|s| s.ts > ts) {
            return Ok(());
        }
        self.file.list = Some(Saved { pos, ts });
        self.save()
    }

    /// Запоминает место чтения ленты сессии и сохраняет файл.
    pub fn set_session(&mut self, session: &str, pos: ScrollPos, ts: i64) -> Result<(), String> {
        self.file.sessions.insert(session.to_owned(), Saved { pos, ts });
        // Давно не тронутые места вытесняются: панель открывает всё новые
        // сессии, а файл не должен расти бесконечно.
        while self.file.sessions.len() > MAX_SCROLLS {
            let Some(oldest) = self
                .file
                .sessions
                .iter()
                .min_by_key(|(_, s)| s.ts)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.file.sessions.remove(&oldest);
        }
        self.save()
    }

    /// Сессию удалили: её место чтения больше некуда возвращать.
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
        // Через временный файл: оборванная запись не оставит половину состояния.
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
        std::env::temp_dir().join(format!("aipanel-scroll-{name}-{}-{n}.json", std::process::id()))
    }

    fn pos(id: &str, off: f64, top: f64) -> ScrollPos {
        ScrollPos { id: id.into(), off, top, end: false }
    }

    #[test]
    fn keeps_places_per_session_and_survives_restart() {
        let path = tmp("sessions");
        let mut store = Store::at(path.clone());
        assert!(store.session("claude:a").is_none());
        store.set_session("claude:a", pos("m3", -12.5, 340.0), 1).unwrap();
        store.set_session("opencode:b", ScrollPos { end: true, ..Default::default() }, 2).unwrap();
        // Места сессий не смешиваются и переживают перезапуск агента.
        assert_eq!(store.session("claude:a"), Some(pos("m3", -12.5, 340.0)));
        let again = Store::at(path.clone());
        assert_eq!(again.session("claude:a"), Some(pos("m3", -12.5, 340.0)));
        assert_eq!(again.session("opencode:b").unwrap().end, true);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn keeps_newer_list_place() {
        let path = tmp("list");
        let mut store = Store::at(path.clone());
        store.set_list(pos("row:claude\nclaude:a", 8.0, 120.0), 100).unwrap();
        // Более старое место от агента, который был не в сети, ничего не меняет.
        store.set_list(pos("", 0.0, 0.0), 50).unwrap();
        assert_eq!(Store::at(path.clone()).list().unwrap().1, 100);
        store.set_list(pos("row:opencode\nopencode:b", 0.0, 40.0), 200).unwrap();
        let (p, ts) = Store::at(path.clone()).list().unwrap();
        assert_eq!((p.id.as_str(), ts), ("row:opencode\nopencode:b", 200));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn removes_session_and_limits_count() {
        let path = tmp("caps");
        let mut store = Store::at(path.clone());
        store.set_session("claude:a", pos("", 0.0, 1.0), 1).unwrap();
        store.remove_session("claude:a");
        assert!(Store::at(path.clone()).session("claude:a").is_none());
        for i in 0..MAX_SCROLLS + 5 {
            store.set_session(&format!("claude:{i}"), pos("", 0.0, i as f64), (i + 10) as i64).unwrap();
        }
        assert_eq!(Store::at(path.clone()).session("claude:0"), None);
        assert!(Store::at(path.clone()).session(&format!("claude:{}", MAX_SCROLLS + 4)).is_some());
        let _ = std::fs::remove_file(&path);
    }
}
