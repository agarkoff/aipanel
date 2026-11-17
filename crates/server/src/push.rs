//! Отправка push-уведомлений (Web Push) на устройства пользователя.
//!
//! Сервер шлёт уведомление HTTPS-запросом на адрес подписки у push-службы
//! браузера (Google FCM, Apple, Mozilla, Microsoft). Текст шифруется ключами
//! устройства (RFC 8291, `aes128gcm`), а запрос подписывается ключом сервера
//! VAPID (RFC 8292): по нему push-служба проверяет, что подписку создали для
//! этого сервера. Один ключ на всю панель задаётся в `PANEL_VAPID_KEY`.

use std::{sync::Arc, time::Duration};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes128Gcm, Nonce,
};
use aipanel_proto::{b64url, PushSub, SessionInfo};
use hkdf::Hkdf;
use p256::{
    ecdsa::{signature::Signer, Signature, SigningKey},
    elliptic_curve::sec1::ToEncodedPoint,
    PublicKey, SecretKey,
};
use rand_core::{OsRng, RngCore};
use serde_json::json;
use sha2::Sha256;

use crate::App;

/// Размер записи в заголовке `aes128gcm`. Уведомление — одна запись.
const RECORD_SIZE: u32 = 4096;
/// Сколько push-служба хранит недоставленное уведомление (выключенный телефон).
const TTL_SECS: u64 = 24 * 60 * 60;
/// Срок подписи VAPID: службы принимают не дольше 24 часов.
const JWT_SECS: u64 = 12 * 60 * 60;

/// Ключ сервера VAPID.
pub struct Vapid {
    key: SigningKey,
    /// Открытый ключ в base64url: его браузер передаёт в `pushManager.subscribe`.
    public: String,
}

impl Vapid {
    /// Ключ из `PANEL_VAPID_KEY`: закрытый ключ P-256 (32 байта) в base64url.
    pub fn from_b64(text: &str) -> Result<Self, String> {
        let bytes = b64url::decode(text.trim()).ok_or("PANEL_VAPID_KEY is not base64url")?;
        let key = SigningKey::from_slice(&bytes).map_err(|_| "PANEL_VAPID_KEY is not a P-256 private key")?;
        let public = b64url::encode(key.verifying_key().to_encoded_point(false).as_bytes());
        Ok(Self { key, public })
    }

    /// Новая пара ключей: (закрытый, открытый) в base64url.
    pub fn generate() -> (String, String) {
        let key = SigningKey::random(&mut OsRng);
        let private = b64url::encode(&key.to_bytes());
        let public = b64url::encode(key.verifying_key().to_encoded_point(false).as_bytes());
        (private, public)
    }

    pub fn public_key(&self) -> &str {
        &self.public
    }

    /// Заголовок `Authorization` для запроса к `endpoint`: JWT, подписанный ES256.
    /// `contact` — адрес владельца сервера (`mailto:` или `https:`): Apple без
    /// настоящего адреса отклоняет запрос.
    fn authorization(&self, endpoint: &str, contact: &str, now: u64) -> String {
        let header = b64url::encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({ "aud": audience(endpoint), "exp": now + JWT_SECS, "sub": contact });
        let claims = b64url::encode(claims.to_string().as_bytes());
        let signed = format!("{header}.{claims}");
        let signature: Signature = self.key.sign(signed.as_bytes());
        format!("vapid t={signed}.{}, k={}", b64url::encode(&signature.to_bytes()), self.public)
    }
}

/// `aud` в VAPID — схема и хост адреса подписки.
fn audience(endpoint: &str) -> String {
    let rest = endpoint.strip_prefix("https://").unwrap_or(endpoint);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    format!("https://{host}")
}

