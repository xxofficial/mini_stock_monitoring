use mini_stock_monitor::hotkey::Hotkey;

#[derive(Debug, PartialEq, Eq)]
pub enum HotkeyEvent {
    Toggle,
    Recorded(Hotkey),
    RecordingError(String),
    RecordingCancelled,
}

#[cfg(windows)]
mod windows {
    use std::{cell::Cell, rc::Rc, sync::mpsc};

    use super::{Hotkey, HotkeyEvent};
    use eframe::egui;
    use raw_window_handle::HasWindowHandle;
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        UI::{
            Input::KeyboardAndMouse::{
                GetKeyState, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VK_CONTROL, VK_LWIN,
                VK_MENU, VK_RWIN, VK_SHIFT,
            },
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                WM_CHAR, WM_DEADCHAR, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_NCDESTROY,
                WM_SYSCHAR, WM_SYSDEADCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP,
            },
        },
    };

    const SUBCLASS_ID: usize = 0x4D53;
    const FIRST_ID: i32 = 0x4D00;

    struct CallbackState {
        hwnd: Cell<HWND>,
        id: Cell<Option<i32>>,
        recording: Cell<bool>,
        blocked_key: Cell<Option<u32>>,
        tx: mpsc::Sender<HotkeyEvent>,
        ctx: egui::Context,
    }

    /// Lives on the window's UI thread. The subclass owns a separate Rc until
    /// removal or WM_NCDESTROY, so either window/app destruction order is safe.
    pub struct GlobalHotkey {
        state: Rc<CallbackState>,
        binding: Option<Hotkey>,
        pub events: mpsc::Receiver<HotkeyEvent>,
    }

    impl GlobalHotkey {
        pub fn new(window: &impl HasWindowHandle, ctx: egui::Context) -> Result<Self, String> {
            let hwnd = crate::platform::hwnd(window).ok_or("无法获取快捷键所需的窗口")?;
            let (tx, events) = mpsc::channel();
            let state = Rc::new(CallbackState {
                hwnd: Cell::new(hwnd),
                id: Cell::new(None),
                recording: Cell::new(false),
                blocked_key: Cell::new(None),
                tx,
                ctx,
            });
            let subclass_ref = Rc::into_raw(state.clone());
            // SAFETY: our HWND belongs to this thread. The subclass retains its
            // own stable Rc pointer and releases it when detached.
            if unsafe {
                SetWindowSubclass(hwnd, Some(callback), SUBCLASS_ID, subclass_ref as usize)
            } == 0
            {
                let error = std::io::Error::last_os_error();
                // SAFETY: installation failed, so no callback owns this reference.
                unsafe {
                    drop(Rc::from_raw(subclass_ref));
                }
                return Err(format!("快捷键初始化失败：{error}"));
            }
            Ok(Self {
                state,
                binding: None,
                events,
            })
        }

        pub fn is_active(&self) -> bool {
            self.state.id.get().is_some()
        }

        pub fn is_recording(&self) -> bool {
            self.state.recording.get()
        }

        pub fn begin_recording(&mut self) -> Result<(), String> {
            if self.state.hwnd.get().is_null() {
                return Err("窗口已关闭，无法录制快捷键".into());
            }
            while self.events.try_recv().is_ok() {}
            self.state.blocked_key.set(None);
            self.state.recording.set(true);
            Ok(())
        }

        pub fn cancel_recording(&self) {
            self.state.recording.set(false);
        }

        pub fn set(&mut self, binding: Option<Hotkey>) -> Result<(), String> {
            self.cancel_recording();
            if self.binding == binding {
                return Ok(());
            }
            let hwnd = self.state.hwnd.get();
            if hwnd.is_null() {
                return Err("窗口已关闭，无法设置快捷键".into());
            }
            let previous = self.state.id.get();
            let next = if previous == Some(FIRST_ID) {
                FIRST_ID + 1
            } else {
                FIRST_ID
            };
            if let Some(binding) = binding {
                // SAFETY: registration is on the HWND's thread. Alternate IDs
                // let us validate the new binding before releasing the old one.
                if unsafe {
                    RegisterHotKey(hwnd, next, binding.modifiers | MOD_NOREPEAT, binding.key)
                } == 0
                {
                    let error = std::io::Error::last_os_error();
                    return Err(format!(
                        "无法启用 {binding}：快捷键已被占用或由系统保留，请换一组。({error})"
                    ));
                }
            }
            if let Some(previous) = previous {
                // SAFETY: previous is the registration this instance owns.
                if unsafe { UnregisterHotKey(hwnd, previous) } == 0 {
                    let error = std::io::Error::last_os_error();
                    if binding.is_some() {
                        // SAFETY: roll back only the newly acquired registration.
                        unsafe {
                            UnregisterHotKey(hwnd, next);
                        }
                    }
                    return Err(format!("原快捷键未能释放：{error}"));
                }
            }
            self.state.id.set(binding.map(|_| next));
            self.binding = binding;
            // Discard actions queued for a binding that has just been replaced.
            while self.events.try_recv().is_ok() {}
            Ok(())
        }
    }

    impl CallbackState {
        fn emit(&self, event: HotkeyEvent) {
            let _ = self.tx.send(event);
            self.ctx.request_repaint();
        }

        fn record_key(&self, modifiers: u32, key: u32, repeat: bool) {
            if repeat || Hotkey::is_modifier_key(key) {
                return;
            }
            if key == 0x1B && modifiers == 0 {
                self.recording.set(false);
                self.blocked_key.set(Some(key));
                self.emit(HotkeyEvent::RecordingCancelled);
                return;
            }
            match Hotkey::from_virtual_key(modifiers, key) {
                Ok(binding) => {
                    self.recording.set(false);
                    // Suppress the rest of this keystroke, including a queued
                    // WM_HOTKEY, character messages and auto-repeat.
                    self.blocked_key.set(Some(key));
                    self.emit(HotkeyEvent::Recorded(binding));
                }
                Err(error) => self.emit(HotkeyEvent::RecordingError(error)),
            }
        }
    }

    fn pressed_modifiers() -> u32 {
        let mut modifiers = 0;
        for (key, mask) in [
            (VK_CONTROL, 2),
            (VK_MENU, 1),
            (VK_SHIFT, 4),
            (VK_LWIN, 8),
            (VK_RWIN, 8),
        ] {
            // SAFETY: reads this UI thread's keyboard state at the current message.
            if unsafe { GetKeyState(key as i32) } < 0 {
                modifiers |= mask;
            }
        }
        modifiers
    }

    unsafe extern "system" fn callback(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        subclass_id: usize,
        reference: usize,
    ) -> LRESULT {
        // SAFETY: SetWindowSubclass stores the Rc reference until this callback
        // handles WM_NCDESTROY or GlobalHotkey::drop removes the subclass.
        let state = unsafe { &*(reference as *const CallbackState) };
        if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
            if state.recording.get() {
                state.record_key(pressed_modifiers(), wparam as u32, lparam & (1 << 30) != 0);
                return 0;
            }
            if state.blocked_key.get() == Some(wparam as u32) {
                return 0;
            }
        }
        if matches!(message, WM_KEYUP | WM_SYSKEYUP) {
            if state.blocked_key.get() == Some(wparam as u32) {
                state.blocked_key.set(None);
                return 0;
            }
            if state.recording.get() {
                return 0;
            }
        }
        if matches!(message, WM_CHAR | WM_SYSCHAR | WM_DEADCHAR | WM_SYSDEADCHAR)
            && (state.recording.get() || state.blocked_key.get().is_some())
        {
            return 0;
        }
        if message == WM_HOTKEY && state.id.get() == Some(wparam as i32) {
            if state.recording.get() {
                // A currently registered chord may arrive only as WM_HOTKEY.
                // Keep its registration reserved, but record it instead of hiding.
                state.record_key(lparam as u32 & 0xF, (lparam as u32 >> 16) & 0xFFFF, false);
            } else if state.blocked_key.get().is_none() {
                state.emit(HotkeyEvent::Toggle);
            }
            return 0;
        }
        if message == WM_KILLFOCUS {
            state.blocked_key.set(None);
            if state.recording.replace(false) {
                state.emit(HotkeyEvent::RecordingCancelled);
            }
        }
        if message == WM_NCDESTROY {
            if let Some(id) = state.id.take() {
                // SAFETY: this callback is running on the registered window.
                unsafe {
                    UnregisterHotKey(hwnd, id);
                }
            }
            state.hwnd.set(std::ptr::null_mut());
            // SAFETY: detach before releasing the subclass's Rc. No access to
            // state follows this release, even if the app guard is already gone.
            unsafe {
                RemoveWindowSubclass(hwnd, Some(callback), subclass_id);
                drop(Rc::from_raw(reference as *const CallbackState));
            }
        }
        // SAFETY: forward the original message to the remaining window procedures.
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    impl Drop for GlobalHotkey {
        fn drop(&mut self) {
            let hwnd = self.state.hwnd.get();
            if !hwnd.is_null() {
                // SAFETY: the guard is confined to the HWND's UI thread. If
                // removal fails, the subclass retains its Rc until WM_NCDESTROY.
                unsafe {
                    if let Some(id) = self.state.id.take() {
                        UnregisterHotKey(hwnd, id);
                    }
                    if RemoveWindowSubclass(hwnd, Some(callback), SUBCLASS_ID) != 0 {
                        drop(Rc::from_raw(Rc::as_ptr(&self.state)));
                    }
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use raw_window_handle::{HandleError, RawWindowHandle, Win32WindowHandle, WindowHandle};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_MESSAGE, SendMessageW,
        };

        struct TestWindow(HWND);

        impl TestWindow {
            fn new() -> Self {
                let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
                // SAFETY: creates a message-only test window on this test thread
                // using the built-in STATIC class; it never displays any UI.
                let hwnd = unsafe {
                    CreateWindowExW(
                        0,
                        class.as_ptr(),
                        std::ptr::null(),
                        0,
                        0,
                        0,
                        0,
                        0,
                        HWND_MESSAGE,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null(),
                    )
                };
                assert!(!hwnd.is_null());
                Self(hwnd)
            }
        }

        impl HasWindowHandle for TestWindow {
            fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
                let raw =
                    Win32WindowHandle::new(std::num::NonZeroIsize::new(self.0 as isize).unwrap());
                // SAFETY: TestWindow keeps this HWND alive for the borrow.
                Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(raw)) })
            }
        }

        impl Drop for TestWindow {
            fn drop(&mut self) {
                // SAFETY: this test thread created and owns the window.
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }

        fn acquire_available(manager: &mut GlobalHotkey) -> Hotkey {
            (13..=24)
                .map(|key| format!("Ctrl+Alt+Shift+F{key}").parse::<Hotkey>().unwrap())
                .find(|binding| manager.set(Some(*binding)).is_ok())
                .expect("at least one spare test shortcut")
        }

        #[test]
        fn native_registration_conflict_rebinding_delivery_and_both_drop_orders() {
            let window = TestWindow::new();
            let other_window = TestWindow::new();
            let mut manager = GlobalHotkey::new(&window, egui::Context::default()).unwrap();
            let mut other = GlobalHotkey::new(&other_window, egui::Context::default()).unwrap();
            let first = acquire_available(&mut manager);
            let second = acquire_available(&mut other);
            assert_ne!(first, second);
            assert!(manager.set(Some(second)).is_err());
            assert_eq!(manager.binding, Some(first));
            assert!(
                other.set(Some(first)).is_err(),
                "failed rebind must keep the original key registered"
            );
            assert!(
                manager.set(Some(first)).is_ok(),
                "applying the same key is a no-op"
            );

            let previous_id = manager.state.id.get().unwrap();
            other.set(None).unwrap();
            manager.set(Some(second)).unwrap();
            other.set(Some(first)).unwrap();
            let current_id = manager.state.id.get().unwrap();
            assert_ne!(previous_id, current_id);
            // SAFETY: deliver messages synchronously to this thread's test HWND;
            // no keyboard input is injected into the user's desktop.
            unsafe {
                SendMessageW(window.0, WM_HOTKEY, previous_id as usize, 0);
                SendMessageW(window.0, WM_HOTKEY, current_id as usize, 0);
            }
            assert_eq!(
                manager.events.try_iter().count(),
                1,
                "ignore messages from retired IDs"
            );

            // Destroying the HWND first must release the registration and leave
            // the guard safe to drop, without retaining a recycled HWND.
            drop(window);
            assert!(!manager.is_active());
            other.set(Some(second)).unwrap();
            drop(manager);

            // Destroying the guard first must remove its subclass and hotkey.
            drop(other);
            let mut replacement =
                GlobalHotkey::new(&other_window, egui::Context::default()).unwrap();
            replacement.set(Some(second)).unwrap();
            replacement.set(None).unwrap();
            assert!(!replacement.is_active());
        }

        #[test]
        fn records_f1_through_f8_and_combinations_without_repeat_or_toggle() {
            let window = TestWindow::new();
            let mut manager = GlobalHotkey::new(&window, egui::Context::default()).unwrap();
            let binding = acquire_available(&mut manager);
            let id = manager.state.id.get().unwrap();
            let hotkey_message = ((binding.key << 16) | binding.modifiers) as isize;
            for key in 0x70..=0x77 {
                manager.begin_recording().unwrap();
                // SAFETY: only this thread's message-only test window is targeted.
                unsafe {
                    SendMessageW(window.0, WM_KEYDOWN, key, 0);
                    SendMessageW(window.0, WM_KEYDOWN, key, 1 << 30);
                    SendMessageW(window.0, WM_HOTKEY, id as usize, hotkey_message);
                }
                let recorded: Vec<_> = manager.events.try_iter().collect();
                assert!(
                    matches!(recorded.as_slice(), [HotkeyEvent::Recorded(recorded)] if recorded.key == key as u32)
                );
                assert!(!manager.is_recording());
                assert!(manager.is_active());
                // SAFETY: release the synthetic test key on its owning thread.
                unsafe {
                    SendMessageW(window.0, WM_KEYUP, key, 0);
                }
            }
            for modifiers in 0..=15 {
                manager.begin_recording().unwrap();
                for key in [0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA5] {
                    manager.state.record_key(modifiers, key, false);
                }
                assert!(manager.events.try_recv().is_err());
                assert!(manager.is_recording());
                manager.state.record_key(modifiers, 0x77, false);
                assert_eq!(
                    manager.events.try_recv().unwrap(),
                    HotkeyEvent::Recorded(Hotkey {
                        modifiers,
                        key: 0x77
                    })
                );
            }
            // An already registered chord must be recordable too, without
            // temporarily releasing its registration to other applications.
            manager.begin_recording().unwrap();
            unsafe {
                SendMessageW(window.0, WM_HOTKEY, id as usize, hotkey_message);
            }
            assert_eq!(
                manager.events.try_recv().unwrap(),
                HotkeyEvent::Recorded(binding)
            );
            assert_eq!(manager.binding, Some(binding));
            assert!(manager.is_active());
            // Once released, the next press should toggle normally.
            unsafe {
                SendMessageW(window.0, WM_KEYUP, binding.key as usize, 0);
                SendMessageW(window.0, WM_HOTKEY, id as usize, hotkey_message);
            }
            assert_eq!(manager.events.try_recv().unwrap(), HotkeyEvent::Toggle);
        }

        #[test]
        fn invalid_keys_retry_and_escape_or_focus_loss_cancel_recording() {
            let window = TestWindow::new();
            let mut manager = GlobalHotkey::new(&window, egui::Context::default()).unwrap();
            manager.begin_recording().unwrap();
            manager.state.record_key(0, 0x7B, false);
            assert!(
                matches!(manager.events.try_recv().unwrap(), HotkeyEvent::RecordingError(error) if error.contains("F12"))
            );
            assert!(manager.is_recording());
            manager.state.record_key(0, 0x7B, true);
            assert!(manager.events.try_recv().is_err());
            manager.state.record_key(0, 0x1B, false);
            assert_eq!(
                manager.events.try_recv().unwrap(),
                HotkeyEvent::RecordingCancelled
            );
            assert!(!manager.is_recording());
            manager.begin_recording().unwrap();
            // SAFETY: simulates focus leaving this isolated message-only HWND.
            unsafe {
                SendMessageW(window.0, WM_KILLFOCUS, 0, 0);
            }
            assert_eq!(
                manager.events.try_recv().unwrap(),
                HotkeyEvent::RecordingCancelled
            );
            assert!(!manager.is_recording());
            manager.begin_recording().unwrap();
            // SAFETY: Alt and function keys use the system-key message path.
            unsafe {
                SendMessageW(window.0, WM_SYSKEYDOWN, 0x77, 0);
            }
            assert!(
                matches!(manager.events.try_recv().unwrap(), HotkeyEvent::Recorded(binding) if binding.key == 0x77)
            );
            manager.begin_recording().unwrap();
            manager.cancel_recording();
            assert!(!manager.is_recording());
        }
    }
}

#[cfg(windows)]
pub use windows::GlobalHotkey;

#[cfg(not(windows))]
pub struct GlobalHotkey {
    pub events: std::sync::mpsc::Receiver<HotkeyEvent>,
}
#[cfg(not(windows))]
impl GlobalHotkey {
    pub fn new<T>(_: &T, _: eframe::egui::Context) -> Result<Self, String> {
        Err("当前平台未启用全局快捷键".into())
    }
    pub fn is_active(&self) -> bool {
        false
    }
    pub fn is_recording(&self) -> bool {
        false
    }
    pub fn begin_recording(&mut self) -> Result<(), String> {
        Err("当前平台未启用快捷键录制".into())
    }
    pub fn cancel_recording(&self) {}
    pub fn set(&mut self, _: Option<mini_stock_monitor::hotkey::Hotkey>) -> Result<(), String> {
        Err("当前平台未启用全局快捷键".into())
    }
}
