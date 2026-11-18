//! Сессии opencode через HTTP API его фонового сервиса.
//!
//! Адрес и пароль сервиса opencode хранит в `~/.local/state/opencode/service.json`.

mod convert;
mod subagents;
mod tasks;

use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aipanel_proto::{
    AgentMsg, BgTask, Command, Item, Limits, ModelInfo, SessionInfo, Subagent, TurnUsage, MAX_TITLE,
};
use serde_json::{json, Value};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::{
    backend::{Backend, Fut},
    config::OpenCodeCfg,
    handoff,
    util::{basename, meter, truncate},
};

use convert::{
    convert, form_prompt, last_context, models_from, older_page, permission_prompt, prompt_session_ids, prompts_hash,
    queued_from_prompt, queued_items, session_model, split_model, text_hash, turn_from_messages,
};

const TEXT_LIMIT: usize = 20_000;
const RESULT_LIMIT: usize = 4_000;
/// Сколько последних сообщений читается, чтобы собрать токены текущего хода.
/// Ход длиннее страницы считается неизвестным (о таком случае пишется строка в
/// лог, см. `note_turn_limit`): видно, насколько предел мешает на самом деле.
const TURN_PAGE: usize = 500;
/// Сколько последних сообщений подагента читается ради контекста его последнего
/// ответа: сам ответ лежит в начале страницы, глубже искать нечего.
const SUB_CONTEXT_PAGE: usize = 20;
/// Ограничения на запросы выбора из окна opencode: форма приходит из локального
/// сервиса, но её поля попадают в браузер, поэтому размеры ограничены.
const PROMPT_FIELDS: usize = 20;
const PROMPT_OPTIONS: usize = 50;
/// Сколько сообщений opencode отдаёт за один запрос: столько же догружается
/// кнопкой «Загрузить ранние сообщения».
const PAGE: usize = 100;
/// Сколько сессий opencode запрашивается, когда нужны дочерние (подагенты):
/// подпись и id подагента есть только в списке сессий.
const SUBAGENT_SESSIONS: usize = 100;
/// Пока окно запускается, повторное сообщение в ту же сессию не открывает второе окно.
const LAUNCH_GUARD: Duration = Duration::from_secs(25);

/// Отменяет задачу чтения событий, когда наблюдение за сессией прекращается.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Файл в каталоге состояния opencode (`~/.local/state/opencode`).
fn state_file(name: &str) -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::util::home_dir().join(".local").join("state"))
        .join("opencode")
        .join(name)
}

fn endpoint(cfg: &OpenCodeCfg) -> Result<Endpoint, String> {
    if let (Some(url), Some(password)) = (&cfg.url, &cfg.password) {
        return Ok(Endpoint { url: url.trim_end_matches('/').to_owned(), password: password.clone() });
    }
    let path = state_file("service.json");
    let text = std::fs::read_to_string(&path).map_err(|_| "сервис opencode не запущен".to_owned())?;
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    match (v["url"].as_str(), v["password"].as_str()) {
        (Some(url), Some(password)) => Ok(Endpoint {
            url: cfg.url.as_deref().unwrap_or(url).trim_end_matches('/').to_owned(),
            password: cfg.password.clone().unwrap_or_else(|| password.to_owned()),
        }),
        _ => Err("в service.json opencode нет url или password".into()),
    }
}

/// Состояние ленты: хэши доставленного и буферы потоковых частей.
#[derive(Default)]
struct Feed {
    sent: HashMap<String, u64>,
    reasoning: HashMap<(String, i64), String>,
    text: HashMap<(String, i64), String>,
}

impl Feed {
    fn item_hash(item: &Item) -> u64 {
        let mut h = DefaultHasher::new();
        (&item.kind, &item.text, &item.tool).hash(&mut h);
        h.finish()
    }

    /// true, если элемент новый или его текст изменился.
    fn changed(&mut self, item: &Item) -> bool {
        let hash = Self::item_hash(item);
        self.sent.insert(item.id.clone(), hash) != Some(hash)
    }

    /// Элемент из потока событий opencode: накапливает дельту и отдаёт
    /// обновлённую часть с тем же id, что будет у итогового элемента ленты.
    fn event(&mut self, ev: &Value) -> Option<Item> {
        let kind = ev["type"].as_str()?;
        let (prefix, item_kind, buffers) = if kind.starts_with("session.reasoning.") {
            ("reasoning", "thinking", &mut self.reasoning)
        } else if kind.starts_with("session.text.") {
            ("text", "text", &mut self.text)
        } else {
            return None;
        };
        let data = &ev["data"];
        let message = data["assistantMessageID"].as_str().filter(|m| !m.is_empty())?;
        let ordinal = data["ordinal"].as_i64().unwrap_or(0);
        let key = (message.to_owned(), ordinal);
        let text = if kind.ends_with(".ended") {
            data["text"].as_str().unwrap_or_default().to_owned()
        } else if kind.ends_with(".delta") {
            let buf = buffers.entry(key.clone()).or_default();
            buf.push_str(data["delta"].as_str().unwrap_or_default());
            buf.clone()
        } else {
            return None;
        };
        if kind.ends_with(".ended") {
            buffers.remove(&key);
        }
        if text.trim().is_empty() {
            return None;
        }
        let item = Item {
            id: format!("{message}:{prefix}:{ordinal}"),
            ts: ev["created"].as_i64().unwrap_or_else(crate::util::now_ms),
            role: "assistant".into(),
            kind: item_kind.into(),
            text: truncate(&text, TEXT_LIMIT),
            tool: None,
        };
        self.changed(&item).then_some(item)
    }
}

/// Список фоновых задач уходит в панель, только когда меняется: как и лента,
/// он приходит целиком и заменяет показанный.
fn send_tasks(
    out: &UnboundedSender<AgentMsg>,
    key: &str,
    tasks: &tasks::Tracker,
    shown: &mut Option<Vec<BgTask>>,
) {
    let now = tasks.running();
    if shown.as_ref() != Some(&now) {
        let _ = out.send(AgentMsg::Tasks { session: key.to_owned(), tasks: now.clone() });
        *shown = Some(now);
    }
}

/// То же для подагентов: полный список уходит, только когда он изменился.
fn send_subagents(
    out: &UnboundedSender<AgentMsg>,
    key: &str,
    now: Vec<Subagent>,
    shown: &mut Option<Vec<Subagent>>,
) {
    if shown.as_ref() != Some(&now) {
        let _ = out.send(AgentMsg::Subagents { session: key.to_owned(), subagents: now.clone() });
        *shown = Some(now);
    }
}

/// Контекст последнего ответа подагента: выход уже посчитан счётчиками его
/// сессии, здесь только вход. Если счётчиков не было, `turn` создаётся с
/// нулевым выходом — вход всё равно полезен.
fn set_sub_input(sub: &mut Subagent, input: Option<u64>) {
    let Some(input) = input else { return };
    match &mut sub.turn {
        Some(turn) => turn.input = input,
        None => sub.turn = Some(TurnUsage { started_at: sub.started_at, input, output: 0 }),
    }
}

