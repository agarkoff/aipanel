//! Установка плагина AiPanel в TUI opencode.
//!
//! opencode 2.x сам подхватывает плагины окна из каталогов
//! `<конфигурация>/plugins/<имя>/tui.js`, поэтому достаточно скопировать файл.

use std::path::PathBuf;

const PLUGIN_JS: &str = include_str!("../../../plugins/opencode/aipanel-tui-session.js");
const PLUGIN_DIR: &str = "aipanel-tui";

fn plugin_dir() -> PathBuf {
    std::env::var_os("OPENCODE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| crate::util::home_dir().join(".config"))
                .join("opencode")
        })
        .join("plugins")
        .join(PLUGIN_DIR)
}

pub fn install() -> Result<String, String> {
    let dir = plugin_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join("tui.js");
    std::fs::write(&file, PLUGIN_JS).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(format!(
        "Плагин установлен: {}\nПерезапустите открытые окна opencode, чтобы плагин заработал.",
        file.display()
    ))
}

pub fn uninstall() -> Result<String, String> {
    let dir = plugin_dir();
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok("Плагин удалён.".into())
}