/// Шифрует `payload` для устройства по RFC 8291. `secret` и `salt` — одноразовые;
/// параметрами они передаются для проверки по примеру из RFC.
fn encrypt(sub: &PushSub, payload: &[u8], secret: &SecretKey, salt: [u8; 16]) -> Result<Vec<u8>, String> {
    let ua_public_bytes = b64url::decode(&sub.p256dh).ok_or("p256dh is not base64url")?;
    let auth = b64url::decode(&sub.auth).ok_or("auth is not base64url")?;
    let ua_public = PublicKey::from_sec1_bytes(&ua_public_bytes).map_err(|_| "p256dh is not a P-256 point")?;
    let as_public = secret.public_key().to_encoded_point(false);
    let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), ua_public.as_affine());

    // Ключ шифрования из общего секрета ECDH и секрета устройства.
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(&ua_public_bytes);
    key_info.extend_from_slice(as_public.as_bytes());
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&auth), shared.raw_secret_bytes())
        .expand(&key_info, &mut ikm)
        .map_err(|_| "HKDF expand failed")?;
    let prk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).map_err(|_| "HKDF expand failed")?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).map_err(|_| "HKDF expand failed")?;

    // Одна запись, она же последняя: разделитель 0x02 без дополнения.
    let mut plain = payload.to_vec();
    plain.push(2);
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|_| "bad AES key")?;
    let sealed = cipher.encrypt(Nonce::from_slice(&nonce), plain.as_slice()).map_err(|_| "AES-GCM failed")?;

    // Заголовок: соль, размер записи, длина и сам открытый ключ сервера.
    let mut body = Vec::with_capacity(16 + 4 + 1 + 65 + sealed.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.as_bytes().len() as u8);
    body.extend_from_slice(as_public.as_bytes());
    body.extend_from_slice(&sealed);
    Ok(body)
}

pub enum SendError {
    /// Подписки больше нет (404/410): устройство отписалось или удалило данные
    /// сайта. Её нужно забыть.
    Gone,
    Other(String),
}

/// Отправляет уведомление. Блокирующий вызов: зовётся из отдельного потока.
pub fn send(vapid: &Vapid, contact: &str, sub: &PushSub, payload: &[u8]) -> Result<(), SendError> {
    let secret = SecretKey::random(&mut OsRng);
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let body = encrypt(sub, payload, &secret, salt).map_err(SendError::Other)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let result = ureq::post(&sub.endpoint)
        .timeout(Duration::from_secs(15))
        .set("TTL", &TTL_SECS.to_string())
        .set("Urgency", "high")
        .set("Content-Encoding", "aes128gcm")
        .set("Content-Type", "application/octet-stream")
        .set("Authorization", &vapid.authorization(&sub.endpoint, contact, now))
        .send_bytes(&body);
    match result {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(404 | 410, _)) => Err(SendError::Gone),
        Err(ureq::Error::Status(code, resp)) => {
            let text: String = resp.into_string().unwrap_or_default().chars().take(300).collect();
            Err(SendError::Other(format!("HTTP {code}: {text}")))
        }
        Err(e) => Err(SendError::Other(e.to_string())),
    }
}

/// Ключ VAPID из `PANEL_VAPID_KEY`. Без него или с битым ключом панель
/// работает, но push-уведомлений нет: из-за них сервер не должен падать.
pub fn load_vapid() -> Option<Vapid> {
    let raw = std::env::var("PANEL_VAPID_KEY").unwrap_or_default();
    if raw.trim().is_empty() {
        eprintln!("PANEL_VAPID_KEY is not set: push notifications are off");
        return None;
    }
    match Vapid::from_b64(&raw) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("{e}: push notifications are off");
            None
        }
    }
}

/// Событие сессии, о котором стоит позвать человека.
#[derive(Debug, PartialEq)]
pub struct StatusEvent {
    pub session: String,
    pub title: String,
    /// Сессия просит выбор (`true`) или закончила ход.
    pub waiting: bool,
}

/// Те же признаки, что у браузерных уведомлений панели (`noteSessions` в
/// app.notify.js): сессия начала ждать ответа или закончила ход. Новые сессии
/// (их не было в прошлом списке) и остальные переходы — не повод для уведомления.
pub fn status_events(old: &[SessionInfo], new: &[SessionInfo]) -> Vec<StatusEvent> {
    const BUSY: [&str; 3] = ["busy", "shell", "stalled"];
    const DONE: [&str; 2] = ["idle", "ready"];
    let mut out = Vec::new();
    for s in new {
        let Some(was) = old.iter().find(|o| o.key == s.key).map(|o| o.status.as_str()) else {
            continue;
        };
        if was == s.status {
            continue;
        }
        let waiting = s.status == "waiting";
        if waiting || (BUSY.contains(&was) && DONE.contains(&s.status.as_str())) {
            let title = if s.title.trim().is_empty() { s.key.clone() } else { s.title.clone() };
            out.push(StatusEvent { session: s.key.clone(), title, waiting });
        }
    }
    out
}

