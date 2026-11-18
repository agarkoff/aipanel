//! Платформенные операции: проверка процесса и ввод текста в чужую консоль (Windows).
//!
//! Ввод в консоль делает отдельный короткоживущий процесс (`aipanel-agent inject`):
//! процесс может быть подключён только к одной консоли, и агент не должен терять свою.

#[cfg(windows)]
mod imp {
    use std::{process::Stdio, time::Duration};

    use super::InjectMode;
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
    /// Имя окна Windows Terminal, куда открываются сессии вкладками. Отдельное имя,
    /// а не «последнее окно»: иначе вкладка легла бы в терминал, где человек работает.
    const TAB_WINDOW: &str = "aipanel";
    const VK_RETURN: u16 = 0x0D;
    const VK_ESCAPE: u16 = 0x1B;
    const VK_DOWN: u16 = 0x28;

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

    /// Есть ли запущенный процесс `exe`, в командной строке которого встречается `needle`.
    /// `needle` должен состоять только из букв, цифр, `-` и `_`.
    pub async fn process_cmdline_contains(exe: &str, needle: &str) -> bool {
        let script = format!(
            "@(Get-CimInstance Win32_Process | Where-Object {{ $_.Name -eq '{exe}' -and $_.CommandLine -like '*{needle}*' }}).Count"
        );
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        no_window(&mut cmd);
        match tokio::time::timeout(Duration::from_secs(15), cmd.output()).await {
            Ok(Ok(out)) => String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().is_ok_and(|n| n > 0),
            _ => false,
        }
    }

