use std::path::PathBuf;

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
    pub claude: ClaudeCfg,
    pub opencode: OpenCodeCfg,
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

impl Default for Config {
    fn default() -> Self {
        Self {
            server: String::new(),
            token: String::new(),
            name: std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "agent".into()),
            terminal_profile: String::new(),
            claude: ClaudeCfg::default(),
            opencode: OpenCodeCfg::default(),
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
        Ok(cfg)
    }
}