/// Текст уведомления для service worker панели (`sw.js`).
pub fn payload(agent: &str, e: &StatusEvent) -> String {
    let title: String = e.title.chars().take(120).collect();
    json!({
        "title": title,
        "body": format!("{agent} · {}", if e.waiting { "сессия ждёт ответа" } else { "ход завершён" }),
        // Тот же tag, что у уведомлений страницы: новое заменяет прежнее.
        "tag": format!("aipanel:{agent}:{}", e.session),
        "agent": agent,
        "session": e.session,
    })
    .to_string()
}

/// Уведомления, которые нужно разослать устройствам пользователя.
pub struct Job {
    pub owner: String,
    pub agent: String,
    pub events: Vec<StatusEvent>,
    pub subs: Vec<PushSub>,
    pub contact: Option<String>,
}

/// Рассылает уведомления на устройства пользователя. Запросы к push-службам
/// блокирующие и небыстрые, поэтому идут в отдельном потоке; подписки, которых
/// у службы больше нет, после этого забываются.
pub fn dispatch(app: &Arc<App>, job: Job) {
    let app = Arc::clone(app);
    let Job { owner, agent, events, subs, contact } = job;
    let contact = contact.unwrap_or_else(|| "mailto:aipanel@localhost".into());
    std::thread::spawn(move || {
        let Some(vapid) = app.vapid.as_ref() else { return };
        let mut gone = Vec::new();
        for event in &events {
            let text = payload(&agent, event);
            for sub in &subs {
                if gone.contains(&sub.endpoint) {
                    continue;
                }
                match send(vapid, &contact, sub, text.as_bytes()) {
                    Ok(()) => {}
                    Err(SendError::Gone) => {
                        eprintln!("push: subscription gone, dropping: {}", sub.endpoint);
                        gone.push(sub.endpoint.clone());
                    }
                    Err(SendError::Other(e)) => eprintln!("push to {} failed: {e}", sub.endpoint),
                }
            }
        }
        if !gone.is_empty() {
            app.lock().drop_push(&owner, &gone);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{signature::Verifier, VerifyingKey};

    fn d(text: &str) -> Vec<u8> {
        b64url::decode(text).unwrap()
    }

    /// Пример из RFC 8291, раздел 5: те же ключи и соль дают то же тело байт в байт.
    #[test]
    fn encrypts_like_rfc8291_example() {
        let sub = PushSub {
            endpoint: "https://push.example.net/push/JzLQ3raZJfFBR0aqvOMsLrt54w4rJUsV".into(),
            p256dh: "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4".into(),
            auth: "BTBZMqHH6r4Tts7J_aSIgg".into(),
        };
        let secret = SecretKey::from_slice(&d("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
        let salt: [u8; 16] = d("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let body = encrypt(&sub, b"When I grow up, I want to be a watermelon", &secret, salt).unwrap();
        assert_eq!(
            b64url::encode(&body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn vapid_header_is_signed_jwt() {
        let (private, public) = Vapid::generate();
        let vapid = Vapid::from_b64(&private).unwrap();
        assert_eq!(vapid.public_key(), public);
        assert_eq!(d(&public).len(), 65);
        let header = vapid.authorization("https://fcm.googleapis.com/fcm/send/x?y", "https://panel.example", 1000);
        let (t, k) = header.strip_prefix("vapid t=").unwrap().split_once(", k=").unwrap();
        assert_eq!(k, public);
        let parts: Vec<&str> = t.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: serde_json::Value = serde_json::from_slice(&d(parts[1])).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["sub"], "https://panel.example");
        assert_eq!(claims["exp"], 1000 + JWT_SECS);
        let key = VerifyingKey::from_sec1_bytes(&d(&public)).unwrap();
        let signature = Signature::from_slice(&d(parts[2])).unwrap();
        assert!(key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature).is_ok());
    }

    #[test]
    fn rejects_bad_key() {
        assert!(Vapid::from_b64("not a key!").is_err());
        assert!(Vapid::from_b64(&b64url::encode(&[0u8; 32])).is_err());
    }
}
