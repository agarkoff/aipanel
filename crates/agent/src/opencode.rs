//! Сессии opencode через HTTP API его фонового сервиса.
//!
//! Адрес и пароль сервиса opencode хранит в `~/.local/state/opencode/service.json`.

use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use aipanel_proto::{AgentMsg, Item, SessionInfo};
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    config::OpenCodeCfg,
    util::{basename, tool_input, truncate},
};

const TEXT_LIMIT: usize = 20_000;
const RESULT_LIMIT: usize = 4_000;
/// Пока окно запускается, повторное сообщение в ту же сессию не открывает второе окно.
const LAUNCH_GUARD: Duration = Duration::from_secs(25);

/// Файл в каталоге состояния opencode (`~/.local/state/opencode`).
fn state_file(name: &str) -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::util::home_dir().join(".local").join("state"))
        .join("opencode")
        .join(name)
}

pub struct OpenCode {
    cfg: OpenCodeCfg,
    http: reqwest::Client,
    launched: Mutex<HashMap<String, Instant>>,
}

struct Endpoint {
    url: String,
    password: String,
}

impl OpenCode {
    pub fn new(cfg: OpenCodeCfg) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self { cfg, http, launched: Mutex::new(HashMap::new()) }
    }

    /// Сессии, открытые сейчас в окнах opencode, по отчётам плагина AiPanel
    /// (`~/.aipanel/opencode/<pid>.json`). Отчёты мёртвых процессов удаляются.
    fn windowed_sessions(&self) -> HashSet<String> {
        let mut out = HashSet::new();
        let Ok(entries) = std::fs::read_dir(crate::util::aipanel_dir().join("opencode")) else {
            return out;
        };
        let now = crate::util::now_ms();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Some(v) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
            else {
                continue;
            };
            let pid = v["pid"].as_u64().unwrap_or(0) as u32;
            if pid == 0 || !crate::platform::pid_alive(pid) {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            // Плагин обновляет отчёт каждые 5 с; давно не обновлявшийся отчёт устарел.
            let fresh = v["updated"].as_i64().is_some_and(|u| now - u < 60_000);
            if let (true, Some(id)) = (fresh, v["sessionID"].as_str()) {
                out.insert(id.to_owned());
            }
        }
        out
    }

    fn recently_launched(&self, id: &str) -> bool {
        let mut map = self.launched.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, t| t.elapsed() < LAUNCH_GUARD);
        map.contains_key(id)
    }

    fn endpoint(&self) -> Result<Endpoint, String> {
        if let (Some(url), Some(password)) = (&self.cfg.url, &self.cfg.password) {
            return Ok(Endpoint { url: url.trim_end_matches('/').to_owned(), password: password.clone() });
        }
        let path = state_file("service.json");
        let text = std::fs::read_to_string(&path).map_err(|_| "сервис opencode не запущен".to_owned())?;
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        match (v["url"].as_str(), v["password"].as_str()) {
            (Some(url), Some(password)) => Ok(Endpoint {
                url: self.cfg.url.as_deref().unwrap_or(url).trim_end_matches('/').to_owned(),
                password: self.cfg.password.clone().unwrap_or_else(|| password.to_owned()),
            }),
            _ => Err("в service.json opencode нет url или password".into()),
        }
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, String> {
        let ep = self.endpoint()?;
        let mut req = self
            .http
            .request(method, format!("{}{path}", ep.url))
            .basic_auth("opencode", Some(ep.password));
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(|e| format!("opencode: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("opencode: HTTP {status}: {}", truncate(&text, 300)));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        self.call(reqwest::Method::GET, path, None).await
    }

    pub async fn scan(&self) -> Vec<SessionInfo> {
        let Ok(list) = self.get(&format!("/api/session?limit={}", self.cfg.max_sessions)).await else {
            return Vec::new();
        };
        let active: HashSet<String> = self
            .get("/api/session/active")
            .await
            .ok()
            .and_then(|v| v["data"].as_object().map(|o| o.keys().cloned().collect()))
            .unwrap_or_default();
        let waiting: HashSet<String> = self
            .get("/api/permission/request")
            .await
            .ok()
            .and_then(|v| {
                v["data"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|r| r["sessionID"].as_str().map(str::to_owned)).collect())
            })
            .unwrap_or_default();

        let windowed = self.windowed_sessions();
        let mut out = Vec::new();
        for s in list["data"].as_array().into_iter().flatten() {
            let Some(id) = s["id"].as_str() else { continue };
            let cwd = s["location"]["directory"].as_str().unwrap_or_default();
            let title = s["title"].as_str().filter(|t| !t.is_empty()).unwrap_or_else(|| basename(cwd));
            let status = if waiting.contains(id) {
                "waiting"
            } else if active.contains(id) || self.recently_launched(id) {
                "busy"
            } else if windowed.contains(id) {
                "idle"
            } else {
                // Сервис opencode хранит все сессии и примет сообщение в любую из них,
                // но открыта ли сессия в каком-то окне, он не сообщает. Поэтому сессия,
                // которая ничего не выполняет, показывается как неактивная, а не «ждёт ввода».
                "ready"
            };
            out.push(SessionInfo {
                key: format!("opencode:{id}"),
                tool: "opencode".into(),
                title: title.to_owned(),
                cwd: cwd.to_owned(),
                status: status.into(),
                live: status != "ready",
                updated_at: s["time"]["updated"].as_i64().unwrap_or(0),
                delivery: if status == "ready" && cfg!(windows) { "window" } else { "api" }.into(),
            });
        }
        out
    }

    /// Опрашивает ленту сессии и отправляет только изменившиеся элементы.
    pub async fn watch(&self, key: String, id: String, out: UnboundedSender<AgentMsg>) {
        let mut sent: HashMap<String, u64> = HashMap::new();
        let mut first = true;
        loop {
            let limit = if first { 100 } else { 20 };
            match self.get(&format!("/api/session/{id}/message?limit={limit}&order=desc")).await {
                Ok(v) => {
                    let mut items = Vec::new();
                    for m in v["data"].as_array().into_iter().flatten().rev() {
                        convert(m, &mut items);
                    }
                    items.retain(|it| {
                        let mut h = DefaultHasher::new();
                        (&it.kind, &it.text, &it.tool).hash(&mut h);
                        let h = h.finish();
                        sent.insert(it.id.clone(), h) != Some(h)
                    });
                    if first || !items.is_empty() {
                        let _ = out.send(AgentMsg::History { session: key.clone(), reset: first, items });
                    }
                    first = false;
                }
                Err(e) => {
                    if first {
                        let items = vec![Item {
                            id: "err-load".into(),
                            ts: crate::util::now_ms(),
                            role: "system".into(),
                            kind: "error".into(),
                            text: e,
                            tool: None,
                        }];
                        let _ = out.send(AgentMsg::History { session: key.clone(), reset: true, items });
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
    }

    /// Создаёт новую сессию opencode в каталоге `cwd` и открывает её в окне.
    /// Сессию создаёт API, поэтому она сразу известна панели даже без окна.
    pub async fn start(&self, cwd: &str) -> Result<Option<String>, String> {
        crate::util::session_dir(cwd)?;
        let v = self
            .call(
                reqwest::Method::POST,
                "/api/session",
                Some(json!({ "location": { "directory": cwd } })),
            )
            .await?;
        let id = v["data"]["id"]
            .as_str()
            .or_else(|| v["id"].as_str())
            .ok_or("opencode не вернул идентификатор сессии")?
            .to_owned();
        if cfg!(windows) {
            let bin = crate::util::resolve_exe(&self.cfg.bin);
            self.launched.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), Instant::now());
            let args = vec!["-s".to_owned(), id.clone()];
            if let Err(e) = crate::platform::spawn_window(&bin, &args, cwd) {
                self.launched.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                // Сессия без окна пользователю не нужна: убираем её из списка opencode.
                let _ = self.call(reqwest::Method::DELETE, &format!("/api/session/{id}"), None).await;
                return Err(e);
            }
        }
        Ok(Some(format!("opencode:{id}")))
    }

    /// Окно открывается, если сессия ничего не выполняет и её окна ещё нет. Окно ищется
    /// по `-s <id>` в командной строке `opencode.exe`: окно, в котором сессию выбрали
    /// уже внутри программы, так не найти и будет открыто ещё одно.
    async fn should_open_window(&self, id: &str, active: bool) -> bool {
        if !cfg!(windows) || self.recently_launched(id) {
            return false;
        }
        if active || self.windowed_sessions().contains(id) {
            return false;
        }
        // Окна, открытые до установки плагина, видны только по командной строке.
        !crate::platform::process_cmdline_contains("opencode.exe", id).await
    }

    /// Открывает окно Windows Terminal с сессией; сообщение уходит первым запросом.
    async fn open_window(&self, id: &str, text: &str) -> Result<(), String> {
        if text.encode_utf16().count() > crate::claude::MAX_ARG_UTF16 {
            return Err(format!(
                "сообщение слишком длинное для запуска окна (до {} символов)",
                crate::claude::MAX_ARG_UTF16
            ));
        }
        let info = self.get(&format!("/api/session/{id}")).await?;
        let cwd = info["data"]["location"]["directory"]
            .as_str()
            .or(info["location"]["directory"].as_str())
            .unwrap_or_default()
            .to_owned();
        if cwd.is_empty() || !Path::new(&cwd).is_dir() {
            return Err(format!("каталог сессии недоступен: {cwd}"));
        }
        self.launched.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_owned(), Instant::now());
        // Форма `--prompt=<текст>` не даёт принять текст, начинающийся с «-», за параметр.
        let args = vec!["-s".to_owned(), id.to_owned(), format!("--prompt={text}")];
        let bin = crate::util::resolve_exe(&self.cfg.bin);
        crate::platform::spawn_window(&bin, &args, &cwd).map(|_| ()).inspect_err(|_| {
            self.launched.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        })
    }

    /// Модель, на которой должны идти сообщения из панели: `(провайдер, модель)`.
    fn target_model(&self) -> Option<(String, String)> {
        match self.cfg.model.trim() {
            "" | "session" => None,
            "auto" => {
                // Последняя модель, выбранная в окне opencode: именно она видна в строке состояния.
                let text = std::fs::read_to_string(state_file("model.json")).ok()?;
                let v: Value = serde_json::from_str(&text).ok()?;
                let first = v["recent"].as_array()?.first()?;
                Some((first["providerID"].as_str()?.to_owned(), first["modelID"].as_str()?.to_owned()))
            }
            explicit => explicit.split_once('/').map(|(p, m)| (p.to_owned(), m.to_owned())),
        }
    }

    /// Переключает сессию на целевую модель. Сообщение, отправленное без этого, уходит на
    /// модель, сохранённую в сессии (или на умолчание сервера), а не на ту, что видна в окне.
    async fn apply_model(&self, id: &str) {
        let Some((provider, model)) = self.target_model() else { return };
        let Ok(info) = self.get(&format!("/api/session/{id}")).await else { return };
        let cur = if info["data"].is_object() { &info["data"]["model"] } else { &info["model"] };
        if cur["providerID"].as_str() == Some(&provider) && cur["id"].as_str() == Some(&model) {
            return;
        }
        let body = json!({ "model": { "providerID": provider, "id": model } });
        if let Err(e) = self.call(reqwest::Method::POST, &format!("/api/session/{id}/model"), Some(body)).await {
            crate::util::log(&format!("opencode: не удалось переключить модель {provider}/{model}: {e}"));
        }
    }

    pub async fn send(&self, id: &str, text: &str) -> Result<(), String> {
        let active = self
            .get("/api/session/active")
            .await
            .ok()
            .is_some_and(|v| v["data"].get(id).is_some());
        // Модель меняется только у сессии, которая сейчас ничего не выполняет.
        if !active {
            self.apply_model(id).await;
        }
        if self.should_open_window(id, active).await {
            return self.open_window(id, text).await;
        }
        self.call(reqwest::Method::POST, &format!("/api/session/{id}/prompt"), Some(json!({ "text": text })))
            .await
            .map(|_| ())
    }

    pub async fn interrupt(&self, id: &str) -> Result<(), String> {
        self.call(reqwest::Method::POST, &format!("/api/session/{id}/interrupt"), None)
            .await
            .map(|_| ())
    }
}

fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn convert(m: &Value, out: &mut Vec<Item>) {
    let id = m["id"].as_str().unwrap_or_default();
    let ts = m["time"]["created"].as_i64().unwrap_or(0);
    let mut push = |suffix: String, role: &str, kind: &str, text: String, tool: Option<String>| {
        if !text.trim().is_empty() || kind == "tool_use" {
            out.push(Item { id: format!("{id}:{suffix}"), ts, role: role.into(), kind: kind.into(), text, tool });
        }
    };
    match m["type"].as_str() {
        Some("user") => push("0".into(), "user", "text", truncate(m["text"].as_str().unwrap_or_default(), TEXT_LIMIT), None),
        Some("assistant") => {
            for (i, c) in m["content"].as_array().into_iter().flatten().enumerate() {
                match c["type"].as_str() {
                    Some("text") => push(
                        i.to_string(),
                        "assistant",
                        "text",
                        truncate(c["text"].as_str().unwrap_or_default(), TEXT_LIMIT),
                        None,
                    ),
                    Some("reasoning") => push(
                        i.to_string(),
                        "assistant",
                        "thinking",
                        truncate(c["text"].as_str().unwrap_or_default(), TEXT_LIMIT),
                        None,
                    ),
                    Some("tool") => {
                        let state = &c["state"];
                        push(
                            i.to_string(),
                            "assistant",
                            "tool_use",
                            tool_input(&state["input"]),
                            c["name"].as_str().map(str::to_owned),
                        );
                        let mut result = content_text(&state["content"]);
                        if result.is_empty() {
                            result = content_text(&state["error"]);
                        }
                        push(format!("{i}:r"), "tool", "tool_result", truncate(&result, RESULT_LIMIT), None);
                    }
                    _ => {}
                }
            }
            if !m["error"].is_null() {
                let text = m["error"]["message"].as_str().map(str::to_owned).unwrap_or_else(|| m["error"].to_string());
                push("err".into(), "system", "error", truncate(&text, RESULT_LIMIT), None);
            }
        }
        Some("shell") => {
            push("0".into(), "assistant", "tool_use", m["command"].as_str().unwrap_or_default().to_owned(), Some("shell".into()));
            push("0:r".into(), "tool", "tool_result", truncate(&content_text(&m["output"]), RESULT_LIMIT), None);
        }
        // Сообщение типа idle повторяет ошибку предыдущего ответа, поэтому не показывается.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_messages() {
        let mut items = Vec::new();
        convert(&json!({"id":"m1","type":"user","time":{"created":5},"text":"hi"}), &mut items);
        convert(
            &json!({"id":"m2","type":"assistant","time":{"created":6},"content":[
                {"type":"reasoning","text":"hm"},
                {"type":"tool","name":"shell","state":{"status":"completed","input":{"command":"ls"},"content":[{"type":"text","text":"a\nb"}]}},
                {"type":"text","text":"done"}
            ]}),
            &mut items,
        );
        convert(&json!({"id":"m3","type":"idle","time":{"created":7},"outcome":"succeeded"}), &mut items);
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.kind.as_str(), i.text.as_str())).collect();
        assert_eq!(
            got,
            [
                ("m1:0", "text", "hi"),
                ("m2:0", "thinking", "hm"),
                ("m2:1", "tool_use", "ls"),
                ("m2:1:r", "tool_result", "a\nb"),
                ("m2:2", "text", "done"),
            ]
        );
    }
}