    /// Добавляет аргумент в командную строку по правилам CommandLineToArgvW.
    fn quote_arg(arg: &str, out: &mut String) {
        if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
            out.push_str(arg);
            return;
        }
        out.push('"');
        let mut backslashes = 0usize;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    out.extend(std::iter::repeat('\\').take(backslashes * 2 + 1));
                    out.push('"');
                    backslashes = 0;
                }
                _ => {
                    out.extend(std::iter::repeat('\\').take(backslashes));
                    out.push(c);
                    backslashes = 0;
                }
            }
        }
        out.extend(std::iter::repeat('\\').take(backslashes * 2));
        out.push('"');
    }

    /// Отсоединяет запускаемый процесс от консоли: он переживёт закрытие окна, где его запустили.
    pub fn detach(cmd: &mut std::process::Command) {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }

    pub fn kill_pid(pid: u32) -> bool {
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if h.is_null() {
                return false;
            }
            let ok = TerminateProcess(h, 1) != 0;
            CloseHandle(h);
            ok
        }
    }

    static PROFILE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    /// Открывать каждую сессию отдельным окном, а не вкладкой в общем окне агента.
    static NEW_WINDOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    pub fn set_terminal_profile(profile: &str) {
        let _ = PROFILE.set(profile.trim().to_owned());
    }

    pub fn set_terminal_window(new_window: bool) {
        NEW_WINDOW.store(new_window, std::sync::atomic::Ordering::Relaxed);
    }

    /// Профиль по умолчанию из настроек Windows Terminal (guid или имя).
    fn default_wt_profile() -> Option<String> {
        let local = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
        [
            "Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/LocalState/settings.json",
            "Packages/Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe/LocalState/settings.json",
            "Microsoft/Windows Terminal/settings.json",
        ]
        .iter()
        .filter_map(|rel| std::fs::read_to_string(local.join(rel)).ok())
        .find_map(|text| {
            let after = text.get(text.find("\"defaultProfile\"")? + "\"defaultProfile\"".len()..)?;
            let value = after.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
            Some(value[..value.find('"')?].to_owned())
        })
        .filter(|p| !p.is_empty())
    }

    unsafe extern "system" fn ignore_ctrl_c(_event: u32) -> i32 {
        1
    }

    /// Тело обёртки окна: запускает программу в этой же консоли и ждёт её завершения.
    /// Возвращает код выхода для окна: 0, чтобы терминал закрылся сам; настоящий код
    /// сохраняется, только если программа упала в первые секунды, чтобы была видна ошибка.
    pub fn run_window(program: &str, args: &[String]) -> i32 {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        // Ctrl+C предназначен программе: обёртка его переживает и дожидается её выхода.
        // Собственный обработчик (а не «игнорировать») не наследуется программой.
        unsafe { SetConsoleCtrlHandler(Some(ignore_ctrl_c), 1) };
        let started = std::time::Instant::now();
        match std::process::Command::new(program).args(args).status() {
            Ok(status) => {
                let code = status.code().unwrap_or(1);
                if code != 0 && started.elapsed() < Duration::from_secs(15) {
                    code
                } else {
                    0
                }
            }
            Err(e) => {
                eprintln!("не удалось запустить {program}: {e}");
                1
            }
        }
    }

    /// Запускает программу в Windows Terminal (`wt.exe`) — вкладкой в окне агента или,
    /// если так настроено, в новом окне; без `wt.exe` — в отдельном окне обычной консоли.
    /// Возвращает pid запущенного процесса.
    pub fn spawn_window(program: &str, args: &[String], cwd: &str) -> Result<u32, String> {
        use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;
        // Windows Terminal разбирает `;` как разделитель своих команд, а `\;` передаёт
        // программе как обычный символ: без этого текст сообщения мог бы подставить команду.
        // Кавычки экранируются дважды: wt собирает командную строку программы заново и
        // не возвращает им обратные слеши, из-за чего текст обрывался на первой кавычке.
        let escape = |s: &str| s.replace(';', "\\;").replace('"', "\\\"");
        // Сессии открываются вкладками в одном окне (`-w aipanel`), чтобы каждое нажатие
        // «Новая сессия» не добавляло окно на панель задач. Окно появляется само при первой
        // сессии; если его закрыть, следующая снова его откроет.
        let mut wt = if NEW_WINDOW.load(std::sync::atomic::Ordering::Relaxed) {
            String::from("wt.exe -w new ")
        } else {
            format!("wt.exe -w {TAB_WINDOW} ")
        };
        // Без профиля wt берёт общие настройки (чёрный фон), а не оформление вашего профиля.
        let configured = PROFILE.get().map(String::as_str).filter(|p| !p.is_empty());
        if let Some(profile) = configured.map(str::to_owned).or_else(default_wt_profile) {
            wt.push_str("-p ");
            quote_arg(&escape(&profile), &mut wt);
            wt.push(' ');
        }
        wt.push_str("-d ");
        quote_arg(&escape(cwd), &mut wt);
        // Программа запускается через обёртку `run-window`: Windows Terminal оставляет окно
        // открытым после ненулевого кода выхода, а обёртка закрывает его кодом 0.
        let me = std::env::current_exe().map(|p| p.to_string_lossy().into_owned());
        let wrapped: Vec<&str> = match &me {
            Ok(exe) => [exe.as_str(), "run-window", program].into_iter().chain(args.iter().map(String::as_str)).collect(),
            Err(_) => std::iter::once(program).chain(args.iter().map(String::as_str)).collect(),
        };
        for a in wrapped {
            wt.push(' ');
            quote_arg(&escape(a), &mut wt);
        }
        match create(&wt, cwd, 0) {
            Ok(pid) => Ok(pid),
            Err(_) => {
                let mut line = String::new();
                quote_arg(program, &mut line);
                for a in args {
                    line.push(' ');
                    quote_arg(a, &mut line);
                }
                create(&line, cwd, CREATE_NEW_CONSOLE)
            }
        }
    }

    /// Дескрипторы агента не наследуются: иначе программа писала бы в его лог, а не в окно.
    fn create(line: &str, cwd: &str, flags: u32) -> Result<u32, String> {
        use windows_sys::Win32::System::Threading::{
            CreateProcessW, PROCESS_INFORMATION, STARTF_USESHOWWINDOW, STARTUPINFOW,
        };
        let mut wide: Vec<u16> = line.encode_utf16().chain(Some(0)).collect();
        let wcwd: Vec<u16> = cwd.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let mut si: STARTUPINFOW = std::mem::zeroed();
            si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            si.dwFlags = STARTF_USESHOWWINDOW;
            si.wShowWindow = 1; // SW_SHOWNORMAL
            let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
            let ok = CreateProcessW(
                std::ptr::null(),
                wide.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags,
                std::ptr::null(),
                wcwd.as_ptr(),
                &si,
                &mut pi,
            );
            if ok == 0 {
                return Err(format!("CreateProcess: код ошибки {}", GetLastError()));
            }
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
            Ok(pi.dwProcessId)
        }
    }

    /// `Some(text)` — набрать текст и нажать Enter, `None` — нажать Esc.
    pub async fn console_input(pid: u32, text: Option<&str>) -> Result<(), String> {
        let mode = if text.is_none() { InjectMode::Esc } else { InjectMode::Text };
        run_inject(pid, mode, text.unwrap_or_default()).await
    }

    /// Нажать клавиши по сценарию (см. [`super::Key`]): ответ в меню выбора,
    /// где нужен не текст с Enter, а цифры и стрелки.
    pub async fn console_keys(pid: u32, keys: &[super::Key]) -> Result<(), String> {
        run_inject(pid, InjectMode::Keys, &super::encode_keys(keys)).await
    }

    async fn run_inject(pid: u32, mode: InjectMode, input: &str) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("inject").arg(pid.to_string());
        match mode {
            InjectMode::Text => {}
            InjectMode::Esc => {
                cmd.arg("--esc");
            }
            InjectMode::Keys => {
                cmd.arg("--keys");
            }
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
        no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes()).await;
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

    fn press(handle: windows_sys::Win32::Foundation::HANDLE, vk: u16, ch: u16) -> Result<(), String> {
        unsafe { write(handle, &[key(vk, ch, true), key(vk, ch, false)]) }
    }

    /// Сценарий клавиш: между нажатиями пауза, чтобы интерфейс успел
    /// перерисоваться (иначе часть нажатий уходит не в тот пункт меню).
    fn run_keys(handle: windows_sys::Win32::Foundation::HANDLE, keys: &[super::Key]) -> Result<(), String> {
        use super::Key;
        for k in keys {
            match k {
                Key::Text(text) => {
                    let mut records = Vec::new();
                    for unit in text.encode_utf16() {
                        records.push(key(0, unit, true));
                        records.push(key(0, unit, false));
                    }
                    unsafe { write(handle, &records)? };
                }
                Key::Down => press(handle, VK_DOWN, 0)?,
                Key::Enter => press(handle, VK_RETURN, 0x0D)?,
                Key::Esc => press(handle, VK_ESCAPE, 0x1B)?,
                Key::Wait(ms) => std::thread::sleep(Duration::from_millis(*ms)),
            }
            if !matches!(k, Key::Wait(_)) {
                std::thread::sleep(Duration::from_millis(super::KEY_PAUSE_MS));
            }
        }
        Ok(())
    }

    /// Тело процесса-помощника. Текст (или сценарий клавиш) приходит в stdin в UTF-8.
    pub fn inject_main(pid: u32, mode: InjectMode) -> Result<(), String> {
        let esc = mode == InjectMode::Esc;
        let mut text = String::new();
        if !esc {
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).map_err(|e| e.to_string())?;
        }
        let keys = if mode == InjectMode::Keys { Some(super::decode_keys(&text)?) } else { None };
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
                if let Some(keys) = &keys {
                    return run_keys(handle, keys);
                }
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

    pub async fn process_cmdline_contains(_exe: &str, _needle: &str) -> bool {
        false
    }

    pub fn run_window(_program: &str, _args: &[String]) -> i32 {
        1
    }

    pub fn set_terminal_profile(_profile: &str) {}

    pub fn set_terminal_window(_new_window: bool) {}

    pub fn detach(_cmd: &mut std::process::Command) {}

    pub fn kill_pid(pid: u32) -> bool {
        std::process::Command::new("kill").arg(pid.to_string()).status().is_ok_and(|s| s.success())
    }

    pub fn spawn_window(_program: &str, _args: &[String], _cwd: &str) -> Result<u32, String> {
        Err("открытие окна консоли поддерживается только в Windows".into())
    }

    pub async fn console_input(_pid: u32, _text: Option<&str>) -> Result<(), String> {
        Err("ввод в открытую сессию поддерживается только в Windows".into())
    }

    pub async fn console_keys(_pid: u32, _keys: &[super::Key]) -> Result<(), String> {
        Err("ввод в открытую сессию поддерживается только в Windows".into())
    }

    pub fn inject_main(_pid: u32, _mode: super::InjectMode) -> Result<(), String> {
        Err("console input is supported on Windows only".into())
    }
}