/// Пересылает части рассуждений и текста этой сессии из потока событий opencode.
async fn watch_events(
    cfg: OpenCodeCfg,
    client: reqwest::Client,
    session: String,
    tx: mpsc::Sender<Value>,
) {
    loop {
        if let Ok(ep) = endpoint(&cfg) {
            let req = client
                .get(format!("{}/api/event", ep.url))
                .basic_auth("opencode", Some(ep.password))
                .header("Accept", "text/event-stream");
            if let Ok(mut resp) = req.send().await {
                if resp.status().is_success() {
                    let mut buf = Vec::new();
                    while let Some(line) = next_line(&mut resp, &mut buf).await {
                        let Some(data) = line.strip_prefix("data:") else { continue };
                        let Ok(v) = serde_json::from_str::<Value>(data.trim()) else { continue };
                        let kind = v["type"].as_str().unwrap_or_default();
                        // Концы фоновых команд: по ним задача уходит из списка.
                        // Запуск берётся из сообщений, поэтому `shell.created` тут
                        // не нужен — по нему не отличить фоновую команду от обычной.
                        let ended = kind == "shell.exited" || kind == "shell.deleted";
                        if !ended {
                            if !(kind.starts_with("session.reasoning.") || kind.starts_with("session.text.")) {
                                continue;
                            }
                            if !(kind.ends_with(".delta") || kind.ends_with(".ended")) {
                                continue;
                            }
                            if v["data"]["sessionID"].as_str() != Some(session.as_str()) {
                                continue;
                            }
                        }
                        if tx.send(v).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Читает из потока одну строку SSE. `None` — поток закрылся.
async fn next_line(resp: &mut reqwest::Response, buf: &mut Vec<u8>) -> Option<String> {
    loop {
        if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]);
            return Some(line.trim_end_matches('\r').to_owned());
        }
        match resp.chunk().await {
            Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
            _ => return None,
        }
    }
}

pub struct OpenCode {
    cfg: OpenCodeCfg,
    http: reqwest::Client,
    /// Клиент без общего таймаута: поток событий живёт часами.
    stream: reqwest::Client,
    launched: Mutex<HashMap<String, Instant>>,
    /// Сообщения, показанные сразу после отправки, но ещё не замеченные наблюдением.
    /// Когда наблюдение их увидит (или не найдёт в очереди), оно уберёт их из ленты.
    optimistic: Mutex<HashMap<String, HashSet<String>>>,
    /// Догрузка ранних сообщений: сессия -> курсор на страницу до показанной.
    pages: Mutex<HashMap<String, Page>>,
    /// Контекст последних ответов подагентов: id дочерней сессии ->
    /// (время её изменения, вход последнего ответа). Выход и границы приходят
    /// счётчиками сессии, а контекст читается из сообщений.
    sub_inputs: Mutex<HashMap<String, (i64, Option<u64>)>>,
    /// Когда последний раз писали в лог, что ход не поместился в страницу:
    /// сессия -> время. Иначе журнал забивался бы опросом каждые две секунды.
    turn_limit_logged: Mutex<HashMap<String, Instant>>,
    /// Накопленная стоимость страницы хода за текущее окно.
    turn_cost: Mutex<TurnCost>,
}

/// Место, с которого продолжается догрузка ранних сообщений сессии.
#[derive(Default)]
struct Page {
    /// Курсор opencode на более старые сообщения; `None` — старых больше нет.
    cursor: Option<String>,
    /// Догрузка уже идёт: вторую страницу по тому же курсору не просим.
    loading: bool,
}

/// Стоимость запросов страницы хода: размер и время ответа, число сообщений.
/// Копится между опросами и раз в 10 минут уходит в лог одной строкой — по ней
/// видно, стало ли тяжелее после увеличения страницы.
#[derive(Default)]
struct TurnCost {
    /// Начало окна; `None` — окно ещё не открыто.
    since: Option<Instant>,
    calls: u64,
    bytes: u64,
    max_bytes: usize,
    msgs: u64,
    max_msgs: usize,
    /// Сколько раз страница пришла заполненной целиком.
    full: u64,
    millis: u128,
    max_millis: u128,
    /// Сколько раз токены не посчитались: ход длиннее страницы.
    unknown: u64,
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
        let stream = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            cfg,
            http,
            stream,
            launched: Mutex::new(HashMap::new()),
            optimistic: Mutex::new(HashMap::new()),
            pages: Mutex::new(HashMap::new()),
            sub_inputs: Mutex::new(HashMap::new()),
            turn_limit_logged: Mutex::new(HashMap::new()),
            turn_cost: Mutex::new(TurnCost::default()),
        }
    }

    /// Сессии, открытые сейчас в окнах opencode, по отчётам плагина AiPanel
    /// (`~/.aipanel/opencode/<pid>.json`). Отчёты мёртвых процессов удаляются.
    fn windowed_sessions(&self) -> HashSet<String> {
        let mut out = HashSet::new();
        let now = crate::util::now_ms();
        for dir in crate::util::window_reports_dirs() {
            collect_windowed(&dir, now, &mut out);
        }
        out
    }

    fn recently_launched(&self, id: &str) -> bool {
        let mut map = self.launched.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, t| t.elapsed() < LAUNCH_GUARD);
        map.contains_key(id)
    }

    fn endpoint(&self) -> Result<Endpoint, String> {
        endpoint(&self.cfg)
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, String> {
        self.call_with(method, path, &[], body).await
    }

    /// Запрос с параметрами строки запроса: списку команд нужен каталог сессии,
    /// а он приходит в виде `location[directory]`.
    async fn call_with(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<Value, String> {
        self.call_text(method, path, query, body).await.map(|(v, _)| v)
    }

    /// То же, но с размером ответа в байтах: по нему видно, насколько тяжёл
    /// запрос страницы хода.
    async fn call_text(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<(Value, usize), String> {
        let ep = self.endpoint()?;
        let mut req = self
            .http
            .request(method, format!("{}{path}", ep.url))
            .basic_auth("opencode", Some(ep.password))
            .query(query);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(|e| format!("opencode: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("opencode: HTTP {status}: {}", truncate(&text, 300)));
        }
        let bytes = text.len();
        Ok((serde_json::from_str(&text).unwrap_or(Value::Null), bytes))
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        self.call(reqwest::Method::GET, path, None).await
    }

    /// GET с размером тела ответа: мониторинг страницы хода.
    async fn get_sized(&self, path: &str) -> Result<(Value, usize), String> {
        self.call_text(reqwest::Method::GET, path, &[], None).await
    }

    /// Команды, доступные сессии (`/init`, `/review`, пользовательские из
    /// настроек opencode). Их отдаёт сам opencode — для каталога сессии,
    /// поэтому список у каждого проекта свой.
    pub async fn commands(&self, id: &str) -> Vec<Command> {
        let cwd = self.session_dir(id).await;
        // Каталог сессии уточняет список (команды проекта), но без него opencode
        // отдаёт хотя бы пользовательские команды.
        let query: &[(&str, &str)] = if cwd.is_empty() { &[] } else { &[("location[directory]", cwd.as_str())] };
        let Ok(v) = self.call_with(reqwest::Method::GET, "/api/command", query, None).await else {
            return Vec::new();
        };
        v["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let name = c["name"].as_str().filter(|n| !n.is_empty())?;
                Some(Command {
                    name: name.to_owned(),
                    description: c["description"].as_str().unwrap_or_default().chars().take(200).collect(),
                })
            })
            .collect()
    }

    /// Модели, которые можно выбрать в сессии: их отдаёт opencode для её каталога.
    pub async fn models(&self, id: &str) -> Vec<ModelInfo> {
        let cwd = self.session_dir(id).await;
        self.models_at(&cwd).await
    }

    /// Модели для каталога: нужны и сессии, и окну «Новая сессия», где сессии
    /// ещё нет. Список большой (все подключённые провайдеры), поэтому в панель
    /// уходят только провайдер, id и название.
    pub async fn models_at(&self, cwd: &str) -> Vec<ModelInfo> {
        let cwd = cwd.trim();
        let query: &[(&str, &str)] = if cwd.is_empty() { &[] } else { &[("location[directory]", cwd)] };
        let Ok(v) = self.call_with(reqwest::Method::GET, "/api/model", query, None).await else {
            return Vec::new();
        };
        models_from(&v)
    }

    /// Переключает модель сессии и запоминает выбор там же, откуда его берёт
    /// `model = "auto"`: иначе следующее сообщение из панели вернуло бы прежнюю.
    pub async fn set_model(&self, id: &str, provider: &str, model: &str) -> Result<(), String> {
        if provider.is_empty() || model.is_empty() || provider.len() > 200 || model.len() > 200 {
            return Err("некорректная модель".into());
        }
        let body = json!({ "model": { "providerID": provider, "id": model } });
        self.call(reqwest::Method::POST, &format!("/api/session/{id}/model"), Some(body)).await?;
        remember_model(provider, model);
        Ok(())
    }

    /// Каталог сессии: от него зависят и команды, и список моделей opencode.
    async fn session_dir(&self, id: &str) -> String {
        let info = self.get(&format!("/api/session/{id}")).await.unwrap_or(Value::Null);
        info["data"]["location"]["directory"]
            .as_str()
            .or(info["location"]["directory"].as_str())
            .unwrap_or_default()
            .to_owned()
    }

    /// Токены текущего хода по последним сообщениям сессии. Заодно копится
    /// стоимость страницы: её размер, время ответа и число сообщений.
    async fn turn(&self, id: &str) -> Option<TurnUsage> {
        let started = Instant::now();
        let path = format!("/api/session/{id}/message?limit={TURN_PAGE}&order=desc");
        let Ok((v, bytes)) = self.get_sized(&path).await else { return None };
        let msgs = v["data"].as_array()?;
        let read = turn_from_messages(msgs);
        if read.beyond_page {
            self.note_turn_limit(id);
        }
        self.note_turn_cost(bytes, msgs.len(), read.beyond_page, started.elapsed());
        read.usage
    }

    /// Копит стоимость страницы хода и раз в 10 минут пишет сводку в лог:
    /// средний и максимальный размер ответа, число сообщений, время, сколько раз
    /// страница была полной и сколько раз токены не посчитались.
    fn note_turn_cost(&self, bytes: usize, msgs: usize, beyond: bool, took: Duration) {
        const WINDOW: Duration = Duration::from_secs(10 * 60);
        let mut c = self.turn_cost.lock().unwrap_or_else(|e| e.into_inner());
        let since = *c.since.get_or_insert_with(Instant::now);
        c.calls += 1;
        c.bytes += bytes as u64;
        c.max_bytes = c.max_bytes.max(bytes);
        c.msgs += msgs as u64;
        c.max_msgs = c.max_msgs.max(msgs);
        c.full += u64::from(msgs >= TURN_PAGE);
        c.millis += took.as_millis();
        c.max_millis = c.max_millis.max(took.as_millis());
        c.unknown += u64::from(beyond);
        if since.elapsed() < WINDOW {
            return;
        }
        let calls = c.calls.max(1);
        crate::util::log(&format!(
            "opencode: страница хода {TURN_PAGE} — за {} мин {calls} опросов: {} КБ в среднем, {} КБ максимум; \
             сообщений {} в среднем, {} максимум, полная страница {} раз; {} мс в среднем, {} мс максимум; \
             токены не посчитаны {} раз",
            WINDOW.as_secs() / 60,
            c.bytes / 1024 / calls,
            c.max_bytes / 1024,
            c.msgs / calls,
            c.max_msgs,
            c.full,
            c.millis / calls as u128,
            c.max_millis,
            c.unknown
        ));
        *c = TurnCost::default();
    }

    /// Ход длиннее страницы: токены не показаны. Пишем в лог не чаще раза в час
    /// на сессию: по этим строкам видно, насколько предел `TURN_PAGE` мешает в жизни.
    fn note_turn_limit(&self, id: &str) {
        const EVERY: Duration = Duration::from_secs(60 * 60);
        let mut map = self.turn_limit_logged.lock().unwrap_or_else(|e| e.into_inner());
        if map.get(id).is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        map.retain(|_, t| t.elapsed() < EVERY);
        map.insert(id.to_owned(), Instant::now());
        crate::util::log(&format!(
            "opencode: ход сессии {id} длиннее {TURN_PAGE} сообщений — токены не показаны"
        ));
    }

    /// Контекст последнего ответа подагента: выход и границы приходят
    /// счётчиками сессии прямо в списке, а контекст есть только в её последнем
    /// ответе — он лежит в начале первой страницы. Перечитывается, только
    /// когда `time.updated` сессии сдвинулся.
    async fn with_sub_inputs(&self, subs: &mut [Subagent], stamps: &HashMap<String, i64>) {
        let mut missing = Vec::new();
        for (i, sub) in subs.iter_mut().enumerate() {
            let stamp = stamps.get(&sub.id).copied().unwrap_or(0);
            let cached = {
                let cache = self.sub_inputs.lock().unwrap_or_else(|e| e.into_inner());
                cache.get(&sub.id).filter(|(at, _)| *at == stamp).map(|(_, input)| *input)
            };
            match cached {
                Some(input) => set_sub_input(sub, input),
                None => missing.push((i, sub.id.clone())),
            }
        }
        let loaded = futures_util::future::join_all(missing.into_iter().map(|(i, id)| async move {
            let path = format!("/api/session/{id}/message?limit={SUB_CONTEXT_PAGE}&order=desc");
            let input = self.get(&path).await.ok().and_then(|v| last_context(v["data"].as_array()?));
            (i, id, input)
        }))
        .await;
        for (i, id, input) in loaded {
            set_sub_input(&mut subs[i], input);
            let stamp = stamps.get(&id).copied().unwrap_or(0);
            self.sub_inputs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id, (stamp, input));
        }
        // Кеш живёт, пока подагент есть в списке сессии.
        self.sub_inputs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|id, _| subs.iter().any(|s| &s.id == id));
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
        let waiting = self.waiting_sessions(&list, &active).await;
        let windowed = self.windowed_sessions();
        let mut out = Vec::new();
        for s in list["data"].as_array().into_iter().flatten() {
            let Some(id) = s["id"].as_str() else { continue };
            // Дочерняя сессия — подагент: у неё своя лента в панели, а в списке
            // сессий ей делать нечего.
            if s["parentID"].as_str().is_some_and(|p| !p.is_empty()) {
                continue;
            }
            let cwd = s["location"]["directory"].as_str().unwrap_or_default();
            let title = s["title"].as_str().filter(|t| !t.is_empty()).unwrap_or_else(|| basename(cwd));
            let status = session_status(
                waiting.contains(id),
                active.contains(id) || self.recently_launched(id),
                windowed.contains(id),
                &s["time"],
            );
            let turn = if matches!(status, "busy" | "stalled") { self.turn(id).await } else { None };
            out.push(SessionInfo {
                key: format!("opencode:{id}"),
                tool: "opencode".into(),
                title: title.to_owned(),
                cwd: cwd.to_owned(),
                status: status.into(),
                live: status != "ready",
                updated_at: s["time"]["updated"].as_i64().unwrap_or(0),
                delivery: if status == "ready" && cfg!(windows) { "window" } else { "api" }.into(),
                model: session_model(&s["model"]),
                turn,
                repo: None,
            });
        }
        out
    }

    /// Сессии, которые ждут ответа пользователя: разрешение на действие или
    /// форму с вариантами ответа. Оба списка opencode отдаёт по каталогу, а без
    /// него берёт каталог своего процесса; поэтому спрашиваем каталог каждой
    /// сессии, которая выглядела бы занятой. Иначе ожидание выбора показывалось
    /// бы жёлтым «работает» вместо «ждёт ответа». Каталогов столько же, сколько
    /// проектов у таких сессий, обычно один-два.
    async fn waiting_sessions(&self, list: &Value, active: &HashSet<String>) -> HashSet<String> {
        let mut dirs: Vec<&str> = Vec::new();
        for s in list["data"].as_array().into_iter().flatten() {
            let Some(id) = s["id"].as_str() else { continue };
            // Выполняется или принята, но ход ещё не начат — как в `session_status`.
            let accepted =
                s["time"]["busy"].as_i64().unwrap_or(0) > s["time"]["idle"].as_i64().unwrap_or(0);
            if !active.contains(id) && !accepted {
                continue;
            }
            let dir = s["location"]["directory"].as_str().unwrap_or_default();
            if !dir.is_empty() && !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        let mut out = HashSet::new();
        for dir in dirs {
            let query = [("location[directory]", dir)];
            let (perms, forms) = tokio::join!(
                self.call_with(reqwest::Method::GET, "/api/permission/request", &query, None),
                self.call_with(reqwest::Method::GET, "/api/form", &query, None),
            );
            for v in [&perms, &forms] {
                if let Ok(v) = v {
                    out.extend(prompt_session_ids(v).map(str::to_owned));
                }
            }
        }
        out
    }

    /// Опрашивает ленту сессии и отправляет только изменившиеся элементы.
    ///
    /// Кроме самой ленты читается очередь сессии (`/inbox`): занятый opencode не
    /// показывает отправленные ему сообщения, пока не закончит текущий ответ, и они
    /// лежат в очереди. Без этого панель показывала бы только начало переписки.
    ///
    /// Параллельно читается поток событий opencode: рассуждения и текст приходят
    /// туда по мере генерации, а не только после завершения шага. Запросы выбора
    /// (разрешения и формы) опрашиваются вместе с лентой и приходят в панель, где
    /// на них можно ответить, не переключаясь в окно opencode.
    pub async fn watch(&self, key: String, id: String, out: UnboundedSender<AgentMsg>) {
        let mut feed = Feed::default();
        let mut queued: HashSet<String> = HashSet::new();
        let mut first = true;
        let mut prompts_sig: Option<u64> = None;
        // Фоновые задачи: команды с `background: true` и подагенты.
        let mut tasks = tasks::Tracker::default();
        let mut shown_tasks: Option<Vec<BgTask>> = None;
        // Подагенты: дочерние сессии. Их вызовы в ленту не попадают, панель
        // показывает список.
        let mut shown_subs: Option<Vec<Subagent>> = None;
        let (tx, mut events) = mpsc::channel(256);
        let _events = AbortOnDrop(tokio::spawn(watch_events(
            self.cfg.clone(),
            self.stream.clone(),
            id.clone(),
            tx,
        )));
        let mut tick = tokio::time::interval(Duration::from_millis(1500));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let limit = if first { PAGE } else { 20 };
                    let mut items = Vec::new();
                    let mut drop = Vec::new();
                    // Есть ли перед первой лентой более ранние сообщения: по этому
                    // признаку панель показывает кнопку догрузки.
                    let mut more = false;
                    // Хэши уже доставленных сообщений: очередь может ещё содержать
                    // сообщение, которое уже попало в ленту.
                    let mut delivered: HashSet<u64> = HashSet::new();
                    match self.get(&format!("/api/session/{id}/message?limit={limit}&order=desc")).await {
                        Ok(v) => {
                            for m in v["data"].as_array().into_iter().flatten().rev() {
                                if m["type"].as_str() == Some("user") {
                                    if let Some(text) = m["text"].as_str().filter(|t| !t.trim().is_empty()) {
                                        delivered.insert(text_hash(text));
                                    }
                                }
                                convert(m, &mut items);
                                tasks.message(m);
                            }
                            items.retain(|it| feed.changed(it));
                            // Первая страница — та, что панель показала сразу:
                            // её курсор начинает догрузку ранних сообщений. Курсор
                            // opencode отдаёт и на последней странице (за концом),
                            // поэтому о конце говорит неполная страница.
                            if first {
                                let full = v["data"].as_array().map_or(0, Vec::len) >= PAGE;
                                let cursor = v["cursor"]["next"].as_str().filter(|c| !c.is_empty()).filter(|_| full);
                                self.remember_page(&id, cursor);
                                more = cursor.is_some();
                            }
                        }
                        Err(e) => {
                            if first {
                                items.push(Item {
                                    id: "err-load".into(),
                                    ts: crate::util::now_ms(),
                                    role: "system".into(),
                                    kind: "error".into(),
                                    text: e,
                                    tool: None,
                                });
                            }
                        }
                    }

                    // Сообщения, показанные сразу после отправки: наблюдение должно
                    // знать их id, чтобы убрать из ленты, если opencode успел
                    // доставить их до первого опроса.
                    if let Some(extra) = self
                        .optimistic
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id)
                    {
                        queued.extend(extra);
                    }

                    let mut current: HashSet<String> = HashSet::new();
                    if let Ok(v) = self.get(&format!("/api/session/{id}/inbox")).await {
                        for item in queued_items(&v, &delivered) {
                            current.insert(item.id.clone());
                            // При первой отправке клиент очищает ленту, поэтому очередь
                            // нужно прислать заново, даже если её элементы уже
                            // показывались сразу после отправки.
                            if first || !queued.contains(&item.id) {
                                items.push(item);
                            }
                        }
                    }
                    for old in queued.difference(&current) {
                        drop.push(old.clone());
                    }
                    queued = current;
                    if first {
                        // reset уже очищает ленту, отдельные удаления не нужны.
                        drop.clear();
                    }

                    // Разрешения и формы сессии. Если сервис не ответил, показанные
                    // запросы не трогаем: временный сбой не должен их скрывать.
                    let perms_path = format!("/api/session/{id}/permission");
                    let forms_path = format!("/api/session/{id}/form");
                    let (perms, forms) = tokio::join!(self.get(&perms_path), self.get(&forms_path));
                    let answered = perms.is_ok() || forms.is_ok();
                    let mut prompts = Vec::new();
                    if let Ok(v) = perms {
                        for p in v["data"].as_array().into_iter().flatten() {
                            if let Some(prompt) = permission_prompt(p) {
                                prompts.push(prompt);
                            }
                        }
                    }
                    if let Ok(v) = forms {
                        for f in v["data"].as_array().into_iter().flatten() {
                            if let Some(prompt) = form_prompt(f) {
                                prompts.push(prompt);
                            }
                        }
                    }
                    if answered {
                        let sig = prompts_hash(&prompts);
                        if prompts_sig != Some(sig) {
                            let _ = out.send(AgentMsg::Prompts { session: key.clone(), prompts });
                            prompts_sig = Some(sig);
                        }
                    }

                    if first || !items.is_empty() || !drop.is_empty() {
                        let _ = out.send(AgentMsg::History {
                            session: key.clone(),
                            reset: first,
                            items,
                            drop,
                            more,
                        });
                        first = false;
                    }
                    send_tasks(&out, &key, &tasks, &mut shown_tasks);
                    // Подагенты — дочерние сессии opencode: id, подпись и тип
                    // агента есть в списке сессий, работа — в списке активных.
                    // Сам вызов инструмента для этого не годится: у синхронного
                    // `subagent` в нём нет ни id сессии, ни статуса.
                    let list_path = format!("/api/session?limit={SUBAGENT_SESSIONS}");
                    let (session_list, active) =
                        tokio::join!(self.get(&list_path), self.get("/api/session/active"));
                    if let Ok(session_list) = session_list {
                        let active = active.unwrap_or(Value::Null);
                        let mut now = subagents::list(&session_list, &active, &id);
                        let stamps = subagents::updated(&session_list, &id);
                        self.with_sub_inputs(&mut now, &stamps).await;
                        send_subagents(&out, &key, now, &mut shown_subs);
                    }
                }
                event = events.recv() => {
                    let Some(event) = event else { break };
                    // Концы фоновых команд нужны и до первой ленты: сама лента
                    // ещё не отправлена, а список задач уже собирается.
                    tasks.event(&event);
                    // До первой ленты браузер ещё не получил reset: пропускаем.
                    if first {
                        continue;
                    }
                    if let Some(item) = feed.event(&event) {
                        let _ = out.send(AgentMsg::History {
                            session: key.clone(),
                            reset: false,
                            items: vec![item],
                            drop: Vec::new(),
                            more: false,
                        });
                    }
                }
            }
        }
    }

    /// Следит за лентой подагента `sub` — дочерней сессии opencode: её сообщения
    /// лежат по тому же адресу, что и у обычной сессии. Очередь, задачи и
    /// запросы подагента панели не нужны, поэтому здесь только сама лента.
    pub async fn watch_sub(&self, key: String, _id: String, sub: String, out: UnboundedSender<AgentMsg>) {
        let mut feed = Feed::default();
        let mut first = true;
        // Ошибку сервиса повторяем, только когда она меняется: иначе подсказка
        // в панели обновлялась бы каждые полторы секунды.
        let mut shown_error: Option<String> = None;
        let mut tick = tokio::time::interval(Duration::from_millis(1500));
        loop {
            tick.tick().await;
            let mut items = Vec::new();
            let error = match self.get(&format!("/api/session/{sub}/message?limit={PAGE}&order=desc")).await {
                Ok(v) => {
                    for m in v["data"].as_array().into_iter().flatten().rev() {
                        convert(m, &mut items);
                    }
                    items.retain(|it| feed.changed(it));
                    None
                }
                Err(e) => Some(e),
            };
            let error_changed = error != shown_error;
            if first || !items.is_empty() || error_changed {
                let _ = out.send(AgentMsg::SubHistory {
                    session: key.clone(),
                    sub: sub.clone(),
                    reset: first,
                    items,
                    more: false,
                    error: error.clone(),
                });
                shown_error = error;
                first = false;
            }
        }
    }

    /// Запоминает, откуда продолжится догрузка ранних сообщений. Её начинает
    /// курсор первой страницы, а дальше — курсор каждой следующей.
    fn remember_page(&self, id: &str, cursor: Option<&str>) {
        let cursor = cursor.filter(|c| !c.is_empty()).map(str::to_owned);
        self.pages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_owned(), Page { cursor, loading: false });
    }

    /// Страница ранних сообщений ленты — следующая за уже показанной. `more` —
    /// остались ли за ней ещё более старые.
    ///
    /// Страницы идут по курсору (`cursor.next` из ответа `order=desc`), поэтому не
    /// пересекаются с показанным: повторно те же сообщения не придут. Курсор
    /// появился не во всех версиях opencode; там, где его нет, `more` сразу false
    /// и панель не предлагает догрузку.
    pub async fn older(&self, id: &str) -> Result<(Vec<Item>, bool), String> {
        let cursor = {
            let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
            let page = pages.get_mut(id).ok_or("лента сессии ещё не загружена")?;
            // Второй запрос по тому же курсору вернул бы ту же страницу: пока
            // первая в пути, второй отвечает «пока ничего».
            if page.loading {
                return Ok((Vec::new(), true));
            }
            match page.cursor.clone() {
                Some(cursor) => {
                    page.loading = true;
                    cursor
                }
                None => return Ok((Vec::new(), false)),
            }
        };
        let limit = PAGE.to_string();
        let result = self
            .call_with(
                reqwest::Method::GET,
                &format!("/api/session/{id}/message"),
                &[("limit", limit.as_str()), ("cursor", cursor.as_str())],
                None,
            )
            .await;
        let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        let page = pages.entry(id.to_owned()).or_default();
        page.loading = false;
        let v = result?;
        let (items, more) = older_page(&v);
        // Дальше ведёт курсор этой страницы; если она последняя, он уже не нужен.
        page.cursor = if more { v["cursor"]["next"].as_str().filter(|c| !c.is_empty()).map(str::to_owned) } else { None };
        Ok((items, more))
    }

    /// Каталог и выжимка из ленты для продолжения в другом инструменте.
    pub async fn handoff_source(&self, id: &str) -> Result<(String, String), String> {
        let info = self.get(&format!("/api/session/{id}")).await?;
        let cwd = info["data"]["location"]["directory"]
            .as_str()
            .or(info["location"]["directory"].as_str())
            .unwrap_or_default()
            .to_owned();
        if cwd.is_empty() {
            return Err("каталог сессии неизвестен".into());
        }
        let v = self.get(&format!("/api/session/{id}/message?limit=100&order=desc")).await?;
        let mut messages: Vec<(String, String)> = Vec::new();
        for m in v["data"].as_array().into_iter().flatten().rev() {
            match m["type"].as_str() {
                Some("user") => messages.push((
                    "user".into(),
                    m["text"].as_str().unwrap_or_default().to_owned(),
                )),
                Some("assistant") => {
                    for c in m["content"].as_array().into_iter().flatten() {
                        if c["type"].as_str() == Some("text") {
                            messages.push((
                                "assistant".into(),
                                c["text"].as_str().unwrap_or_default().to_owned(),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        if messages.iter().all(|(_, text)| text.trim().is_empty()) {
            return Err("в сессии нет текста для переноса".into());
        }
        Ok((cwd.clone(), handoff::build("opencode", &cwd, &messages)))
    }

    /// Создаёт новую сессию opencode в каталоге `cwd` и открывает её в окне.
    /// Сессию создаёт API, поэтому она сразу известна панели даже без окна.
    pub async fn start(&self, cwd: &str, model: &str) -> Result<Option<String>, String> {
        self.start_with_prompt(cwd, None, model).await
    }

    /// Открывает новую сессию; `prompt` уходит её первым сообщением, `model` —
    /// `провайдер/модель` для новой сессии (пусто — модель окна). В Windows
    /// текст передаётся аргументом окна, иначе — через API.
    pub async fn start_with_prompt(
        &self,
        cwd: &str,
        prompt: Option<&str>,
        model: &str,
    ) -> Result<Option<String>, String> {
        crate::util::session_dir(cwd)?;
        if let Some(text) = prompt {
            if text.encode_utf16().count() > crate::claude::MAX_ARG_UTF16 {
                return Err(format!(
                    "сообщение слишком длинное для запуска окна (до {} символов)",
                    crate::claude::MAX_ARG_UTF16
                ));
            }
        }
        let chosen = split_model(model)?;
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
        // Модель ставим до окна: оно показывает сессию уже с выбранной моделью.
        if let Some((provider, model)) = chosen {
            if let Err(e) = self.set_model(&id, provider, model).await {
                let _ = self.call(reqwest::Method::DELETE, &format!("/api/session/{id}"), None).await;
                return Err(e);
            }
        }
        if cfg!(windows) {
            let bin = crate::util::resolve_exe(&self.cfg.bin);
            self.launched.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), Instant::now());
            let mut args = vec!["-s".to_owned(), id.clone()];
            // Форма `--prompt=<текст>` не даёт принять текст, начинающийся с «-», за параметр.
            if let Some(text) = prompt {
                args.push(format!("--prompt={text}"));
            }
            if let Err(e) = crate::platform::spawn_window(&bin, &args, cwd) {
                self.launched.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                // Сессия без окна пользователю не нужна: убираем её из списка opencode.
                let _ = self.call(reqwest::Method::DELETE, &format!("/api/session/{id}"), None).await;
                return Err(e);
            }
        } else if let Some(text) = prompt {
            // Явно выбранную модель не перебиваем моделью окна.
            if chosen.is_none() {
                self.apply_model(&id).await;
            }
            self.call(reqwest::Method::POST, &format!("/api/session/{id}/prompt"), Some(json!({ "text": text })))
                .await?;
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

    /// Отправляет сообщение. Возвращает элемент очереди, если opencode принял
    /// сообщение, но ещё не отдал его в ленту (сессия занята).
    pub async fn send(&self, id: &str, text: &str) -> Result<Option<Item>, String> {
        // Слэш-команду opencode выполняет не как текст сообщения, а отдельным
        // запросом: только он разворачивает шаблон команды. Имя сверяется со
        // списком команд, поэтому сообщение «/путь/к/файлу» остаётся сообщением.
        if let Some((name, args)) = crate::util::slash_command(text) {
            if self.commands(id).await.iter().any(|c| c.name == name) {
                self.call(
                    reqwest::Method::POST,
                    &format!("/api/session/{id}/command"),
                    Some(json!({ "name": name, "text": args })),
                )
                .await?;
                return Ok(None);
            }
        }
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
            self.open_window(id, text).await?;
            return Ok(None);
        }
        let v = self
            .call(
                reqwest::Method::POST,
                &format!("/api/session/{id}/prompt"),
                Some(json!({ "text": text })),
            )
            .await?;
        Ok(queued_from_prompt(&v, text))
    }

    /// Запоминает сообщение, уже показанное клиенту: наблюдение уберёт его из ленты,
    /// когда opencode доставит сообщение или если его больше нет в очереди.
    pub fn note_optimistic(&self, id: &str, item: &str) {
        self.optimistic
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.to_owned())
            .or_default()
            .insert(item.to_owned());
    }

    /// Убирает сообщение из очереди, пока opencode его не отправил.
    pub async fn cancel_queued(&self, id: &str, queue: &str) -> Result<(), String> {
        self.call(reqwest::Method::DELETE, &format!("/api/session/{id}/inbox/{queue}"), None)
            .await
            .map(|_| ())
    }

    /// Переименовывает сессию: заголовок в списке сессий opencode меняется через
    /// его API. Само окно opencode получает новое имя при следующем обновлении.
    pub async fn rename(&self, id: &str, title: &str) -> Result<(), String> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > MAX_TITLE {
            return Err(format!("название сессии — от 1 до {MAX_TITLE} символов"));
        }
        if title.chars().any(char::is_control) {
            return Err("в названии есть управляющие символы".into());
        }
        self.call(reqwest::Method::PATCH, &format!("/api/session/{id}"), Some(json!({ "title": title })))
            .await
            .map(|_| ())
    }

    /// Удаляет сессию через API opencode. Занятую или открытую в окне сессию
    /// без `close` не трогаем: opencode мог бы продолжить писать в неё после
    /// удаления. С `close` сначала закрываем окна с ней и прерываем ход.
    pub async fn delete(&self, id: &str, close: bool) -> Result<(), String> {
        if close {
            self.close(id).await?;
        }
        if self.windowed_sessions().contains(id) || self.recently_launched(id) {
            return Err("сессия открыта в окне opencode — закройте её перед удалением".into());
        }
        if self.is_active(id).await {
            return Err("сессия выполняется — дождитесь завершения или прервите её".into());
        }
        self.call(reqwest::Method::DELETE, &format!("/api/session/{id}"), None).await?;
        self.pages.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        Ok(())
    }

    async fn is_active(&self, id: &str) -> bool {
        self.get("/api/session/active")
            .await
            .ok()
            .is_some_and(|v| v["data"].get(id).is_some())
    }

    /// Закрывает окна opencode с сессией `id` и прерывает её ход, затем ждёт,
    /// пока окна завершатся и сессия перестанет выполняться.
    pub(crate) async fn close(&self, id: &str) -> Result<(), String> {
        let pids = window_pids(id);
        for &pid in &pids {
            crate::platform::kill_pid(pid);
        }
        if self.is_active(id).await {
            let path = format!("/api/session/{id}/interrupt");
            let _ = self.call(reqwest::Method::POST, &path, None).await;
        }
        let deadline = std::time::Instant::now() + crate::claude::CLOSE_WAIT;
        loop {
            let alive = pids.iter().any(|&p| crate::platform::pid_alive(p));
            if !alive && !self.is_active(id).await {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(if alive { "не удалось закрыть окно opencode" } else { "сессия не прервалась" }.into());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Остаток лимитов подписки OpenCode Go. Ключ хранится в базе opencode;
    /// берём его через её же API, чтобы не открывать базу напрямую.
    pub async fn limits(&self) -> Option<Limits> {
        let creds = self.get("/api/credential").await.ok()?;
        let key = creds["data"]
            .as_array()?
            .iter()
            .filter(|c| c["integrationID"].as_str() == Some("opencode-go"))
            .filter(|c| c["active"].as_bool().unwrap_or(true))
            .find_map(|c| c["value"]["key"].as_str())
            .filter(|k| !k.is_empty())?;
        let resp = self
            .http
            .get("https://opencode.ai/zen/go/v1/usage")
            .bearer_auth(key)
            .header("user-agent", format!("aipanel-agent/{}", env!("CARGO_PKG_VERSION")))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: Value = resp.json().await.ok()?;
        let mut meters = Vec::new();
        for (key, label) in [("rolling", "5 ч"), ("weekly", "неделя"), ("monthly", "месяц")] {
            let Some(used) = v["usage"][key]["percent"].as_f64() else { continue };
            let mut m = meter(label, used, v["usage"][key]["resetsAt"].as_str());
            // Исчерпанный лимит API помечает отдельно: процент мог остаться прежним.
            if v["usage"][key]["status"].as_str() == Some("rate-limited") {
                m.left = 0;
            }
            meters.push(m);
        }
        if meters.is_empty() {
            return None;
        }
        Some(Limits { tool: "opencode".into(), title: "OpenCode Go".into(), meters, updated_at: crate::util::now_ms() })
    }

    /// Отвечает на запрос разрешения или на форму с вариантами.
    #[allow(clippy::too_many_arguments)]
    pub async fn reply_prompt(
        &self,
        id: &str,
        prompt: &str,
        kind: &str,
        decision: &str,
        answer: &Value,
        message: &str,
        cancel: bool,
    ) -> Result<(), String> {
        match kind {
            "permission" => {
                if !matches!(decision, "once" | "always" | "reject") {
                    return Err(format!("неизвестный ответ на разрешение: {decision}"));
                }
                let mut body = json!({ "decision": decision });
                if !message.is_empty() {
                    body["message"] = json!(message);
                }
                self.call(
                    reqwest::Method::POST,
                    &format!("/api/session/{id}/permission/{prompt}/reply"),
                    Some(body),
                )
                .await
                .map(|_| ())
            }
            "form" => {
                if cancel {
                    return self
                        .call(reqwest::Method::DELETE, &format!("/api/session/{id}/form/{prompt}"), None)
                        .await
                        .map(|_| ());
                }
                let answer = answer
                    .as_object()
                    .ok_or("ответ формы должен быть объектом с полями")?;
                self.call(
                    reqwest::Method::POST,
                    &format!("/api/session/{id}/form/{prompt}/reply"),
                    Some(json!({ "answer": answer })),
                )
                .await
                .map(|_| ())
            }
            _ => Err("неизвестный тип запроса opencode".into()),
        }
    }

    /// Прерывает выполнение сессии.
    ///
    /// Если прерывать нечего (`interrupted: false`), просит opencode возобновить
    /// отложенный ввод: «залипший» ход часто выглядит именно так — сообщение
    /// принято, но выполнение ещё не запущено, и обычный interrupt ничего не делает.
    pub async fn interrupt(&self, id: &str) -> Result<(), String> {
        let path = format!("/api/session/{id}/interrupt");
        if self.call(reqwest::Method::POST, &path, None).await?["interrupted"].as_bool() == Some(true) {
            return Ok(());
        }
        self.call(reqwest::Method::POST, &format!("{path}?resume=true"), None)
            .await
            .map(|_| ())
    }
}

/// Статус сессии opencode для панели.
///
/// `stalled` — сообщение принято, но opencode ещё не выполняет ход: сессии нет
/// в списке активных, а отметка занятости новее отметки простоя. Так выглядит
/// «залипшее» окно: запрос уже в сессии, ответа нет, и обычный `busy` не виден.
fn session_status(waiting: bool, active: bool, windowed: bool, time: &Value) -> &'static str {
    if waiting {
        "waiting"
    } else if active {
        "busy"
    } else if time["busy"].as_i64().unwrap_or(0) > time["idle"].as_i64().unwrap_or(0) {
        "stalled"
    } else if windowed {
        "idle"
    } else {
        // Сервис opencode хранит все сессии и примет сообщение в любую из них,
        // но открыта ли сессия в каком-то окне, он не сообщает. Поэтому сессия,
        // которая ничего не выполняет, показывается как неактивная, а не «ждёт ввода».
        "ready"
    }
}

/// Добавляет в `out` сессии, чьи окна живы и чей отчёт свежий.
fn collect_windowed(dir: &Path, now: i64, out: &mut HashSet<String>) {
    each_window(dir, now, |id, _| {
        out.insert(id.to_owned());
    });
}

/// Процессы окон opencode, в которых сейчас открыта сессия `id`.
fn window_pids(id: &str) -> Vec<u32> {
    let now = crate::util::now_ms();
    let mut out = Vec::new();
    for dir in crate::util::window_reports_dirs() {
        each_window(&dir, now, |sid, pid| {
            if sid == id {
                out.push(pid);
            }
        });
    }
    out
}

/// Обходит живые окна opencode со свежим отчётом: `f(id сессии, pid окна)`.
fn each_window(dir: &Path, now: i64, mut f: impl FnMut(&str, u32)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
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
        // Запись обновляется, пока окно живо; старые отчёты не учитываются.
        let fresh = v["updated"].as_i64().is_some_and(|u| now - u < 60_000);
        if let (true, Some(id)) = (fresh, v["sessionID"].as_str()) {
            f(id, pid);
        }
    }
}

/// Запоминает модель как последнюю выбранную в окне opencode: при `model = "auto"`
/// агент берёт её для следующих сообщений, и без этого выбор из панели откатился бы
/// на модель из окна.
fn remember_model(provider: &str, model: &str) {
    let path = state_file("model.json");
    let mut v = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({ "recent": [] }));
    insert_recent(&mut v, provider, model);
    if let Err(e) = write_json(&path, &v) {
        crate::util::log(&format!("opencode: не удалось запомнить модель {provider}/{model}: {e}"));
    }
}

/// Ставит модель первой в `recent` и убирает прежние вхождения. Остальные поля
/// файла opencode (избранное, варианты) не трогаем.
fn insert_recent(v: &mut Value, provider: &str, model: &str) {
    let mut recent: Vec<Value> = v["recent"].as_array().cloned().unwrap_or_default();
    recent.retain(|m| !(m["providerID"].as_str() == Some(provider) && m["modelID"].as_str() == Some(model)));
    recent.insert(0, json!({ "providerID": provider, "modelID": model }));
    recent.truncate(10);
    v["recent"] = Value::Array(recent);
}

/// Перезапись небольших файлов состояния opencode: сначала во временный файл,
/// иначе окно opencode могло бы прочитать половину.
fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let text = serde_json::to_string(v).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// `Backend` для opencode: агент не разбирает ключ сессии в каждом
/// обработчике, а зовёт эти методы (см. `crate::backend`).
impl Backend for Arc<OpenCode> {
    fn tool(&self) -> &'static str {
        "opencode"
    }

    fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    fn scan(&self) -> Fut<'_, Vec<SessionInfo>> {
        Box::pin(async move { OpenCode::scan(self).await })
    }

    fn limits(&self) -> Fut<'_, Option<Limits>> {
        Box::pin(async move { OpenCode::limits(self).await })
    }

    fn watch(&self, key: String, id: String, out: UnboundedSender<AgentMsg>) -> Fut<'_, ()> {
        Box::pin(async move { OpenCode::watch(self, key, id, out).await })
    }

    fn watch_sub(&self, key: String, id: String, sub: String, out: UnboundedSender<AgentMsg>) -> Fut<'_, ()> {
        Box::pin(async move { OpenCode::watch_sub(self, key, id, sub, out).await })
    }

    fn older<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(Vec<Item>, bool), String>> {
        Box::pin(async move { OpenCode::older(self, id).await })
    }

    fn send<'a>(
        &'a self,
        id: &'a str,
        text: &'a str,
        _out: &'a UnboundedSender<AgentMsg>,
    ) -> Fut<'a, Result<Option<Item>, String>> {
        // opencode держит принятое сообщение в очереди до конца текущего ответа:
        // его элемент возвращается, чтобы панель показала сообщение сразу.
        Box::pin(async move { OpenCode::send(self, id, text).await })
    }

    fn commands<'a>(&'a self, id: &'a str) -> Fut<'a, Vec<Command>> {
        Box::pin(async move { OpenCode::commands(self, id).await })
    }

    fn models<'a>(&'a self, id: &'a str) -> Fut<'a, Vec<ModelInfo>> {
        Box::pin(async move { OpenCode::models(self, id).await })
    }

    fn launch_models<'a>(&'a self, cwd: &'a str) -> Fut<'a, Vec<ModelInfo>> {
        Box::pin(async move { OpenCode::models_at(self, cwd).await })
    }

    fn set_model<'a>(&'a self, id: &'a str, provider: &'a str, model: &'a str) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { OpenCode::set_model(self, id, provider, model).await })
    }

    fn interrupt<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { OpenCode::interrupt(self, id).await })
    }

    fn rename<'a>(&'a self, id: &'a str, title: &'a str) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { OpenCode::rename(self, id, title).await })
    }

    fn delete<'a>(&'a self, id: &'a str, close: bool) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { OpenCode::delete(self, id, close).await })
    }

    fn close<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { OpenCode::close(self, id).await })
    }

    fn start<'a>(&'a self, cwd: &'a str, model: &'a str) -> Fut<'a, Result<Option<String>, String>> {
        Box::pin(async move { OpenCode::start(self, cwd, model).await })
    }

    fn start_with_prompt<'a>(
        &'a self,
        cwd: &'a str,
        prompt: Option<&'a str>,
        model: &'a str,
    ) -> Fut<'a, Result<Option<String>, String>> {
        Box::pin(async move { OpenCode::start_with_prompt(self, cwd, prompt, model).await })
    }

    fn handoff_source<'a>(&'a self, id: &'a str) -> Fut<'a, Result<(String, String), String>> {
        Box::pin(async move { OpenCode::handoff_source(self, id).await })
    }

    #[allow(clippy::too_many_arguments)]
    fn reply_prompt<'a>(
        &'a self,
        id: &'a str,
        prompt: &'a str,
        kind: &'a str,
        decision: &'a str,
        answer: &'a Value,
        message: &'a str,
        cancel: bool,
    ) -> Fut<'a, Result<(), String>> {
        Box::pin(async move {
            OpenCode::reply_prompt(self, id, prompt, kind, decision, answer, message, cancel).await
        })
    }

    fn cancel_queued<'a>(&'a self, id: &'a str, queue: &'a str) -> Fut<'a, Result<(), String>> {
        Box::pin(async move { OpenCode::cancel_queued(self, id, queue).await })
    }

    fn note_optimistic(&self, id: &str, item: &str) {
        OpenCode::note_optimistic(self, id, item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_remembers_models() {
        let v = json!({"data": [
            {"providerID": "openai", "id": "gpt-6", "name": "GPT 6"},
            {"providerID": "opencode-go", "id": "deepseek", "name": "DeepSeek"},
            {"providerID": "broken"},
        ]});
        let got: Vec<_> = models_from(&v).into_iter().map(|m| (m.provider, m.id, m.name)).collect();
        assert_eq!(
            got,
            [
                ("openai".to_owned(), "gpt-6".to_owned(), "GPT 6".to_owned()),
                ("opencode-go".to_owned(), "deepseek".to_owned(), "DeepSeek".to_owned()),
            ]
        );

        let mut doc = json!({
            "recent": [{"providerID": "a", "modelID": "m"}, {"providerID": "p", "modelID": "x"}],
            "favorite": ["z"],
        });
        insert_recent(&mut doc, "p", "x");
        assert_eq!(doc["recent"][0], json!({"providerID": "p", "modelID": "x"}));
        assert_eq!(doc["recent"].as_array().unwrap().len(), 2);
        assert_eq!(doc["favorite"][0], "z");
        insert_recent(&mut doc, "n", "y");
        assert_eq!(doc["recent"][0], json!({"providerID": "n", "modelID": "y"}));
        assert_eq!(doc["recent"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn splits_model() {
        assert_eq!(split_model(""), Ok(None));
        assert_eq!(
            split_model(" opencode-go/deepseek-v4.1-flash "),
            Ok(Some(("opencode-go", "deepseek-v4.1-flash")))
        );
        assert_eq!(split_model("openrouter/deepseek/x"), Ok(Some(("openrouter", "deepseek/x"))));
        assert!(split_model("deepseek").is_err());
        assert!(split_model("/x").is_err());
        assert!(split_model("a/b c").is_err());
    }

    #[test]
    fn formats_session_model() {
        assert_eq!(session_model(&json!({"id":"m","providerID":"p","variant":"default"})), "p/m");
        assert_eq!(session_model(&json!({"id":"m"})), "m");
        assert_eq!(session_model(&Value::Null), "");
    }

    #[test]
    fn location_switch_becomes_cwd_mark() {
        let mut items = Vec::new();
        convert(
            &json!({"id":"m9","type":"location-switched","time":{"created":9},
                "location":{"directory":"C:\\wt\\refactor-split"},
                "previous":{"location":{"directory":"D:\\Projects\\demo"}}}),
            &mut items,
        );
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.role.as_str(), i.kind.as_str(), i.text.as_str())).collect();
        assert_eq!(got, [("m9:0", "system", "cwd", "C:\\wt\\refactor-split")]);
    }

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
                ("m2:reasoning:0", "thinking", "hm"),
                ("m2:1", "tool_use", "ls"),
                ("m2:1:r", "tool_result", "a\nb"),
                ("m2:text:0", "text", "done"),
            ]
        );
    }

    #[test]
    fn tool_keeps_own_time() {
        // У вызова и результата свои отметки: по ним панель считает, сколько
        // шла команда, а не по времени всего сообщения.
        let mut items = Vec::new();
        convert(
            &json!({"id":"m1","type":"assistant","time":{"created":100,"completed":9000},"content":[
                {"type":"tool","name":"shell","state":{"status":"completed","input":{"command":"sleep 3"},
                    "content":[{"type":"text","text":"ok"}],"time":{"created":150,"completed":3150}}}
            ]}),
            &mut items,
        );
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.ts)).collect();
        assert_eq!(got, [("m1:0", 150), ("m1:0:r", 3150)]);
        // Своих отметок нет: результат получает время конца шага, а без него —
        // просто неизвестен (0), и панель время команды не показывает.
        let mut items = Vec::new();
        convert(
            &json!({"id":"m2","type":"assistant","time":{"created":100,"completed":9000},"content":[
                {"type":"tool","name":"shell","state":{"status":"completed","input":{"command":"ls"},"content":"ok"}}
            ]}),
            &mut items,
        );
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.ts)).collect();
        assert_eq!(got, [("m2:0", 100), ("m2:0:r", 9000)]);
    }

    fn step(input: u64, output: u64, reasoning: u64, cache_read: u64) -> Value {
        json!({"type":"assistant","time":{"created":1},
            "tokens":{"input":input,"output":output,"reasoning":reasoning,"cache":{"read":cache_read,"write":0}}})
    }

    #[test]
    fn turn_sums_steps_after_last_idle() {
        // Новые первыми: недописанный шаг, два завершённых, запрос, конец прошлого хода и его шаг.
        let msgs = vec![
            json!({"type":"assistant","time":{"created":9}}),
            step(100, 20, 5, 1000),
            step(90, 10, 0, 800),
            json!({"type":"user","time":{"created":1000},"text":"сделай"}),
            json!({"type":"idle","time":{"created":900},"outcome":"succeeded"}),
            step(5000, 999, 999, 5000),
        ];
        let turn = turn_from_messages(&msgs).usage.unwrap();
        assert_eq!(turn.started_at, 1000);
        // Контекст — по последнему завершённому шагу, выход — по всем шагам хода.
        assert_eq!(turn.input, 1100);
        assert_eq!(turn.output, 35);
    }

    #[test]
    fn turn_cost_counts_page_weight() {
        let oc = OpenCode::new(OpenCodeCfg::default());
        oc.note_turn_cost(1024, 3, false, Duration::from_millis(5));
        oc.note_turn_cost(2048, TURN_PAGE, true, Duration::from_millis(7));
        let c = oc.turn_cost.lock().unwrap();
        assert_eq!((c.calls, c.bytes, c.max_bytes), (2, 3072, 2048));
        assert_eq!((c.msgs, c.max_msgs), (3 + TURN_PAGE as u64, TURN_PAGE));
        assert_eq!((c.full, c.unknown), (1, 1));
        assert_eq!((c.millis, c.max_millis), (12, 7));
    }

    #[test]
    fn last_context_takes_the_newest_answer() {
        let msgs = vec![
            json!({"type":"user","time":{"created":9}}),
            step(100, 20, 5, 1000),
            step(90, 10, 0, 800),
        ];
        assert_eq!(last_context(&msgs), Some(1100));
        // Ответов ещё нет — контекста тоже.
        assert_eq!(last_context(&[json!({"type":"user"})]), None);
    }

    #[test]
    fn turn_keeps_start_when_user_writes_mid_turn() {
        let msgs = vec![
            step(10, 1, 0, 0),
            json!({"type":"user","time":{"created":3000},"text":"ещё"}),
            step(10, 2, 0, 0),
            json!({"type":"user","time":{"created":1000},"text":"раз"}),
            json!({"type":"idle","time":{"created":500}}),
        ];
        let turn = turn_from_messages(&msgs).usage.unwrap();
        assert_eq!(turn.started_at, 1000);
        assert_eq!(turn.output, 3);
    }

    #[test]
    fn turn_unknown_without_prompt_or_beyond_page() {
        // Ход только начался: запрос есть, завершённых шагов нет.
        let fresh = vec![json!({"type":"user","time":{"created":7}}), json!({"type":"idle","time":{"created":1}})];
        let read = turn_from_messages(&fresh);
        let turn = read.usage.unwrap();
        assert_eq!((turn.started_at, turn.input, turn.output), (7, 0, 0));
        assert!(!read.beyond_page);
        // Отметка конца хода есть, а запроса после неё нет.
        assert!(turn_from_messages(&[step(1, 1, 0, 0), json!({"type":"idle","time":{"created":1}})]).usage.is_none());
        // Страница заполнена, и конца прошлого хода в ней не видно.
        let long: Vec<Value> = (0..TURN_PAGE).map(|_| step(1, 1, 0, 0)).collect();
        let read = turn_from_messages(&long);
        assert!(read.usage.is_none());
        assert!(read.beyond_page);
    }

    #[test]
    fn collects_live_window_reports() {
        let dir = std::env::temp_dir().join(format!("aipanel-windows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = crate::util::now_ms();
        let report = |pid: u32, sid: &str, updated: i64| {
            serde_json::json!({"pid": pid, "sessionID": sid, "updated": updated}).to_string()
        };
        std::fs::write(dir.join("live.json"), report(std::process::id(), "ses_live", now)).unwrap();
        // Просроченный отчёт: окно молчит дольше минуты.
        std::fs::write(dir.join("stale.json"), report(std::process::id(), "ses_stale", now - 120_000)).unwrap();
        // Отчёт убитого процесса.
        std::fs::write(dir.join("dead.json"), report(4_294_967_295, "ses_dead", now)).unwrap();
        std::fs::write(dir.join("not-a-report.txt"), "{}").unwrap();

        let mut out = HashSet::new();
        collect_windowed(&dir, now, &mut out);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(out.into_iter().collect::<Vec<_>>(), ["ses_live"]);
    }

    #[test]
    fn marks_accepted_but_not_running_session() {
        // Ход принят в 200, простой был в 100: сессия занята, но не выполняется.
        let accepted = serde_json::json!({"busy": 200, "idle": 100});
        assert_eq!(session_status(false, false, true, &accepted), "stalled");
        assert_eq!(session_status(false, true, true, &accepted), "busy");
        assert_eq!(session_status(true, false, true, &accepted), "waiting");
        // Ход завершён: простой новее занятости.
        let finished = serde_json::json!({"busy": 100, "idle": 200});
        assert_eq!(session_status(false, false, true, &finished), "idle");
        assert_eq!(session_status(false, false, false, &finished), "ready");
    }

    #[test]
    fn reads_pending_request_sessions() {
        // Список запросов opencode: берём сессии, у которых есть неотвеченный
        // запрос; записи без sessionID пропускаются.
        let v = json!({
            "location": {"directory": "D:\\Projects\\demo"},
            "data": [
                {"id": "per_1", "sessionID": "ses_a"},
                {"id": "frm_2", "sessionID": "ses_b"},
                {"id": "per_3"},
            ],
        });
        assert_eq!(prompt_session_ids(&v).collect::<Vec<_>>(), ["ses_a", "ses_b"]);
        // Пустой список и ответ без данных — не ошибка, просто никто не ждёт.
        assert_eq!(prompt_session_ids(&json!({"data": []})).count(), 0);
        assert_eq!(prompt_session_ids(&json!({})).count(), 0);
    }

    #[test]
    fn converts_edits_to_diffs() {
        let mut items = Vec::new();
        convert(
            &json!({"id":"m4","type":"assistant","time":{"created":8},"content":[
                {"type":"tool","name":"edit","state":{"status":"completed",
                    "input":{"path":"C:\\p\\a.rs","oldString":"old","newString":"new"},
                    "metadata":{"files":[{"file":"C:/p/a.rs","patch":"--- a/C:/p/a.rs\n+++ b/C:/p/a.rs\n@@ -1 +1 @@\n-old\n+new\n"}]}}},
                {"type":"tool","name":"write","state":{"status":"completed",
                    "input":{"path":"C:\\p\\b.txt","content":"x\ny"},
                    "metadata":{"truncated":false}}}
            ]}),
            &mut items,
        );
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.kind.as_str(), i.text.as_str())).collect();
        assert_eq!(
            got,
            [
                ("m4:0", "tool_use", "C:\\p\\a.rs"),
                ("m4:0:diff0", "diff", "--- a/C:/p/a.rs\n+++ b/C:/p/a.rs\n@@ -1 +1 @@\n-old\n+new\n"),
                ("m4:1", "tool_use", "C:\\p\\b.txt"),
                ("m4:1:diff", "diff", "--- /dev/null\n+++ b/C:/p/b.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n"),
            ]
        );
    }

    #[test]
    fn streams_reasoning_and_text() {
        let mut feed = Feed::default();
        let ev = |ty: &str, delta: &str| {
            json!({"type": ty, "created": 10, "data": {
                "sessionID": "s", "assistantMessageID": "m", "ordinal": 0, "delta": delta
            }})
        };
        let a = feed.event(&ev("session.reasoning.delta", "раз ")).unwrap();
        assert_eq!(a.id, "m:reasoning:0");
        assert_eq!(a.kind, "thinking");
        assert_eq!(a.text, "раз ");
        let b = feed.event(&ev("session.reasoning.delta", "два")).unwrap();
        assert_eq!(b.text, "раз два");
        // Пустая дельта не меняет текст — повторно не отправляется.
        assert!(feed.event(&ev("session.reasoning.delta", "")).is_none());

        let end = json!({"type":"session.reasoning.ended","created":12,"data":{
            "sessionID":"s","assistantMessageID":"m","ordinal":0,"text":"итог"
        }});
        let c = feed.event(&end).unwrap();
        assert_eq!(c.text, "итог");
        assert!(!feed.reasoning.contains_key(&("m".to_owned(), 0)));

        // Текст — отдельный вид, ordinal считается внутри своего вида.
        let t = feed.event(&ev("session.text.delta", "ответ")).unwrap();
        assert_eq!(t.id, "m:text:0");
        assert_eq!(t.kind, "text");
        assert_eq!(t.text, "ответ");
    }

    #[test]
    fn reads_queue_from_prompt_response() {
        let v = json!({"data":{
            "id":"msg_1","type":"user","delivery":"steer","time":{"created":5},
            "payload":{"text":"привет"}
        }});
        let item = queued_from_prompt(&v, "запасной").unwrap();
        assert_eq!(item.id, "queue:msg_1");
        assert_eq!(item.kind, "queued");
        assert_eq!(item.text, "привет");
        assert_eq!(item.ts, 5);
        assert!(queued_from_prompt(&json!({}), "x").is_none());
    }

    #[test]
    fn converts_queue() {
        let inbox = json!({"data":[
            {"id":"q1","type":"user","time":{"created":5},"payload":{"text":"раз"}},
            {"id":"q2","type":"user","time":{"created":6},"payload":{"text":"два"}},
            {"id":"q3","type":"assistant","time":{"created":7},"payload":{}},
            {"id":"q4","type":"user","time":{"created":8},"payload":{"text":"  "}},
        ]});
        let mut delivered = HashSet::new();
        delivered.insert(text_hash("раз"));
        let items = queued_items(&inbox, &delivered);
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.kind.as_str(), i.text.as_str())).collect();
        assert_eq!(got, [("queue:q2", "queued", "два")]);
    }

    #[test]
    fn converts_permission_request() {
        let p = permission_prompt(&json!({
            "id":"per_1", "sessionID":"s", "action":"read",
            "resources":["C:\\p\\.env"], "save":["*.env"]
        }))
        .unwrap();
        assert_eq!(p.id, "per_1");
        assert_eq!(p.kind, "permission");
        assert_eq!(p.text, "Прочитать файл: C:\\p\\.env");
        assert_eq!(p.save, ["*.env"]);
        assert!(p.fields.is_empty());
        // Несколько ресурсов — списком.
        let p = permission_prompt(&json!({"id":"per_2","action":"bash","resources":["a","b"]})).unwrap();
        assert_eq!(p.text, "Выполнить команду:\n• a\n• b");
        assert!(p.save.is_empty());
    }

    #[test]
    fn converts_form_with_options() {
        let f = form_prompt(&json!({
            "id":"frm_1", "sessionID":"s", "title":"Выберите вариант",
            "fields":[
                {"key":"q0","type":"string","title":"Какой?","required":true,"custom":true,
                 "default":"A","options":[{"value":"A","label":"Вариант A","description":"первый"},
                                          {"value":"B","label":"Вариант B"}]},
                {"key":"q1","type":"multiselect","title":"Несколько","options":[
                    {"value":"x","label":"X"}],"minItems":1},
                {"key":"q2","type":"boolean","title":"Согласны?","default":false},
                {"key":"q3","type":"string","title":"Зависимое",
                 "when":[{"key":"q0","op":"eq","value":"B"}]},
                {"key":"q4","type":"unknown","title":"Неизвестное поле"}
            ]
        }))
        .unwrap();
        assert_eq!(f.kind, "form");
        assert_eq!(f.title, "Выберите вариант");
        let keys: Vec<_> = f.fields.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(keys, ["q0", "q1", "q2", "q3"]);
        let q0 = &f.fields[0];
        assert_eq!(q0.kind, "string");
        assert!(q0.required && q0.custom);
        assert_eq!(q0.default, Some(json!("A")));
        assert_eq!(q0.options[0].label, "Вариант A");
        assert_eq!(q0.options[0].description, "первый");
        assert_eq!(f.fields[1].min_items, Some(1));
        assert_eq!(f.fields[2].default, Some(json!(false)));
        assert_eq!(f.fields[3].when[0].key, "q0");
        assert_eq!(f.fields[3].when[0].op, "eq");
    }

    #[test]
    fn ignores_empty_forms() {
        let f = form_prompt(&json!({
            "id":"frm_2", "fields":[{"key":"q0","type":"unknown"}]
        }));
        assert!(f.is_none());
        assert!(form_prompt(&json!({"id":"frm_3","fields":[]})).is_none());
    }

    #[test]
    fn pages_older_messages() {
        // Страница приходит новыми сообщениями вперёд, а в ленту идёт обратный
        // порядок. Неполная страница — последняя: дальше ранних нет.
        let v = json!({
            "data": [
                {"id":"m3","type":"user","time":{"created":30},"text":"третий"},
                {"id":"m2","type":"assistant","time":{"created":20},"content":[{"type":"text","text":"ответ"}]},
                {"id":"m1","type":"user","time":{"created":10},"text":"первый"},
            ],
            "cursor": {"next": "cur", "previous": null},
        });
        let (items, more) = older_page(&v);
        let got: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.text.as_str())).collect();
        assert_eq!(got, [("m1:0", "первый"), ("m2:text:0", "ответ"), ("m3:0", "третий")]);
        assert!(!more, "страница короче предела — ранних больше нет");

        // Полная страница: opencode мог отдать ровно столько, сколько просили.
        let full: Vec<Value> = (0..PAGE)
            .map(|i| json!({"id":format!("m{i}"),"type":"user","time":{"created":i},"text":"x"}))
            .collect();
        assert!(older_page(&json!({"data": full, "cursor": {"next": "cur"}})).1);

        // Курсор есть, а страница пустая: это конец переписки.
        assert!(!older_page(&json!({"data": [], "cursor": {"next": "cur"}})).1);
        // Старый opencode курсоров не знает — догрузку не предлагаем.
        assert!(!older_page(&json!({"data": []})).1);
    }

}

