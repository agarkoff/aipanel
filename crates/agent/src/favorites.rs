//! Избранные сообщения сессий: хранятся на машине агента, рядом с его
//! состоянием (`favorites.json` в каталоге данных), а не в браузере. Поэтому
//! они общие для всех вкладок и устройств и переживают перезапуск панели.

use std::{collections::BTreeMap, path::PathBuf};

use aipanel_proto::{Favorite, MAX_FAVORITES, MAX_FAVORITE_TEXT};
use serde::{Deserialize, Serialize};

/// Файл избранного в каталоге данных агента (`~/.aipanel`; у второго экземпляра
/// с `--home` — в его каталоге).
pub fn path() -> PathBuf {
    crate::util::aipanel_dir().join("favorites.json")
}

#[derive(Default, Deserialize, Serialize)]
struct File {
    /// Ключ — `tool:id` сессии, значения — в порядке добавления: так при
    /// переполнении вытесняется то, что отложили раньше всех. Показываются же
    /// они по времени сообщения (см. `Store::list`).
    #[serde(default)]
    sessions: BTreeMap<String, Vec<Favorite>>,
}

pub struct Store {
    path: PathBuf,
    file: File,
}

impl Store {
    /// Читает избранное из каталога агента. Битый или отсутствующий файл — это
    /// пустой список: из-за него агент не должен перестать запускаться.
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

    /// Избранное сессии по времени сообщения: старые сверху, как в ленте.
    /// В файле список лежит в порядке добавления, а плашка в шапке должна
    /// читаться как переписка. Сообщения без времени — в конце: их место
    /// неизвестно, и пусть они не разрывают известную хронологию.
    pub fn list(&self, session: &str) -> Vec<Favorite> {
        let mut items = self.file.sessions.get(session).cloned().unwrap_or_default();
        items.sort_by_key(|f| (f.ts <= 0, f.ts));
        items
    }

    /// Добавляет сообщение в избранное или убирает его оттуда и сохраняет файл.
    pub fn set(&mut self, session: &str, fav: Favorite, on: bool) -> Result<(), String> {
        let list = self.file.sessions.entry(session.to_owned()).or_default();
        let found = list.iter().position(|f| f.id == fav.id);
        match (on, found) {
            // Повторное «добавить» заменяет сохранённый текст: сообщение могло
            // дорасти (ответ ещё дописывался), и в списке нужен свежий.
            (true, Some(i)) => list[i] = normalize(fav),
            (true, None) => {
                list.push(normalize(fav));
                // Старое вытесняется, как в очереди: избранное не архив переписки.
                if list.len() > MAX_FAVORITES {
                    let extra = list.len() - MAX_FAVORITES;
                    list.drain(..extra);
                }
            }
            (false, Some(i)) => {
                list.remove(i);
            }
            (false, None) => return Ok(()),
        }
        if list.is_empty() {
            self.file.sessions.remove(session);
        }
        self.save()
    }

    /// Сессию удалили: её избранное больше не к чему относить.
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
        // Через временный файл: оборванная запись не оставит половину списка.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

/// В списке хранится начало сообщения: полный текст был бы копией переписки.
fn normalize(mut fav: Favorite) -> Favorite {
    if fav.text.chars().count() > MAX_FAVORITE_TEXT {
        let mut text: String = fav.text.chars().take(MAX_FAVORITE_TEXT).collect();
        text.push('…');
        fav.text = text;
    }
    if fav.ts < 0 {
        fav.ts = 0;
    }
    fav
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Своё имя каждому тесту: общий путь двух параллельных тестов означал бы,
    /// что один удаляет файл другого.
    fn tmp(name: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("aipanel-fav-{name}-{}-{n}.json", std::process::id()))
    }

    fn fav(id: &str, text: &str) -> Favorite {
        Favorite { id: id.into(), ts: 1, role: "assistant".into(), text: text.into(), of: String::new() }
    }

