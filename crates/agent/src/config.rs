use std::{collections::BTreeMap, path::PathBuf};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Адрес панели, например `wss://panel.example.com/agent`.
    pub server: String,
    /// Токен пользователя из PANEL_USERS на сервере; по нему сервер определяет владельца агента.
    pub token: String,
    /// Имя машины в панели. По умолчанию — имя хоста.
    pub name: String,
    /// Профиль Windows Terminal (имя или guid) для окон, которые открывает агент.
    /// Пусто — профиль по умолчанию из настроек Windows Terminal.
    pub terminal_profile: String,
    /// Куда открывать окно сессии: `tab` — вкладкой в одном окне агента Windows Terminal,
    /// `window` — отдельным окном на каждую сессию.
    pub terminal_window: String,
    pub claude: ClaudeCfg,
    pub opencode: OpenCodeCfg,
    pub voice: VoiceCfg,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeCfg {
    pub enabled: bool,
    /// Исполняемый файл Claude Code для фонового запуска.
    pub bin: String,
    /// Каталог данных Claude Code. По умолчанию `~/.claude`.
    pub home: Option<PathBuf>,
    /// Показывать закрытые сессии, изменённые за последние N дней.
    pub recent_days: u64,
    /// Не больше стольких закрытых сессий.
    pub max_recent: usize,
    /// Дополнительные аргументы при запуске закрытой сессии (`claude --resume <id> ...`).
    pub resume_args: Vec<String>,
    /// Отмечать каталог доверенным в `~/.claude.json` перед открытием нового окна.
    /// Иначе Claude Code спрашивает «Is this a project you created or one you trust?»
    /// и до ответа не заводит сессию — из панели, особенно с телефона, ответить нечем.
    pub trust_dirs: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenCodeCfg {
    pub enabled: bool,
    /// Исполняемый файл opencode для открытия окна. Если его нет в PATH,
    /// ищется `~/.opencode/bin/opencode.exe`.
    pub bin: String,
    /// Модель для сообщений из панели: `auto` — последняя выбранная в окне opencode
    /// (та, что видна в строке состояния), `session` — не менять модель сессии,
    /// либо явно `провайдер/модель`.
    pub model: String,
    pub max_sessions: usize,
    /// Адрес сервиса opencode. По умолчанию читается из его файла состояния.
    pub url: Option<String>,
    pub password: Option<String>,
}

/// Голосовой ввод: запись из панели распознаётся на этой машине (GigaAM v3).
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VoiceCfg {
    pub enabled: bool,
    /// Голова модели: `e2e_rnnt` — с пунктуацией, заглавными и числами,
    /// `rnnt` — без них, но немного точнее на коротких фразах.
    pub model: String,
    /// Слова, к которым декодер смещается: латиница, имена программ и команд.
    /// Модель обучена на русском и без подсказки пишет `cargo` как «карго».
    pub terms: Vec<String>,
    /// Замены в готовом тексте: что модель пишет и что должно стоять вместо
    /// (`[voice.fix]` с парами `"карго" = "cargo"`). Регистр не важен.
    pub fix: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: String::new(),
            token: String::new(),
            name: std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "agent".into()),
            terminal_profile: String::new(),
            terminal_window: "tab".into(),
            claude: ClaudeCfg::default(),
            opencode: OpenCodeCfg::default(),
            voice: VoiceCfg::default(),
        }
    }
}

impl Default for ClaudeCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            bin: "claude".into(),
            home: None,
            recent_days: 7,
            max_recent: 30,
            resume_args: Vec::new(),
            trust_dirs: false,
        }
    }
}

impl Default for OpenCodeCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            bin: "opencode".into(),
            model: "auto".into(),
            max_sessions: 30,
            url: None,
            password: None,
        }
    }
}

