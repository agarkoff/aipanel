//! Дерево каталогов для окна «Новая сессия»: корни (диски) и содержимое папок
//! машины агента. Список строится на той машине, где живёт агент, поэтому
//! на Windows показываются диски, а на Linux и macOS — файловая система.

use std::path::Path;

use aipanel_proto::DirEntry;

/// Предел числа папок в одном каталоге: дерево должно остаться отзывчивым,
/// а сообщение — поместиться в WebSocket.
pub const MAX_ENTRIES: usize = 2000;

/// Корни дерева: диски на Windows, файловая система на остальных платформах.
pub fn roots() -> Vec<DirEntry> {
    platform_roots()
}

/// Содержимое каталога `path`. Пустой путь — корни дерева.
pub fn list(path: &str) -> Result<Vec<DirEntry>, String> {
    if path.is_empty() {
        return Ok(roots());
    }
    let dir = Path::new(path);
    if !dir.is_absolute() {
        return Err(format!("нужен абсолютный путь: {path}"));
    }
    let read = std::fs::read_dir(dir).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("каталог не найден: {path}"),
        std::io::ErrorKind::PermissionDenied => format!("нет доступа: {path}"),
        _ => format!("не удалось прочитать каталог {path}: {e}"),
    })?;

    let mut entries = Vec::new();
    for entry in read.flatten() {
        if entries.len() >= MAX_ENTRIES {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // Только каталоги: файл в этом дереве выбрать нельзя. `metadata` идёт
        // по ссылке, поэтому каталог-ярлык тоже попадёт в список.
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() || is_hidden(&name, &meta) {
            continue;
        }
        entries.push(DirEntry { name, path: entry.path().to_string_lossy().into_owned() });
    }
    sort(&mut entries);
    Ok(entries)
}

/// Скрытые каталоги не показываем — как проводник с настройками по умолчанию:
/// на Windows это атрибуты hidden/system (`$Recycle.Bin`, `AppData`), на
/// Linux и macOS — имена с точки. Иначе корень диска утопает в служебных папках.
#[cfg(windows)]
fn is_hidden(name: &str, meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    // Служебные папки самой Windows ($Recycle.Bin, $WINDOWS.~BT, $WinREAgent)
    // помечены по-разному: атрибуты не у всех, но имя всегда начинается с `$`.
    name.starts_with('$')
        || meta.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
}

#[cfg(not(windows))]
fn is_hidden(name: &str, _meta: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}

/// Порядок как в проводнике: без учёта регистра и с числами по величине,
/// чтобы «папка2» была раньше «папка10», а не наоборот.
fn sort(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| natural_cmp(&a.name, &b.name).then_with(|| a.name.cmp(&b.name)));
}

fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let left = chunks(a);
    let right = chunks(b);
    for (x, y) in left.iter().zip(right.iter()) {
        let ord = if x.0 && y.0 {
            let xs = x.1.trim_start_matches('0');
            let ys = y.1.trim_start_matches('0');
            xs.len()
                .cmp(&ys.len())
                .then_with(|| xs.cmp(ys))
                .then_with(|| x.1.len().cmp(&y.1.len()))
        } else {
            x.1.to_lowercase().cmp(&y.1.to_lowercase())
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    left.len().cmp(&right.len())
}

/// Делит имя на куски из цифр и не-цифр: цифровые куски сравниваются как числа.
fn chunks(name: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    for c in name.chars() {
        let digit = c.is_ascii_digit();
        match out.last_mut() {
            Some((d, text)) if *d == digit => text.push(c),
            _ => out.push((digit, c.to_string())),
        }
    }
    out
}

#[cfg(windows)]
fn platform_roots() -> Vec<DirEntry> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;

    let mask = unsafe { GetLogicalDrives() };
    let mut roots: Vec<DirEntry> = (0..26u32)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| {
            let letter = (b'A' + bit as u8) as char;
            let path = format!("{letter}:\\");
            let (kind, label) = drive_info(&path);
            let name = if label.is_empty() {
                format!("{kind} ({letter}:)")
            } else {
                format!("{label} ({letter}:)")
            };
            DirEntry { name, path }
        })
        .collect();
    // Дисков может не оказаться только при сбое вызова; без корня дерево пустое.
    if roots.is_empty() {
        roots.push(DirEntry { name: "Локальный диск (C:)".into(), path: "C:\\".into() });
    }
    roots
}