    #[test]
    fn keeps_fragments_next_to_their_message() {
        let path = tmp("frag");
        let mut store = Store::at(path.clone());
        store.set("s", fav("m1", "целиком"), true).unwrap();
        let mut part = fav("q:1", "кусок ответа");
        part.of = "m1".into();
        store.set("s", part, true).unwrap();
        // Фрагмент переживает перезапуск и помнит, из какого он сообщения.
        let again = Store::at(path.clone());
        let list = again.list("s");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].of, "");
        assert_eq!((list[1].id.as_str(), list[1].of.as_str()), ("q:1", "m1"));
        // У сообщения целиком поле `of` в файл не пишется.
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("\"of\"").count(), 1, "{text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn adds_lists_and_removes() {
        let path = tmp("adds");
        let mut store = Store::at(path.clone());
        assert!(store.list("claude:a").is_empty());
        store.set("claude:a", fav("m1", "первый"), true).unwrap();
        store.set("claude:a", fav("m2", "второй"), true).unwrap();
        // Другая сессия не видит чужого избранного.
        assert!(store.list("claude:b").is_empty());
        let ids: Vec<_> = store.list("claude:a").into_iter().map(|f| f.id).collect();
        assert_eq!(ids, ["m1", "m2"]);
        // Файл прочитается заново: избранное переживает перезапуск агента.
        let mut again = Store::at(path.clone());
        assert_eq!(again.list("claude:a").len(), 2);
        again.set("claude:a", fav("m1", ""), false).unwrap();
        let ids: Vec<_> = again.list("claude:a").into_iter().map(|f| f.id).collect();
        assert_eq!(ids, ["m2"]);
        // Последнее убранное сообщение убирает и саму сессию из файла.
        again.set("claude:a", fav("m2", ""), false).unwrap();
        assert!(again.list("claude:a").is_empty());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("claude:a"), "{text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lists_by_message_time_not_by_add_order() {
        let path = tmp("order");
        let mut store = Store::at(path.clone());
        let at = |id: &str, ts: i64| Favorite { id: id.into(), ts, role: "user".into(), text: id.into(), of: String::new() };
        // Добавляем вразнобой: в файле остаётся порядок добавления, а списком
        // правит время сообщения — позднее сообщение добавляют не последним.
        store.set("s", at("позднее", 3000), true).unwrap();
        store.set("s", at("без времени", 0), true).unwrap();
        store.set("s", at("раннее", 1000), true).unwrap();
        store.set("s", at("среднее", 2000), true).unwrap();
        let ids: Vec<_> = store.list("s").into_iter().map(|f| f.id).collect();
        assert_eq!(ids, ["раннее", "среднее", "позднее", "без времени"]);
        // Файл при этом не пересортирован: вытесняется то, что отложили раньше.
        let again = Store::at(path.clone());
        assert_eq!(again.list("s").len(), 4);
        let text = std::fs::read_to_string(&path).unwrap();
        let first = text.find("позднее").unwrap();
        assert!(first < text.find("раннее").unwrap(), "{text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn caps_text_and_count() {
        let path = tmp("caps");
        let mut store = Store::at(path.clone());
        let long = "я".repeat(MAX_FAVORITE_TEXT + 10);
        store.set("s", fav("m1", &long), true).unwrap();
        let text = &store.list("s")[0].text;
        assert_eq!(text.chars().count(), MAX_FAVORITE_TEXT + 1);
        assert!(text.ends_with('…'));
        for i in 0..MAX_FAVORITES + 5 {
            store.set("s", fav(&format!("x{i}"), "текст"), true).unwrap();
        }
        let list = store.list("s");
        assert_eq!(list.len(), MAX_FAVORITES);
        assert_eq!(list.last().unwrap().id, format!("x{}", MAX_FAVORITES + 4));
        // Удаление сессии убирает и её избранное.
        store.remove_session("s");
        assert!(store.list("s").is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
