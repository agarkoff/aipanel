//! Чтение картинки по пути из ленты сессии: панель показывает её по клику.
//!
//! Отдаются только файлы картинок: по расширению и по содержимому (сигнатуре
//! формата). Иначе через панель можно было бы прочитать любой файл машины.
//!
//! Путь из ответа модели не всегда отсчитан от каталога сессии: скриншот может
//! лежать во временном каталоге, файл — в подкаталоге проекта. Поэтому
//! относительный путь ищется по всему дереву каталога сессии (и корня
//! репозитория), а берётся только единственное совпадение: при двух кандидатах
//! лучше ошибка, чем чужая картинка.

use std::path::{Path, PathBuf};

use aipanel_proto::MAX_IMAGE_BYTES;

const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "ico", "svg"];

/// Каталоги, в которые поиск по дереву не заходит: там картинок из ленты нет.
const SKIP_DIRS: &[&str] =
    &[".git", "node_modules", "target", ".venv", "venv", "__pycache__", ".gradle", ".idea"];

const MAX_SEARCH_DEPTH: usize = 8;
const MAX_SEARCH_ENTRIES: usize = 100_000;

/// Читает картинку `raw`; относительный путь ищется от каталога сессии `cwd`,
/// корня репозитория, временного и домашнего каталога. Возвращает MIME-тип и
/// байты файла.
pub fn read(cwd: Option<&str>, raw: &str) -> Result<(&'static str, Vec<u8>), String> {
    let path = resolve(cwd, raw)?;
    read_file(&path)
}

fn read_file(path: &Path) -> Result<(&'static str, Vec<u8>), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !EXTENSIONS.contains(&ext.as_str()) {
        return Err("это не файл картинки".into());
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("файл {} недоступен: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} — не файл", path.display()));
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(format!("картинка больше {} МБ", MAX_IMAGE_BYTES >> 20));
    }
    let data = std::fs::read(path).map_err(|e| format!("не удалось прочитать {}: {e}", path.display()))?;
    let mime = sniff(&data, &ext).ok_or("содержимое файла не похоже на картинку")?;
    Ok((mime, data))
}

/// Путь из текста ответа модели в путь на этой машине.
fn resolve(cwd: Option<&str>, raw: &str) -> Result<PathBuf, String> {
    resolve_with(cwd, raw, temp_dir(), home_dir())
}

fn resolve_with(
    cwd: Option<&str>,
    raw: &str,
    temp: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err("некорректный путь".into());
    }
    let raw = raw.strip_prefix("file:///").or_else(|| raw.strip_prefix("file://")).unwrap_or(raw);
    let raw = git_bash_drive(raw).unwrap_or_else(|| raw.to_owned());
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        let home = home.ok_or("не найден домашний каталог")?;
        return Ok(home.join(rest));
    }
    let path = PathBuf::from(&raw);
    if path.is_absolute() {
        return Ok(path);
    }

    // `./a/b.png` — обычный путь; `../a.png` тоже годится от каталога сессии,
    // но по хвосту его искать нельзя: он ведёт в другую ветку.
    let rel = Path::new(&raw);
    let has_parent = raw.split(['/', '\\']).any(|c| c == "..");
    // Деревья, в которых ищем по хвосту пути: каталог сессии и корень репозитория.
    // Временный и домашний каталоги проверяются только точным путём: рекурсивный
    // поиск по ним нашёл бы одноимённые файлы совсем не из этой задачи.
    let mut trees: Vec<PathBuf> = Vec::new();
    let mut exact: Vec<PathBuf> = Vec::new();
    if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
        trees.push(PathBuf::from(cwd));
    }
    let git = cwd.filter(|c| !c.is_empty()).and_then(|c| git_root(Path::new(c)));
    if let Some(git) = git {
        if Some(git.as_path()) != cwd.map(Path::new) {
            trees.push(git);
        }
    }
    if let Some(temp) = temp {
        exact.push(temp);
    }
    if let Some(home) = home {
        exact.push(home);
    }

    // Ищем по всему поддереву каталога сессии, включая сам каталог: файл может
    // лежать на любом уровне (`docs/social-pdf/x.png`, `screen.png` в `docs/`).
    // Совпадение принимается, только если оно одно: при двух кандидатах лучше
    // ошибка, чем чужая картинка.
    if !has_parent {
        if let Some(tail) = clean_relative(&raw) {
            for root in &trees {
                match search_by_tail(root, &tail) {
                    Found::One(path) => return Ok(path),
                    Found::Many => {
                        return Err(format!(
                            "«{raw}»: несколько файлов с таким концом пути — укажите каталог"
                        ))
                    }
                    Found::None => {}
                }
            }
        }
    } else {
        for base in &trees {
            let candidate = base.join(rel);
            if candidate.is_file() {
                return Ok(normalize(candidate));
            }
        }
    }

    for base in &exact {
        let candidate = base.join(rel);
        if candidate.is_file() {
            return Ok(normalize(candidate));
        }
    }

    let tried = trees
        .iter()
        .chain(exact.iter())
        .map(|b| b.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "файл {raw} не найден; искали в каталоге сессии, корне репозитория и временном каталоге ({tried})"
    ))
}

