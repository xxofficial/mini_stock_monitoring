#[derive(Clone, Copy)]
pub enum Action {
    Show,
    Hide,
    TogglePin,
    Reconnect,
    Exit,
}

#[cfg(windows)]
pub struct Tray {
    _icon: tray_icon::TrayIcon,
    pub events: std::sync::mpsc::Receiver<Action>,
}

#[cfg(windows)]
impl Tray {
    pub fn new(ctx: eframe::egui::Context) -> Result<Self, String> {
        use tray_icon::{
            MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
            menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
        };
        let menu = Menu::new();
        let show = MenuItem::new("显示微行情", true, None);
        let hide = MenuItem::new("隐藏窗口", true, None);
        let pin = MenuItem::new("切换置顶", true, None);
        let reconnect = MenuItem::new("重新连接行情", true, None);
        let exit = MenuItem::new("退出", true, None);
        menu.append_items(&[
            &show,
            &hide,
            &pin,
            &reconnect,
            &PredefinedMenuItem::separator(),
            &exit,
        ])
        .map_err(|error| error.to_string())?;
        let actions = [
            (show.id().clone(), Action::Show),
            (hide.id().clone(), Action::Hide),
            (pin.id().clone(), Action::TogglePin),
            (reconnect.id().clone(), Action::Reconnect),
            (exit.id().clone(), Action::Exit),
        ];
        let icon = tray_icon::Icon::from_rgba(crate::platform::icon_rgba(), 32, 32)
            .map_err(|error| error.to_string())?;
        let icon = TrayIconBuilder::new()
            .with_tooltip("微行情 · 左键显示，右键菜单")
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_menu_on_left_click(false)
            .build()
            .map_err(|error| error.to_string())?;
        let (tx, events) = std::sync::mpsc::channel();
        let menu_tx = tx.clone();
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, action)) = actions.iter().find(|(id, _)| id == event.id()) {
                let _ = menu_tx.send(*action);
                menu_ctx.request_repaint();
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                let _ = tx.send(Action::Show);
                ctx.request_repaint();
            }
        }));
        Ok(Self {
            _icon: icon,
            events,
        })
    }
}

#[cfg(not(windows))]
pub struct Tray {
    pub events: std::sync::mpsc::Receiver<Action>,
}
#[cfg(not(windows))]
impl Tray {
    pub fn new(_: eframe::egui::Context) -> Result<Self, String> {
        Err("当前平台未启用托盘".into())
    }
}
