//! Чтение картинки по пути из ленты сессии: панель показывает её по клику.
//!
//! Отдаются только файлы картинок: по расширению и по содержимому (сигнатуре
//! формата). Иначе через панель можно было бы прочитать любой файл машины.

use std::path::{Path, PathBuf};

use aipanel_proto::MAX_IMAGE_BYTES;

const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "ico", "svg"];

/// Читает картинку `raw`; относительный путь считается от каталога сессии `cwd`.
/// Возвращает MIME-тип и байты файла.
pub fn read(cwd: Option<&str>, raw: &str) -> Result<(&'static str, Vec<u8>), String> {
    let path = resolve(cwd, raw)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !EXTENSIONS.contains(&ext.as_str()) {
        return Err("это не файл картинки".into());
    }
    let meta = std::fs::metadata(&path).map_err(|e| format!("файл {} недоступен: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} — не файл", path.display()));
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(format!("картинка больше {} МБ", MAX_IMAGE_BYTES >> 20));
    }
    let data = std::fs::read(&path).map_err(|e| format!("не удалось прочитать {}: {e}", path.display()))?;
    let mime = sniff(&data, &ext).ok_or("содержимое файла не похоже на картинку")?;
    Ok((mime, data))
}

/// Путь из текста ответа модели в путь на этой машине.
fn resolve(cwd: Option<&str>, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err("некорректный путь".into());
    }
    let raw = raw.strip_prefix("file:///").or_else(|| raw.strip_prefix("file://")).unwrap_or(raw);
    let raw = git_bash_drive(raw).unwrap_or_else(|| raw.to_owned());
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .ok_or("не найден домашний каталог")?;
        return Ok(PathBuf::from(home).join(rest));
    }
    let path = PathBuf::from(&raw);
    if path.is_absolute() {
        return Ok(path);
    }
    match cwd.filter(|c| !c.is_empty()) {
        Some(cwd) => Ok(Path::new(cwd).join(path)),
        None => Err("путь относительный, а каталог сессии неизвестен".into()),
    }
}

/// `/d/Projects/x.png` (так пути пишет Git Bash) — это `D:/Projects/x.png` на Windows.
fn git_bash_drive(raw: &str) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let b = raw.as_bytes();
    (b.len() > 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/')
        .then(|| format!("{}:/{}", (b[1] as char).to_ascii_uppercase(), &raw[3..]))
}

/// Тип картинки по сигнатуре в начале файла. SVG — текст, его узнаём по тегу.
fn sniff(data: &[u8], ext: &str) -> Option<&'static str> {
    let at = |off: usize, sig: &[u8]| data.get(off..off + sig.len()) == Some(sig);
    if at(0, b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if at(0, b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if at(0, b"GIF87a") || at(0, b"GIF89a") {
        Some("image/gif")
    } else if at(0, b"RIFF") && at(8, b"WEBP") {
        Some("image/webp")
    } else if at(4, b"ftypavif") || at(4, b"ftypavis") {
        Some("image/avif")
    } else if at(0, b"BM") && ext == "bmp" {
        Some("image/bmp")
    } else if at(0, b"\0\0\x01\0") {
        Some("image/x-icon")
    } else if ext == "svg" {
        let head = String::from_utf8_lossy(&data[..data.len().min(4096)]).to_ascii_lowercase();
        head.contains("<svg").then_some("image/svg+xml")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, data: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aipanel-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn reads_png_by_absolute_and_relative_path() {
        let p = tmp("shot.png", b"\x89PNG\r\n\x1a\n rest");
        let (mime, data) = read(None, p.to_str().unwrap()).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(data.len(), 13);
        let dir = p.parent().unwrap().to_str().unwrap();
        assert_eq!(read(Some(dir), "shot.png").unwrap().0, "image/png");
        assert_eq!(read(Some(dir), "./shot.png").unwrap().0, "image/png");
    }

    #[test]
    fn refuses_non_images() {
        let txt = tmp("secret.txt", b"password");
        assert!(read(None, txt.to_str().unwrap()).is_err(), "не картинка по расширению");
        let fake = tmp("fake.png", b"password");
        assert!(read(None, fake.to_str().unwrap()).is_err(), "не картинка по содержимому");
        assert!(read(None, "relative.png").is_err(), "относительный путь без каталога");
    }

    #[test]
    fn svg_needs_svg_tag() {
        let ok = tmp("pic.svg", b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>");
        assert_eq!(read(None, ok.to_str().unwrap()).unwrap().0, "image/svg+xml");
        let bad = tmp("bad.svg", b"just text");
        assert!(read(None, bad.to_str().unwrap()).is_err());
    }

    #[test]
    fn git_bash_paths() {
        if cfg!(windows) {
            assert_eq!(git_bash_drive("/d/Projects/x.png").as_deref(), Some("D:/Projects/x.png"));
        }
        assert_eq!(git_bash_drive("/tmp/x.png").filter(|_| cfg!(windows)), None);
    }
}
