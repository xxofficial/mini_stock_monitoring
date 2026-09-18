pub const WINDOW_TITLE: &str = "微行情 · Mini Stock Monitor";

pub fn icon_rgba() -> Vec<u8> {
    let mut pixels = vec![0u8; 32 * 32 * 4];
    for y in 0..32usize {
        for x in 0..32usize {
            let corner_x = (x as i32).clamp(6, 25);
            let corner_y = (y as i32).clamp(6, 25);
            if (x as i32 - corner_x).pow(2) + (y as i32 - corner_y).pow(2) <= 36 {
                pixels[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4].copy_from_slice(&[20, 27, 39, 255]);
            }
        }
    }
    for (a, b) in [
        ((6, 23), (13, 16)),
        ((13, 16), (18, 19)),
        ((18, 19), (26, 9)),
    ] {
        for step in 0..=80 {
            let t = step as f32 / 80.0;
            let x = (a.0 as f32 + (b.0 - a.0) as f32 * t).round() as usize;
            let y = (a.1 as f32 + (b.1 - a.1) as f32 * t).round() as usize;
            for dy in 0..2 {
                for dx in 0..2 {
                    pixels[((y + dy) * 32 + x + dx) * 4..((y + dy) * 32 + x + dx) * 4 + 4]
                        .copy_from_slice(&[98, 218, 180, 255]);
                }
            }
        }
    }
    pixels
}

#[cfg(windows)]
mod windows {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, RECT},
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect},
        System::Threading::CreateMutexW,
        UI::WindowsAndMessaging::{
            FindWindowW, GetWindowRect, IsIconic, MB_ICONERROR, MB_OK, MessageBoxW, SW_RESTORE,
            SW_SHOW, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos,
            ShowWindow,
        },
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    pub struct InstanceGuard(HANDLE);

    impl Drop for InstanceGuard {
        fn drop(&mut self) {
            // SAFETY: this guard owns the valid handle returned by CreateMutexW.
            unsafe {
                if !self.0.is_null() {
                    CloseHandle(self.0);
                }
            }
        }
    }

    pub fn single_instance() -> Option<InstanceGuard> {
        let name = wide("Local\\MiniStockMonitor.Desktop.1");
        // SAFETY: the UTF-16 buffers are null-terminated and live through the calls.
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                return Some(InstanceGuard(handle));
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                let title = wide(super::WINDOW_TITLE);
                let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
                if !hwnd.is_null() {
                    ShowWindow(
                        hwnd,
                        if IsIconic(hwnd) != 0 {
                            SW_RESTORE
                        } else {
                            SW_SHOW
                        },
                    );
                    SetForegroundWindow(hwnd);
                }
                None
            } else {
                Some(InstanceGuard(handle))
            }
        }
    }

    fn hwnd(window: &impl HasWindowHandle) -> Option<HWND> {
        match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
            _ => None,
        }
    }

    pub fn remove_native_border(window: &impl HasWindowHandle) {
        use windows_sys::Win32::Graphics::Dwm::{
            DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DwmSetWindowAttribute,
        };
        let Some(hwnd) = hwnd(window) else {
            return;
        };
        let color: u32 = DWMWA_COLOR_NONE;
        // SAFETY: a COLORREF pointer and its exact size are passed for our own HWND.
        // Windows 10 may reject this Windows 11 attribute; borderless rendering still works.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR as u32,
                (&color as *const u32).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
    }

    pub fn cursor_position() -> Option<[i32; 2]> {
        let mut point = windows_sys::Win32::Foundation::POINT::default();
        // SAFETY: GetCursorPos writes to an initialized, writable POINT.
        if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) } != 0 {
            Some([point.x, point.y])
        } else {
            None
        }
    }

    pub fn position(window: &impl HasWindowHandle) -> Option<[i32; 2]> {
        let hwnd = hwnd(window)?;
        let mut rect = RECT::default();
        // SAFETY: hwnd comes from the live eframe window; rect is writable.
        if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
            None
        } else {
            Some([rect.left, rect.top])
        }
    }

    pub fn restore_position(window: &impl HasWindowHandle, saved: Option<[i32; 2]>) {
        let Some(hwnd) = hwnd(window) else { return };
        // SAFETY: only a live app-owned HWND and stack-allocated Win32 structs are used.
        unsafe {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect) == 0 {
                return;
            }
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            if let Some([x, y]) = saved {
                rect = RECT {
                    left: x,
                    top: y,
                    right: x.saturating_add(width),
                    bottom: y.saturating_add(height),
                };
            }
            let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(monitor, &mut info) == 0 {
                return;
            }
            let work = info.rcWork;
            let x = rect
                .left
                .clamp(work.left, (work.right - width).max(work.left));
            let y = rect
                .top
                .clamp(work.top, (work.bottom - height).max(work.top));
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    pub fn show_error(message: &str) {
        // SAFETY: both UTF-16 buffers are valid for the duration of the modal call.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                wide(message).as_ptr(),
                wide("微行情").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

#[cfg(windows)]
pub use windows::*;

#[cfg(not(windows))]
pub struct InstanceGuard;
#[cfg(not(windows))]
pub fn single_instance() -> Option<InstanceGuard> {
    Some(InstanceGuard)
}
#[cfg(not(windows))]
pub fn position(_: &eframe::Frame) -> Option<[i32; 2]> {
    None
}
#[cfg(not(windows))]
pub fn restore_position<T>(_: &T, _: Option<[i32; 2]>) {}
#[cfg(not(windows))]
pub fn remove_native_border<T>(_: &T) {}
#[cfg(not(windows))]
pub fn cursor_position() -> Option<[i32; 2]> {
    None
}
#[cfg(not(windows))]
pub fn show_error(message: &str) {
    eprintln!("{message}");
}
