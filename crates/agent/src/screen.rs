//! Снимок экрана машины агента: панель открывает его поверх интерфейса.
//!
//! Снимается каждый монитор отдельно, в его собственном разрешении. PNG, который
//! не влезает в сообщение WebSocket, уменьшается вдвое, пока не влезет: панель
//! показывает снимок, а не хранит исходник.

#[cfg(windows)]
use aipanel_proto::MAX_SCREEN_BYTES;

/// Один снятый экран.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Shot {
    pub index: u32,
    pub count: u32,
    pub label: String,
    /// Размер картинки после возможного уменьшения.
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

/// Делает процесс знающим о DPI: иначе на мониторах с масштабом 125–200%
/// Windows отдаёт уменьшенную копию экрана вместо настоящих пикселей.
#[cfg(windows)]
pub fn enable_dpi_awareness() {
    imp::enable_dpi_awareness();
}

#[cfg(not(windows))]
pub fn enable_dpi_awareness() {}

/// Снимает все экраны машины. Основной идёт первым.
#[cfg(windows)]
pub fn capture() -> Result<Vec<Shot>, String> {
    imp::capture()
}

#[cfg(not(windows))]
pub fn capture() -> Result<Vec<Shot>, String> {
    Err("снимок экрана из панели есть только на Windows".into())
}

/// PNG из пикселей RGB (по 3 байта на точку).
#[cfg(windows)]
fn encode_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    use std::io::Cursor;

    let mut out = Cursor::new(Vec::new());
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        // Быстрое сжатие: снимок делается по нажатию, ждать секунды нельзя.
        encoder.set_compression(png::Compression::Fast);
        encoder.set_filter(png::FilterType::Sub);
        let mut writer = encoder.write_header().map_err(|e| format!("PNG: {e}"))?;
        writer.write_image_data(rgb).map_err(|e| format!("PNG: {e}"))?;
        writer.finish().map_err(|e| format!("PNG: {e}"))?;
    }
    Ok(out.into_inner())
}

/// Уменьшает картинку вдвое усреднением четырёх точек. Возвращает новые пиксели
/// и размер.
#[cfg(windows)]
fn halve(rgb: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let (w, h) = (width as usize, height as usize);
    let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = vec![0u8; nw * nh * 3];
    for y in 0..nh {
        for x in 0..nw {
            let mut sum = [0u32; 3];
            let mut n = 0u32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let (px, py) = (x * 2 + dx, y * 2 + dy);
                    if px >= w || py >= h {
                        continue;
                    }
                    let at = (py * w + px) * 3;
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s += u32::from(rgb[at + c]);
                    }
                    n += 1;
                }
            }
            let at = (y * nw + x) * 3;
            for (c, s) in sum.iter().enumerate() {
                out[at + c] = (s / n) as u8;
            }
        }
    }
    (out, nw as u32, nh as u32)
}

