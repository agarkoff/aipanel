//! Подписки устройств на push-уведомления. У сервера панели нет диска, поэтому
//! подписки хранит агент (`push.json` в каталоге данных, как и избранное) и
//! присылает их серверу при каждом подключении. Уведомления шлёт сервер.

use std::path::PathBuf;

use aipanel_proto::{PushSub, MAX_PUSH_SUBS};
use serde::{Deserialize, Serialize};

pub fn path() -> PathBuf {
    crate::util::aipanel_dir().join("push.json")
}

#[derive(Default, Deserialize, Serialize)]
struct File {
    #[serde(default)]
    subs: Vec<PushSub>,
}

pub struct Store {
    path: PathBuf,
    file: File,
}

impl Store {
    /// Битый или отсутствующий файл — пустой список: из-за него агент не
    /// должен перестать запускаться.
    pub fn open() -> Self {
        Self::at(path())
    }

    fn at(path: PathBuf) -> Self {
        let mut file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<File>(&text).ok())
            .unwrap_or_default();
        file.subs.retain(PushSub::valid);
        Self { path, file }
    }

    pub fn list(&self) -> Vec<PushSub> {
        self.file.subs.clone()
    }

    /// Сохраняет подписку (`on`) или удаляет её. Подписка одного устройства
    /// узнаётся по адресу: новые ключи с тем же адресом заменяют прежние.
    pub fn set(&mut self, sub: PushSub, on: bool) -> Result<(), String> {
        if on && !sub.valid() {
            return Err(format!("некорректная подписка: {}", sub.endpoint));
        }
        let found = self.file.subs.iter().position(|s| s.endpoint == sub.endpoint);
        match (on, found) {
            (true, Some(i)) if self.file.subs[i] == sub => return Ok(()),
            (true, Some(i)) => self.file.subs[i] = sub,
            (true, None) => {
                self.file.subs.push(sub);
                // Самые старые подписки — скорее всего, давно не существующих устройств.
                if self.file.subs.len() > MAX_PUSH_SUBS {
                    let extra = self.file.subs.len() - MAX_PUSH_SUBS;
                    self.file.subs.drain(..extra);
                }
            }
            (false, Some(i)) => {
                self.file.subs.remove(i);
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
        // Через временный файл: оборванная запись не оставит половину списка.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aipanel_proto::b64url;

    fn tmp(name: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("aipanel-push-{name}-{}-{n}.json", std::process::id()))
    }

    fn sub(id: &str, auth: u8) -> PushSub {
        let mut key = vec![4u8];
        key.extend([9u8; 64]);
        PushSub {
            endpoint: format!("https://fcm.googleapis.com/fcm/send/{id}"),
            p256dh: b64url::encode(&key),
            auth: b64url::encode(&[auth; 16]),
        }
    }

    #[test]
    fn saves_replaces_and_removes() {
        let path = tmp("set");
        let mut store = Store::at(path.clone());
        store.set(sub("a", 1), true).unwrap();
        store.set(sub("b", 1), true).unwrap();
        // То же устройство с новыми ключами — замена, а не вторая подписка.
        store.set(sub("a", 2), true).unwrap();
        let again = Store::at(path.clone());
        assert_eq!(again.list(), [sub("a", 2), sub("b", 1)]);
        let mut again = again;
        again.set(sub("a", 0), false).unwrap();
        assert_eq!(Store::at(path.clone()).list(), [sub("b", 1)]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_foreign_endpoint_and_caps_count() {
        let path = tmp("cap");
        let mut store = Store::at(path.clone());
        let mut bad = sub("x", 1);
        bad.endpoint = "https://example.com/push".into();
        assert!(store.set(bad, true).is_err());
        for i in 0..MAX_PUSH_SUBS + 3 {
            store.set(sub(&format!("d{i}"), 1), true).unwrap();
        }
        let list = store.list();
        assert_eq!(list.len(), MAX_PUSH_SUBS);
        assert_eq!(list[0], sub("d3", 1));
        let _ = std::fs::remove_file(&path);
    }
}
