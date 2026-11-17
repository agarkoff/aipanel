//! Git-репозиторий каталога сессии: имя, адрес `origin`, ветка и признак worktree.
//!
//! Читаются только файлы `.git` (без запуска `git`): опрос сессий идёт раз в
//! пару секунд, а процесс на каждую сессию был бы заметен. Ответ кешируется на
//! [`TTL`]: ветка может смениться, но не настолько часто.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use aipanel_proto::RepoInfo;

const TTL: Duration = Duration::from_secs(15);

static CACHE: Mutex<Option<HashMap<String, (Instant, Option<RepoInfo>)>>> = Mutex::new(None);

/// Репозиторий, в котором лежит `cwd`; `None` — обычный каталог (или его нет).
pub fn info(cwd: &str) -> Option<RepoInfo> {
    if cwd.trim().is_empty() {
        return None;
    }
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some((at, v)) = cache.get(cwd) {
        if at.elapsed() < TTL {
            return v.clone();
        }
    }
    let v = detect(Path::new(cwd));
    cache.retain(|_, (at, _)| at.elapsed() < TTL);
    cache.insert(cwd.to_owned(), (Instant::now(), v.clone()));
    v
}

fn detect(cwd: &Path) -> Option<RepoInfo> {
    let (work, git) = find_git(cwd)?;
    // Свой каталог git: у основной копии это `.git`, у worktree — `.git/worktrees/<имя>`.
    let gitdir = if git.is_dir() { git.clone() } else { read_gitdir(&git, &work)? };
    let common = match fs::read_to_string(gitdir.join("commondir")) {
        Ok(s) => resolve(&gitdir, s.trim()),
        Err(_) => gitdir.clone(),
    };
    let worktree = !same_path(&gitdir, &common);
    // Основная копия — каталог над общим `.git`; у голого репозитория — сам он.
    let root = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().map(Path::to_path_buf).unwrap_or_else(|| common.clone())
    } else if worktree {
        common.clone()
    } else {
        work.clone()
    };
    let url = fs::read_to_string(common.join("config")).ok().and_then(|c| remote_url(&c)).map(|u| short_url(&u)).unwrap_or_default();
    let name = match url.rsplit('/').next().filter(|s| !s.is_empty()) {
        Some(last) => last.to_owned(),
        None => root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    let branch = fs::read_to_string(gitdir.join("HEAD")).map(|h| head_branch(&h)).unwrap_or_default();
    Some(RepoInfo { name, url, branch, worktree, root: display(&root) })
}

/// Ближайший вверх каталог с `.git` (каталогом или файлом-ссылкой worktree).
///
/// Домашний каталог пропускается, если сессия не в нём самом: бывает, что он
/// целиком под git (настройки), и тогда любая папка профиля — `Downloads`,
/// `Documents` — выглядела бы как репозиторий с именем пользователя.
fn find_git(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    let home = crate::util::home_dir();
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let git = d.join(".git");
        if git.exists() && (d == cwd || !same_path(d, &home)) {
            return Some((d.to_path_buf(), git));
        }
        dir = d.parent();
    }
    None
}

/// Файл `.git` worktree: `gitdir: <путь>`, путь может быть относительным.
fn read_gitdir(file: &Path, work: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(file).ok()?;
    let rest = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    Some(resolve(work, rest))
}

fn resolve(base: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    let joined = if path.is_absolute() { path } else { base.join(path) };
    // Канонизация убирает `..` из относительных ссылок; `\\?\` в начале не нужен.
    fs::canonicalize(&joined).map(|c| strip_verbatim(&c)).unwrap_or(joined)
}

fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase();
    norm(a) == norm(b)
}

/// Путь в привычном для машины виде: на Windows — с обратными слешами.
fn display(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if cfg!(windows) {
        s.replace('/', "\\")
    } else {
        s
    }
}

/// Адрес `origin` из `.git/config`, а без него — первого удалённого.
pub(crate) fn remote_url(config: &str) -> Option<String> {
    let mut section: Option<String> = None;
    let mut first: Option<String> = None;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line
                .strip_prefix("[remote \"")
                .and_then(|r| r.strip_suffix("\"]"))
                .map(str::to_owned);
            continue;
        }
        let Some(name) = &section else { continue };
        let Some((key, value)) = line.split_once('=') else { continue };
        if key.trim() != "url" {
            continue;
        }
        let value = value.trim().trim_matches('"').to_owned();
        if name == "origin" {
            return Some(value);
        }
        first.get_or_insert(value);
    }
    first
}

