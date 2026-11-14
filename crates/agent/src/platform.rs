//! Платформенные операции: проверка процесса и ввод текста в чужую консоль (Windows).
//!
//! Ввод в консоль делает отдельный короткоживущий процесс (`aipanel-agent inject`):
//! процесс может быть подключён только к одной консоли, и агент не должен терять свою.

#[cfg(windows)]
mod imp {
    use std::{process::Stdio, time::Duration};

    use tokio::io::AsyncWriteExt;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, STILL_ACTIVE},
        Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
        System::{
            Console::{
                AttachConsole, FreeConsole, WriteConsoleInputW, INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT,
                KEY_EVENT_RECORD, KEY_EVENT_RECORD_0,
            },
            Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    };

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const VK_RETURN: u16 = 0x0D;
    const VK_ESCAPE: u16 = 0x1B;

    pub fn pid_alive(pid: u32) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }

    pub fn no_window(cmd: &mut tokio::process::Command) {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    /// `Some(text)` — набрать текст и нажать Enter, `None` — нажать Esc.
    pub async fn console_input(pid: u32, text: Option<&str>) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("inject").arg(pid.to_string());
        if text.is_none() {
            cmd.arg("--esc");
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
        no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.unwrap_or_default().as_bytes()).await;
        }
        let out = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
            .await
            .map_err(|_| "ввод в консоль не завершился за 30 с".to_owned())?
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }

    fn key(vk: u16, ch: u16, down: bool) -> INPUT_RECORD {
        INPUT_RECORD {
            EventType: KEY_EVENT as u16,
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: down as i32,
                    wRepeatCount: 1,
                    wVirtualKeyCode: vk,
                    wVirtualScanCode: 0,
                    uChar: KEY_EVENT_RECORD_0 { UnicodeChar: ch },
                    dwControlKeyState: 0,
                },
            },
        }
    }

    unsafe fn write(handle: windows_sys::Win32::Foundation::HANDLE, records: &[INPUT_RECORD]) -> Result<(), String> {
        for chunk in records.chunks(512) {
            let mut written = 0u32;
            if WriteConsoleInputW(handle, chunk.as_ptr(), chunk.len() as u32, &mut written) == 0 {
                return Err(format!("WriteConsoleInput failed, error {}", GetLastError()));
            }
        }
        Ok(())
    }

    /// Тело процесса-помощника. Текст приходит в stdin в UTF-8.
    pub fn inject_main(pid: u32, esc: bool) -> Result<(), String> {
        let mut text = String::new();
        if !esc {
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).map_err(|e| e.to_string())?;
        }
        unsafe {
            FreeConsole();
            if AttachConsole(pid) == 0 {
                return Err(format!("cannot attach to console of pid {pid}, error {}", GetLastError()));
            }
            let name: Vec<u16> = "CONIN$\0".encode_utf16().collect();
            let handle = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                return Err(format!("cannot open console input, error {}", GetLastError()));
            }
            let result = (|| {
                if esc {
                    return write(handle, &[key(VK_ESCAPE, 0x1B, true), key(VK_ESCAPE, 0x1B, false)]);
                }
                let text = text.replace("\r\n", "\n");
                let text = text.trim_end_matches('\n');
                // Многострочный текст передаётся как вставка (bracketed paste),
                // иначе каждый перевод строки отправил бы сообщение по частям.
                let payload = if text.contains('\n') {
                    format!("\x1b[200~{}\x1b[201~", text.replace('\n', "\r"))
                } else {
                    text.to_owned()
                };
                let mut records = Vec::new();
                for unit in payload.encode_utf16() {
                    records.push(key(0, unit, true));
                    records.push(key(0, unit, false));
                }
                write(handle, &records)?;
                // Пауза нужна, чтобы интерфейс обработал текст до нажатия Enter.
                std::thread::sleep(Duration::from_millis(400));
                write(handle, &[key(VK_RETURN, 0x0D, true), key(VK_RETURN, 0x0D, false)])
            })();
            CloseHandle(handle);
            FreeConsole();
            result
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn pid_alive(pid: u32) -> bool {
        let proc_dir = std::path::Path::new("/proc");
        !proc_dir.is_dir() || proc_dir.join(pid.to_string()).exists()
    }

    pub fn no_window(_cmd: &mut tokio::process::Command) {}

    pub async fn console_input(_pid: u32, _text: Option<&str>) -> Result<(), String> {
        Err("ввод в открытую сессию поддерживается только в Windows".into())
    }

    pub fn inject_main(_pid: u32, _esc: bool) -> Result<(), String> {
        Err("console input is supported on Windows only".into())
    }
}

pub use imp::*;