/// Путь без `.` и пустых компонентов; `None` — если в нём есть `..`.
fn clean_relative(raw: &str) -> Option<String> {
    if raw.split(['/', '\\']).any(|c| c == "..") {
        return None;
    }
    let normalized = raw.replace('\\', "/");
    let parts: Vec<&str> = normalized
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

enum Found {
    One(PathBuf),
    Many,
    None,
}

/// Лексическая нормализация: убирает `.` и раскрывает `..` в уже склеенном пути.
fn normalize(path: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Ищет в дереве `root` файл, путь которого кончается на `rel`. Совпадение
/// принимается, только если оно единственное.
fn search_by_tail(root: &Path, rel: &str) -> Found {
    let want: Vec<String> = rel.split('/').map(component).collect();
    if want.is_empty() || !root.is_dir() {
        return Found::None;
    }
    let mut out: Option<PathBuf> = None;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_SEARCH_ENTRIES {
                return Found::None;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() {
                if depth < MAX_SEARCH_DEPTH && !skip_dir(&entry.file_name()) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !kind.is_file() || !has_image_extension(&path) {
                continue;
            }
            if !tail_matches(&path, &want) {
                continue;
            }
            match &out {
                Some(_) => return Found::Many,
                None => out = Some(path),
            }
        }
    }
    match out {
        Some(p) => Found::One(p),
        None => Found::None,
    }
}

fn tail_matches(path: &Path, want: &[String]) -> bool {
    let parts: Vec<String> = path.components().map(|c| component(&c.as_os_str().to_string_lossy())).collect();
    if parts.len() < want.len() {
        return false;
    }
    parts[parts.len() - want.len()..] == *want
}

fn component(s: &str) -> String {
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_owned()
    }
}

fn skip_dir(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    SKIP_DIRS.iter().any(|d| name == *d)
}

fn has_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().find(|p| p.join(".git").exists()).map(Path::to_path_buf)
}

fn temp_dir() -> Option<PathBuf> {
    for key in ["TEMP", "TMP", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            let path = PathBuf::from(value);
            if path.is_dir() {
                return Some(path);
            }
        }
    }
    None
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
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
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, data).unwrap();
        p
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aipanel-image-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
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

    fn png(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"\x89PNG\r\n\x1a\n rest").unwrap();
    }

    #[test]
    fn finds_file_in_temp_when_cwd_is_project() {
        let base = dir("temp-root");
        let temp = base.join("Temp");
        png(&temp.join("aipanel-sessions.png"));
        let cwd = base.join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let p = resolve_with(cwd.to_str(), "aipanel-sessions.png", Some(temp.clone()), None).unwrap();
        assert_eq!(p, temp.join("aipanel-sessions.png"));
    }

    #[test]
    fn finds_file_from_repository_root_when_cwd_is_subdir() {
        let base = dir("git-root");
        std::fs::create_dir_all(base.join(".git")).unwrap();
        png(&base.join("docs").join("shot.png"));
        let cwd = base.join("crate");
        std::fs::create_dir_all(&cwd).unwrap();
        let p = resolve_with(cwd.to_str(), "docs/shot.png", None, None).unwrap();
        assert_eq!(p, base.join("docs").join("shot.png"));
    }

    #[test]
    fn finds_unique_file_by_tail_when_only_name_is_known() {
        let base = dir("by-tail");
        std::fs::create_dir_all(base.join(".git")).unwrap();
        png(&base.join("docs").join("screen-feed.png"));
        let p = resolve_with(base.to_str(), "screen-feed.png", None, None).unwrap();
        assert_eq!(p, base.join("docs").join("screen-feed.png"));
    }

    #[test]
    fn refuses_ambiguous_tail() {
        let base = dir("ambiguous");
        std::fs::create_dir_all(base.join(".git")).unwrap();
        png(&base.join("a").join("shot.png"));
        png(&base.join("b").join("shot.png"));
        let err = resolve_with(base.to_str(), "shot.png", None, None).unwrap_err();
        assert!(err.contains("несколько файлов"), "{err}");
    }

    #[test]
    fn refuses_when_exact_file_has_twin_in_subdirectory() {
        let base = dir("twin");
        std::fs::create_dir_all(base.join(".git")).unwrap();
        png(&base.join("shot.png"));
        png(&base.join("docs").join("shot.png"));
        let err = resolve_with(base.to_str(), "shot.png", None, None).unwrap_err();
        assert!(err.contains("несколько файлов"), "точное совпадение не должно скрывать второе: {err}");
    }

    #[test]
    fn finds_exact_path_with_parent_but_does_not_search_tree() {
        let base = dir("dotdot");
        png(&base.join("sibling").join("shot.png"));
        png(&base.join("sibling").join("extra").join("shot.png"));
        let cwd = base.join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let p = resolve_with(cwd.to_str(), "../sibling/shot.png", None, None).unwrap();
        assert_eq!(p, base.join("sibling").join("shot.png"));
    }

    #[test]
    fn does_not_search_parent_directory() {
        let base = dir("no-parent");
        png(&base.join("sibling").join("shot.png"));
        let cwd = base.join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        assert!(resolve_with(cwd.to_str(), "sibling/shot.png", None, None).is_err());
    }

    #[test]
    fn home_path_with_tilde() {
        let base = dir("home");
        png(&base.join("Pictures").join("shot.png"));
        let p = resolve_with(None, "~/Pictures/shot.png", None, Some(base.clone())).unwrap();
        assert_eq!(p, base.join("Pictures").join("shot.png"));
    }
}