/// `https://user:token@host/owner/repo.git` и `git@host:owner/repo.git` →
/// `host/owner/repo`. Логин и токен в панель не уходят.
pub(crate) fn short_url(url: &str) -> String {
    let mut s = url.trim();
    if let Some((_, rest)) = s.split_once("://") {
        s = rest;
    }
    let s = match s.split_once('@') {
        // `@` до первого `/` — учётные данные или пользователь ssh.
        Some((creds, rest)) if !creds.contains('/') => rest,
        _ => s,
    };
    let mut out = s.to_owned();
    // scp-вид `host:owner/repo`: двоеточие до первого слеша (но не порт `host:22/`).
    if let Some(colon) = out.find(':') {
        let slash = out.find('/').unwrap_or(out.len());
        let port = out[colon + 1..slash].chars().all(|c| c.is_ascii_digit()) && slash > colon + 1;
        // `D:\repos\x.git` — локальный путь с буквой диска, а не scp-вид.
        let drive = colon == 1;
        if colon < slash && !port && !drive && !url.contains("://") {
            out.replace_range(colon..=colon, "/");
        }
    }
    let out = out.replace('\\', "/");
    let out = out.trim_end_matches('/');
    out.strip_suffix(".git").unwrap_or(out).to_owned()
}

/// Ветка из `HEAD`: `ref: refs/heads/<ветка>`; отсоединённый HEAD — первые 7 знаков хеша.
pub(crate) fn head_branch(head: &str) -> String {
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(r) => {
            let r = r.trim();
            r.strip_prefix("refs/heads/").unwrap_or(r).to_owned()
        }
        None => head.chars().take(7).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_url_drops_scheme_credentials_and_suffix() {
        assert_eq!(short_url("https://git.example.ru/root/ai-panel.git"), "git.example.ru/root/ai-panel");
        assert_eq!(short_url("https://user:secret@host.ru/a/b.git"), "host.ru/a/b");
        assert_eq!(short_url("git@github.com:owner/repo.git"), "github.com/owner/repo");
        assert_eq!(short_url("ssh://git@host:2222/owner/repo.git"), "host:2222/owner/repo");
        assert_eq!(short_url("D:\\repos\\thing.git"), "D:/repos/thing");
    }

    #[test]
    fn remote_prefers_origin() {
        let cfg = "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://a/up.git\n[remote \"origin\"]\n\turl = https://a/me.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n";
        assert_eq!(remote_url(cfg).as_deref(), Some("https://a/me.git"));
        assert_eq!(remote_url("[remote \"up\"]\nurl = x\n").as_deref(), Some("x"));
        assert_eq!(remote_url("[core]\n"), None);
    }

    #[test]
    fn head_branch_and_detached() {
        assert_eq!(head_branch("ref: refs/heads/refactor-master\n"), "refactor-master");
        assert_eq!(head_branch("9f442cd0123456789\n"), "9f442cd");
    }

    #[test]
    fn detects_main_copy_and_worktree() {
        let tmp = std::env::temp_dir().join(format!("aipanel-repo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let main = tmp.join("proj");
        let git = main.join(".git");
        fs::create_dir_all(git.join("worktrees").join("wt1")).unwrap();
        fs::write(git.join("HEAD"), "ref: refs/heads/master\n").unwrap();
        fs::write(git.join("config"), "[remote \"origin\"]\n\turl = https://u:p@host/o/proj.git\n").unwrap();
        fs::create_dir_all(main.join("sub")).unwrap();

        let r = detect(&main.join("sub")).unwrap();
        assert_eq!((r.name.as_str(), r.url.as_str(), r.branch.as_str(), r.worktree), ("proj", "host/o/proj", "master", false));

        let wt = tmp.join("elsewhere").join("wt1");
        fs::create_dir_all(&wt).unwrap();
        let wgit = git.join("worktrees").join("wt1");
        fs::write(wt.join(".git"), format!("gitdir: {}\n", wgit.display())).unwrap();
        fs::write(wgit.join("commondir"), "../..\n").unwrap();
        fs::write(wgit.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
        let r = detect(&wt).unwrap();
        assert_eq!((r.name.as_str(), r.branch.as_str(), r.worktree), ("proj", "feature", true));
        assert!(same_path(Path::new(&r.root), &fs::canonicalize(&main).map(|p| strip_verbatim(&p)).unwrap()));

        let plain = tmp.join("plain");
        fs::create_dir_all(&plain).unwrap();
        // Временный каталог может лежать внутри чужого репозитория — тогда проверять нечего.
        if find_git(&plain).is_none() {
            assert!(detect(&plain).is_none());
        }
        let _ = fs::remove_dir_all(&tmp);
    }
}