/// Кодирует снимок, уменьшая его, пока PNG не влезет в предел сообщения.
#[cfg(windows)]
fn shrink_to_fit(mut rgb: Vec<u8>, mut width: u32, mut height: u32) -> Result<(Vec<u8>, u32, u32), String> {
    loop {
        let png = encode_png(&rgb, width, height)?;
        if png.len() <= MAX_SCREEN_BYTES || width <= 640 || height <= 360 {
            return Ok((png, width, height));
        }
        let (smaller, w, h) = halve(&rgb, width, height);
        rgb = smaller;
        width = w;
        height = h;
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use windows_sys::Win32::{
        Foundation::{LPARAM, RECT, TRUE},
        Graphics::Gdi::{
            BitBlt, CreateCompatibleDC, CreateDCW, CreateDIBSection, DeleteDC, DeleteObject,
            EnumDisplayMonitors, GetDeviceCaps, GetMonitorInfoW, SelectObject, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HORZRES, HDC, HMONITOR, MONITORINFOEXW,
            SRCCOPY, VERTRES,
        },
        UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
        UI::WindowsAndMessaging::MONITORINFOF_PRIMARY,
    };

    use super::{shrink_to_fit, Shot};

    pub fn enable_dpi_awareness() {
        // Ошибку не проверяем: если осведомлённость уже задана (манифестом или
        // вызовом раньше), Windows просто вернёт FALSE, и это не мешает съёмке.
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }

    /// Монитор глазами GDI: имя устройства вида `\\.\DISPLAY1`.
    struct Monitor {
        device: String,
        primary: bool,
    }

    pub fn capture() -> Result<Vec<Shot>, String> {
        // Осведомлённость о DPI нужна до первого DC: без неё Windows отдаёт
        // уменьшенную копию экрана вместо настоящих пикселей.
        enable_dpi_awareness();
        let mut monitors: Vec<Monitor> = Vec::new();
        let ok = unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(collect),
                &mut monitors as *mut Vec<Monitor> as LPARAM,
            )
        };
        if ok == 0 || monitors.is_empty() {
            return Err("не удалось перечислить экраны".into());
        }
        // Основной экран — первым: с него начинается просмотр.
        monitors.sort_by_key(|m| !m.primary);

        let count = monitors.len() as u32;
        let mut shots = Vec::with_capacity(monitors.len());
        for (i, monitor) in monitors.iter().enumerate() {
            let index = i as u32;
            let label = if monitor.primary {
                format!("Экран {} · основной", index + 1)
            } else {
                format!("Экран {}", index + 1)
            };
            let (rgb, width, height) = grab(&monitor.device)
                .map_err(|e| format!("экран {}: {e}", index + 1))?;
            let (png, width, height) = shrink_to_fit(rgb, width, height)?;
            shots.push(Shot { index, count, label, width, height, png });
        }
        Ok(shots)
    }

    unsafe extern "system" fn collect(hmonitor: HMONITOR, _hdc: HDC, _rect: *mut RECT, data: LPARAM) -> i32 {
        let monitors = &mut *(data as *mut Vec<Monitor>);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmonitor, &mut info.monitorInfo) == 0 {
            return TRUE;
        }
        let device = String::from_utf16_lossy(&info.szDevice)
            .trim_end_matches('\0')
            .to_owned();
        if device.is_empty() {
            return TRUE;
        }
        let primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
        monitors.push(Monitor { device, primary });
        TRUE
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Снимает экран `device` и отдаёт пиксели RGB построчно сверху вниз.
    fn grab(device: &str) -> Result<(Vec<u8>, u32, u32), String> {
        let driver = wide("DISPLAY");
        let name = wide(device);
        unsafe {
            let screen = CreateDCW(driver.as_ptr(), name.as_ptr(), std::ptr::null(), std::ptr::null());
            if screen.is_null() {
                return Err("нет доступа к экрану".into());
            }
            let width = GetDeviceCaps(screen, HORZRES as i32);
            let height = GetDeviceCaps(screen, VERTRES as i32);
            let mem = if width > 0 && height > 0 { CreateCompatibleDC(screen) } else { std::ptr::null_mut() };
            if mem.is_null() {
                DeleteDC(screen);
                return Err("экран не отдал размер".into());
            }

            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width;
            // Отрицательная высота — строки сверху вниз, как их ждёт PNG.
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;

            let mut bits: *mut c_void = std::ptr::null_mut();
            let dib = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if dib.is_null() || bits.is_null() {
                DeleteDC(mem);
                DeleteDC(screen);
                return Err("не хватило памяти на снимок".into());
            }
            let old = SelectObject(mem, dib);
            let copied = BitBlt(mem, 0, 0, width, height, screen, 0, 0, SRCCOPY);
            SelectObject(mem, old);
            if copied == 0 {
                DeleteObject(dib);
                DeleteDC(mem);
                DeleteDC(screen);
                return Err("Windows не отдала изображение экрана".into());
            }

            // GDI отдаёт BGRA, а PNG без прозрачности ждёт RGB.
            let bgra = std::slice::from_raw_parts(bits as *const u8, width as usize * height as usize * 4);
            let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
            for px in bgra.chunks_exact(4) {
                rgb.extend_from_slice(&[px[2], px[1], px[0]]);
            }

            DeleteObject(dib);
            DeleteDC(mem);
            DeleteDC(screen);
            Ok((rgb, width as u32, height as u32))
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn halving_keeps_size_and_halves_pixels() {
        // 2×2 из четырёх разных цветов становится одной средней точкой.
        let rgb = vec![
            0, 0, 0, //
            200, 100, 50, //
            100, 200, 150, //
            0, 0, 100,
        ];
        let (out, w, h) = halve(&rgb, 2, 2);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![75, 75, 75]);
    }

    #[test]
    fn odd_sizes_round_up() {
        let rgb = vec![10u8; 3 * 3 * 3];
        let (out, w, h) = halve(&rgb, 3, 3);
        assert_eq!((w, h), (2, 2));
        assert_eq!(out.len(), 2 * 2 * 3);
    }

    #[test]
    fn png_is_decodable() {
        let rgb: Vec<u8> = (0..64 * 48 * 3).map(|i| (i % 251) as u8).collect();
        let png = encode_png(&rgb, 64, 48).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (64, 48));
        assert_eq!(&buf[..rgb.len()], &rgb[..]);
    }

    /// Снимает настоящий экран: в безголовой среде смысла нет, поэтому вручную.
    #[test]
    #[ignore]
    fn captures_a_screen() {
        let shots = capture().unwrap();
        assert!(!shots.is_empty());
        assert_eq!(shots[0].index, 0);
        assert!(shots[0].png.len() <= MAX_SCREEN_BYTES);
    }
}
