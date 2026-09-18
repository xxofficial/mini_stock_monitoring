#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod global_hotkey;
mod platform;
mod tray;
mod ui;

use eframe::egui;
use mini_stock_monitor::config::ConfigStore;

fn main() {
    let Some(_instance) = platform::single_instance() else {
        return;
    };
    let store = ConfigStore::default();
    let (settings, warning) = store.load();
    let viewport = egui::ViewportBuilder::default()
        .with_title(platform::WINDOW_TITLE)
        .with_app_id("mini-stock-monitor")
        .with_decorations(false)
        .with_taskbar(false)
        .with_transparent(true)
        .with_resizable(false)
        .with_inner_size([380.0, ui::initial_height(&settings)])
        .with_window_level(if settings.always_on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        })
        .with_icon(egui::IconData {
            rgba: platform::icon_rgba(),
            width: 32,
            height: 32,
        });
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        centered: settings.position.is_none(),
        persist_window: false,
        ..Default::default()
    };
    if let Err(error) = eframe::run_native(
        platform::WINDOW_TITLE,
        options,
        Box::new(move |cc| Ok(Box::new(ui::StockApp::new(cc, settings, store, warning)?))),
    ) {
        platform::show_error(&format!(
            "启动失败：{error}\n请确认显卡驱动支持 OpenGL 3.3。"
        ));
    }
}