/// Тип диска и его метка тома: «Локальный диск», «Съемный диск», «Windows».
#[cfg(windows)]
fn drive_info(root: &str) -> (&'static str, String) {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW};

    // Те же значения, что и в WinBase.h: в windows-sys они лежат в другом
    // разделе, ради пары чисел подключать его целиком не нужно.
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    const DRIVE_RAMDISK: u32 = 6;

    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let kind = match unsafe { GetDriveTypeW(wide.as_ptr()) } {
        DRIVE_REMOVABLE => "Съемный диск",
        DRIVE_FIXED => "Локальный диск",
        DRIVE_REMOTE => "Сетевой диск",
        DRIVE_CDROM => "CD-диск",
        DRIVE_RAMDISK => "Диск в памяти",
        _ => "Диск",
    };

    let mut volume = [0u16; 261];
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            volume.as_mut_ptr(),
            volume.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    let label = if ok == 0 {
        String::new()
    } else {
        let len = volume.iter().position(|&c| c == 0).unwrap_or(volume.len());
        String::from_utf16_lossy(&volume[..len])
    };
    (kind, label)
}

#[cfg(not(windows))]
fn platform_roots() -> Vec<DirEntry> {
    // На Linux и macOS диск один — корень файловой системы; остальные точки
    // монтирования видны в нём обычными папками, как в «Компьютере» проводника.
    vec![DirEntry { name: "Файловая система".into(), path: "/".into() }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_are_absolute() {
        let roots = roots();
        assert!(!roots.is_empty());
        for root in roots {
            assert!(Path::new(&root.path).is_absolute(), "{root:?}");
            assert!(!root.name.is_empty(), "{root:?}");
        }
    }

    #[test]
    fn list_returns_dirs_only_sorted() {
        let base = std::env::temp_dir().join(format!("aipanel-dirs-{}", crate::util::now_ms()));
        std::fs::create_dir_all(base.join("бета")).unwrap();
        std::fs::create_dir_all(base.join("Альфа")).unwrap();
        std::fs::create_dir_all(base.join("папка10")).unwrap();
        std::fs::create_dir_all(base.join("папка2")).unwrap();
        std::fs::write(base.join("файл.txt"), "x").unwrap();

        let entries = list(base.to_str().unwrap()).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Альфа", "бета", "папка2", "папка10"]);
        assert!(entries.iter().all(|e| Path::new(&e.path).is_dir()));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(windows)]
    #[test]
    fn list_skips_windows_system_names() {
        let base = std::env::temp_dir().join(format!("aipanel-dirs-system-{}", crate::util::now_ms()));
        std::fs::create_dir_all(base.join("видимая")).unwrap();
        std::fs::create_dir_all(base.join("$Recycle.Bin")).unwrap();
        let names: Vec<String> = list(base.to_str().unwrap()).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["видимая"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn list_skips_hidden() {
        let base = std::env::temp_dir().join(format!("aipanel-dirs-hidden-{}", crate::util::now_ms()));
        std::fs::create_dir_all(base.join("видимая")).unwrap();
        std::fs::create_dir_all(base.join(".скрытая")).unwrap();
        let names: Vec<String> = list(base.to_str().unwrap()).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["видимая"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn natural_order() {
        let mut names = ["папка10", "папка2", "Альфа", "бета", "Папка1"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["Альфа", "бета", "Папка1", "папка2", "папка10"]);
    }

    #[test]
    fn list_reports_missing_and_relative() {
        let missing = std::env::temp_dir().join("aipanel-нет-такого-каталога");
        let error = list(missing.to_str().unwrap()).unwrap_err();
        assert!(error.starts_with("каталог не найден: "), "{error}");
        let error = list("относительный").unwrap_err();
        assert!(error.starts_with("нужен абсолютный путь: "), "{error}");
    }

    #[test]
    fn empty_path_is_roots() {
        assert_eq!(list("").unwrap(), roots());
    }
}