impl Default for VoiceCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "e2e_rnnt".into(),
            // Латиница и команды, с которыми чаще всего работают в панели:
            // декодер смещается к ним, а не пишет их по-русски.
            terms: [
                "cargo", "clippy", "rust", "git", "GitHub", "GitLab", "commit", "branch", "merge", "push",
                "worktree", "MR", "API", "WebSocket", "opencode", "Claude Code",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            // Русские написания, которые модель выдаёт вместо английских слов:
            // подсказки декодера их не исправляют, а замена в готовом тексте —
            // исправляет. Список пополняется по своим ошибкам (`[voice.fix]`).
            fix: [
                ("карго", "cargo"),
                ("клиппи", "clippy"),
                ("мердж реквест", "merge request"),
                ("мегреквест", "merge request"),
                ("пул реквест", "pull request"),
                ("ворктри", "worktree"),
                ("гитлаб", "GitLab"),
                ("гитхаб", "GitHub"),
                ("опенкод", "opencode"),
            ]
            .iter()
            .map(|(from, to)| ((*from).to_string(), (*to).to_string()))
            .collect(),
        }
    }
}

impl Config {
    /// Читает `--config <путь>`, иначе `agent.toml` рядом с программой или в текущем
    /// каталоге. Переменные AIPANEL_SERVER, AIPANEL_TOKEN, AIPANEL_NAME имеют приоритет.
    pub fn load(explicit: Option<PathBuf>) -> Result<Self, String> {
        let path = explicit.clone().or_else(|| {
            // Рядом с программой, в текущей папке, затем в каталоге данных (~/.aipanel):
            // последний вариант нужен, когда exe лежит в общей папке из PATH.
            let beside = std::env::current_exe().ok()?.parent()?.join("agent.toml");
            [beside, PathBuf::from("agent.toml"), crate::util::aipanel_dir().join("agent.toml")]
                .into_iter()
                .find(|p| p.is_file())
        });
        let mut cfg = match path {
            Some(p) => {
                let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                toml::from_str::<Config>(&text).map_err(|e| format!("{}: {e}", p.display()))?
            }
            None => Config::default(),
        };
        for (var, field) in [
            ("AIPANEL_SERVER", &mut cfg.server),
            ("AIPANEL_TOKEN", &mut cfg.token),
            ("AIPANEL_NAME", &mut cfg.name),
        ] {
            if let Ok(v) = std::env::var(var) {
                if !v.is_empty() {
                    *field = v;
                }
            }
        }
        if cfg.server.is_empty() || cfg.token.is_empty() {
            return Err("server and token are required (agent.toml or AIPANEL_SERVER / AIPANEL_TOKEN)".into());
        }
        if !(cfg.server.starts_with("ws://") || cfg.server.starts_with("wss://")) {
            return Err("server must start with ws:// or wss://".into());
        }
        cfg.terminal_window = terminal_window(&cfg.terminal_window)?.to_owned();
        Ok(cfg)
    }

    /// Открывать каждую сессию отдельным окном, а не вкладкой в общем окне агента.
    pub fn terminal_new_window(&self) -> bool {
        self.terminal_window == "window"
    }
}

/// Приводит `terminal_window` к одному из двух значений; опечатка в конфиге
/// должна быть видна сразу, а не превращаться молча в значение по умолчанию.
fn terminal_window(value: &str) -> Result<&'static str, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "tab" => Ok("tab"),
        "window" | "new" => Ok("window"),
        other => Err(format!("terminal_window: ожидается \"tab\" или \"window\", получено \"{other}\"")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_window_values() {
        assert_eq!(terminal_window("").unwrap(), "tab");
        assert_eq!(terminal_window(" Tab ").unwrap(), "tab");
        assert_eq!(terminal_window("window").unwrap(), "window");
        assert_eq!(terminal_window("NEW").unwrap(), "window");
        assert!(terminal_window("tabs").is_err());
    }

    #[test]
    fn defaults_to_one_window_with_tabs() {
        let cfg = Config::default();
        assert_eq!(cfg.terminal_window, "tab");
        assert!(!cfg.terminal_new_window());
    }
}