pub use imp::*;

/// Что делает процесс-помощник `inject`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InjectMode {
    /// Набрать текст из stdin и нажать Enter.
    Text,
    /// Нажать Esc.
    Esc,
    /// Нажать клавиши по сценарию из stdin (см. [`encode_keys`]).
    Keys,
}

/// Пауза после каждого нажатия в сценарии клавиш, мс.
pub const KEY_PAUSE_MS: u64 = 150;

/// Нажатие в сценарии клавиш.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Набрать символы (цифра пункта меню или свой текст) без Enter.
    Text(String),
    Down,
    Enter,
    Esc,
    /// Подождать, мс: интерфейсу нужно время, чтобы открыть поле или следующий вопрос.
    Wait(u64),
}

/// Сценарий для stdin помощника: строка на нажатие (`t <текст>`, `k down`, `w 300`).
/// Переводы строк в тексте заменяются пробелами: поле ответа однострочное.
pub fn encode_keys(keys: &[Key]) -> String {
    let mut out = String::new();
    for k in keys {
        let line = match k {
            Key::Text(t) => format!("t {}", t.replace(['\r', '\n'], " ")),
            Key::Down => "k down".into(),
            Key::Enter => "k enter".into(),
            Key::Esc => "k esc".into(),
            Key::Wait(ms) => format!("w {ms}"),
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

pub fn decode_keys(text: &str) -> Result<Vec<Key>, String> {
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter(|l| !l.is_empty())
        .map(|line| {
            let (op, arg) = line.split_once(' ').unwrap_or((line, ""));
            match (op, arg) {
                ("t", t) => Ok(Key::Text(t.to_owned())),
                ("k", "down") => Ok(Key::Down),
                ("k", "enter") => Ok(Key::Enter),
                ("k", "esc") => Ok(Key::Esc),
                ("w", ms) => ms.parse().map(|ms: u64| Key::Wait(ms.min(5_000))).map_err(|e| e.to_string()),
                _ => Err(format!("bad key script line: {line}")),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_script_round_trip() {
        let keys = vec![
            Key::Text("2".into()),
            Key::Text("свой ответ с пробелом".into()),
            Key::Down,
            Key::Enter,
            Key::Wait(300),
            Key::Esc,
        ];
        assert_eq!(decode_keys(&encode_keys(&keys)).unwrap(), keys);
        assert_eq!(decode_keys(&encode_keys(&[Key::Text("a\nb".into())])).unwrap(), [Key::Text("a b".into())]);
        assert!(decode_keys("x y\n").is_err());
        assert_eq!(decode_keys("t 1\r\nk down\r\n").unwrap(), [Key::Text("1".into()), Key::Down]);
    }
}
