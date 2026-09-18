use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{FixedOffset, Utc};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Layout, Pos2, Rect, RichText,
    Sense, Stroke, Vec2, ViewportCommand, pos2, vec2,
};
use mini_stock_monitor::{
    config::{ConfigStore, FeedMode, MAX_SYMBOLS, Settings},
    feed::{Feed, FeedConfig, Phase, Snapshot},
    intraday::IntradayFeed,
    quote::{Quote, normalize_symbol, price_decimals},
    search::{MAX_QUERY_CHARS, MAX_RESULTS, SearchSnapshot, StockSearch},
};

use crate::{
    platform,
    tray::{Action, Tray},
};

mod intraday_chart;

const TEXT: Color32 = Color32::from_rgb(236, 240, 245);
const MUTED: Color32 = Color32::from_rgb(139, 151, 168);
const GREEN: Color32 = Color32::from_rgb(92, 213, 174);
const RED: Color32 = Color32::from_rgb(255, 112, 126);
const AMBER: Color32 = Color32::from_rgb(235, 188, 111);

pub fn initial_height(settings: &Settings) -> f32 {
    180.0 + settings.symbols.len().clamp(1, 7) as f32 * if settings.compact { 46.0 } else { 66.0 }
}

fn symbol_to_add(
    input: &str,
    snapshot: &SearchSnapshot,
    selected: Option<usize>,
) -> Option<String> {
    let ready = !snapshot.loading
        && snapshot.error.is_none()
        && snapshot.query == input.trim().to_ascii_lowercase();
    if ready && let Some(item) = selected.and_then(|index| snapshot.matches.get(index)) {
        return Some(item.symbol.clone());
    }
    // Preserve code entry (especially 000001 -> Shenzhen) unless the user
    // explicitly chose a search result. Names use the highlighted first match.
    normalize_symbol(input).ok().or_else(|| {
        ready
            .then(|| snapshot.matches.first().map(|item| item.symbol.clone()))
            .flatten()
    })
}

pub struct StockApp {
    settings: Settings,
    store: ConfigStore,
    feed: Feed,
    intraday: IntradayFeed,
    chart_symbol: Option<String>,
    chart_request: Option<(Option<String>, bool)>,
    search: StockSearch,
    search_snapshot: SearchSnapshot,
    selected_match: Option<usize>,
    ime_composing: bool,
    tray: Option<Tray>,
    settings_open: bool,
    add_open: bool,
    focus_add: bool,
    input: String,
    input_error: Option<String>,
    warning: Option<String>,
    dirty_since: Option<Instant>,
    paused: bool,
    last_size: Vec2,
    drag_regions: Vec<(Rect, egui::LayerId)>,
    window_drag: Option<WindowDrag>,
    drag_released: bool,
}

struct WindowDrag {
    offset: Vec2,
    press_screen: Vec2,
    moved: bool,
}

impl StockApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        store: ConfigStore,
        warning: Option<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        configure_style(&cc.egui_ctx);
        load_chinese_font(&cc.egui_ctx);
        platform::remove_native_border(cc);
        platform::restore_position(cc, settings.position);
        let wake_ctx = cc.egui_ctx.clone();
        let feed = Feed::spawn(
            FeedConfig::from(&settings),
            Arc::new(move || wake_ctx.request_repaint()),
        )?;
        let search_ctx = cc.egui_ctx.clone();
        let search = StockSearch::spawn(Arc::new(move || search_ctx.request_repaint()))?;
        let chart_ctx = cc.egui_ctx.clone();
        let intraday = IntradayFeed::spawn(Arc::new(move || chart_ctx.request_repaint()))?;
        let tray = Tray::new(cc.egui_ctx.clone()).ok();
        let last_size = vec2(380.0, initial_height(&settings));
        Ok(Self {
            settings,
            store,
            feed,
            intraday,
            chart_symbol: None,
            chart_request: None,
            search,
            search_snapshot: SearchSnapshot::default(),
            selected_match: None,
            ime_composing: false,
            tray,
            settings_open: false,
            add_open: false,
            focus_add: false,
            input: String::new(),
            input_error: None,
            warning,
            dirty_since: None,
            paused: false,
            last_size,
            drag_regions: Vec::new(),
            window_drag: None,
            drag_released: false,
        })
    }

    fn changed(&mut self, network: bool) {
        self.dirty_since = Some(Instant::now());
        if network {
            self.reconnect();
        }
    }

    fn reconnect(&self) {
        let mut config = FeedConfig::from(&self.settings);
        config.paused = self.paused;
        self.feed.configure(config);
        self.intraday.request(
            self.chart_symbol.as_deref(),
            self.paused || self.settings_open,
        );
    }

    fn pin(&mut self, ctx: &egui::Context) {
        self.settings.always_on_top = !self.settings.always_on_top;
        self.apply_pin(ctx);
        self.changed(false);
    }

    fn apply_pin(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(
            if self.settings.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            },
        ));
    }

    fn save(&mut self) {
        match self.store.save(&self.settings) {
            Ok(()) => {
                self.dirty_since = None;
            }
            Err(error) => {
                self.warning = Some(format!("设置暂时无法保存：{error}"));
                self.dirty_since = None;
            }
        }
    }

    fn action(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Show => {
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
                self.apply_pin(ctx);
            }
            Action::Hide => {
                if self.tray.is_some() {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                } else {
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                }
            }
            Action::TogglePin => self.pin(ctx),
            Action::Reconnect => self.reconnect(),
            Action::Exit => {
                self.save();
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
        let control_width = 112.0;
        let drag_rect = Rect::from_min_max(
            rect.min,
            pos2(rect.right() - control_width - 4.0, rect.bottom()),
        );
        let drag = ui.interact(
            drag_rect,
            ui.id().with("header-drag"),
            Sense::click_and_drag(),
        );
        self.drag_regions.push((drag_rect, ui.layer_id()));
        if !cfg!(windows) && drag.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        drag.context_menu(|ui| {
            if ui.button("添加自选").clicked() {
                self.open_add();
                ui.close();
            }
            if ui
                .button(if self.settings.always_on_top {
                    "取消置顶"
                } else {
                    "置顶窗口"
                })
                .clicked()
            {
                self.pin(ui.ctx());
                ui.close();
            }
            if ui.button("设置").clicked() {
                self.settings_open = true;
                ui.close();
            }
            if ui.button("隐藏到托盘").clicked() {
                self.action(Action::Hide, ui.ctx());
                ui.close();
            }
            ui.separator();
            if ui.button("退出微行情").clicked() {
                self.action(Action::Exit, ui.ctx());
            }
        });
        let painter = ui.painter();
        let chart = [
            pos2(rect.left() + 1.0, rect.top() + 22.0),
            pos2(rect.left() + 8.0, rect.top() + 14.0),
            pos2(rect.left() + 14.0, rect.top() + 18.0),
            pos2(rect.left() + 23.0, rect.top() + 8.0),
        ];
        painter.add(egui::Shape::line(chart.to_vec(), Stroke::new(2.3, GREEN)));
        painter.text(
            pos2(rect.left() + 33.0, rect.center().y),
            Align2::LEFT_CENTER,
            if self.settings_open {
                "偏好设置"
            } else {
                "微行情"
            },
            FontId::proportional(18.0),
            TEXT,
        );
        if !self.settings_open {
            painter.text(
                pos2(rect.left() + 99.0, rect.center().y + 1.0),
                Align2::LEFT_CENTER,
                "MINI QUOTES",
                FontId::monospace(9.5),
                MUTED,
            );
        }
        let button_rect = |index: usize| {
            Rect::from_min_size(
                pos2(
                    rect.right() - control_width + index as f32 * 28.0,
                    rect.top() + 2.0,
                ),
                vec2(26.0, 28.0),
            )
        };
        if icon_button(
            ui,
            button_rect(0),
            Glyph::Pin,
            "置顶 / 取消置顶",
            self.settings.always_on_top,
        )
        .clicked()
        {
            self.pin(ui.ctx());
        }
        let glyph = if self.settings_open {
            Glyph::Back
        } else {
            Glyph::Settings
        };
        if icon_button(
            ui,
            button_rect(1),
            glyph,
            if self.settings_open {
                "返回行情"
            } else {
                "设置"
            },
            self.settings_open,
        )
        .clicked()
        {
            self.settings_open = !self.settings_open;
        }
        if icon_button(ui, button_rect(2), Glyph::Minimize, "隐藏到托盘", false).clicked() {
            self.action(Action::Hide, ui.ctx());
        }
        if icon_button(ui, button_rect(3), Glyph::Close, "退出微行情", false).clicked() {
            self.action(Action::Exit, ui.ctx());
        }
    }

    fn status(&self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        let (color, text) = match snapshot.phase {
            Phase::Streaming => (GREEN, "新浪 · 推送已连接"),
            Phase::Connecting => (AMBER, "正在连接行情"),
            Phase::Fallback => (
                AMBER,
                if self.settings.feed_mode == FeedMode::Tencent {
                    "腾讯 · 定时更新"
                } else {
                    "备用行情 · 正在重连"
                },
            ),
            Phase::Offline => (RED, "连接暂不可用 · 自动重试"),
            Phase::Paused => (MUTED, "已暂停更新"),
            Phase::Idle => (MUTED, "等待添加自选"),
        };
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
        ui.painter()
            .circle_filled(pos2(rect.left() + 4.0, rect.center().y), 3.0, color);
        ui.painter().text(
            pos2(rect.left() + 14.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(11.0),
            MUTED,
        );
        ui.painter().text(
            rect.right_center(),
            Align2::RIGHT_CENTER,
            format!("{} 只自选", self.settings.symbols.len()),
            FontId::proportional(11.0),
            MUTED,
        );
        response.on_hover_text(&snapshot.detail);
    }

    fn open_add(&mut self) {
        self.settings_open = false;
        self.chart_symbol = None;
        self.add_open = true;
        self.focus_add = true;
        self.refresh_search();
    }

    fn close_add(&mut self) {
        self.add_open = false;
        self.input_error = None;
        self.ime_composing = false;
        self.selected_match = None;
        self.search.request("");
        self.search_snapshot = self.search.latest();
    }

    fn refresh_search(&mut self) {
        self.input_error = None;
        self.selected_match = None;
        self.search
            .request(if self.ime_composing { "" } else { &self.input });
        self.search_snapshot = self.search.latest();
    }

    fn add_height(&self) -> f32 {
        if self.add_open {
            91.0 + if self.input.trim().is_empty() {
                0.0
            } else {
                144.0
            }
        } else {
            0.0
        }
    }

    fn add_symbol(&mut self, symbol: &str) {
        match normalize_symbol(symbol) {
            Err(error) => self.input_error = Some(error),
            Ok(symbol) if self.settings.symbols.contains(&symbol) => {
                self.input_error = Some("这只股票已经在自选列表中".into())
            }
            Ok(_) if self.settings.symbols.len() >= MAX_SYMBOLS => {
                self.input_error = Some(format!("最多同时关注 {MAX_SYMBOLS} 只股票"))
            }
            Ok(symbol) => {
                self.settings.symbols.push(symbol);
                self.input.clear();
                self.close_add();
                self.changed(true);
            }
        }
    }

    fn submit_symbol(&mut self) {
        if let Some(symbol) = symbol_to_add(&self.input, &self.search_snapshot, self.selected_match)
        {
            self.add_symbol(&symbol);
        } else if self.input.trim().is_empty() {
            self.input_error = Some("请输入股票名称、拼音首字母或代码".into());
        } else if !self.search_snapshot.loading {
            self.refresh_search();
        }
        self.focus_add = self.add_open;
    }

    fn add_form(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("添加自选").size(13.0).color(TEXT));
        let input_id = ui.make_persistent_id("stock-search-input");
        let mut ime_input = self.ime_composing;
        ui.input(|input| {
            for event in &input.events {
                match event {
                    egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                        self.ime_composing = !text.is_empty();
                        ime_input = true;
                    }
                    egui::Event::Ime(egui::ImeEvent::Commit(_)) => {
                        self.ime_composing = false;
                        ime_input = true;
                    }
                    _ => {}
                }
            }
        });
        let mut scroll_to_selected = false;
        if !ime_input
            && ui.memory(|memory| memory.has_focus(input_id))
            && !self.search_snapshot.matches.is_empty()
        {
            let count = self.search_snapshot.matches.len();
            let selected = self
                .selected_match
                .or_else(|| normalize_symbol(&self.input).is_err().then_some(0));
            if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
            {
                self.selected_match = Some(selected.map_or(0, |index| (index + 1) % count));
                scroll_to_selected = true;
            }
            if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                self.selected_match =
                    Some(selected.map_or(count - 1, |index| (index + count - 1) % count));
                scroll_to_selected = true;
            }
        }
        ui.horizontal(|ui| {
            let input = ui.add_sized(
                [ui.available_width() - 65.0, 29.0],
                egui::TextEdit::singleline(&mut self.input)
                    .id(input_id)
                    .hint_text("名称 / 拼音首字母 / 代码")
                    .char_limit(MAX_QUERY_CHARS)
                    .font(FontId::proportional(13.0)),
            );
            if input.changed() {
                self.refresh_search();
            }
            if self.focus_add {
                input.request_focus();
                self.focus_add = false;
            }
            let enter = !ime_input
                && (input.has_focus() || input.lost_focus())
                && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if ime_input && input.lost_focus() {
                input.request_focus();
            }
            let can_add =
                symbol_to_add(&self.input, &self.search_snapshot, self.selected_match).is_some();
            if ui
                .add_sized(
                    [57.0, 29.0],
                    egui::Button::new(if can_add { "添加" } else { "搜索" })
                        .fill(Color32::from_rgb(39, 89, 76)),
                )
                .clicked()
                || enter
            {
                self.submit_symbol();
                if self.add_open {
                    input.request_focus();
                }
            }
        });
        if !self.add_open {
            return;
        }
        if let Some(error) = self.input_error.as_ref() {
            ui.label(RichText::new(error).size(11.0).color(AMBER));
        } else {
            ui.label(
                RichText::new("支持沪深 / 港股 · 如茅台、腾讯、00700")
                    .size(10.5)
                    .color(MUTED),
            );
        }
        if !self.input.trim().is_empty() {
            self.search_results(ui, scroll_to_selected);
        }
    }

    fn search_results(&mut self, ui: &mut egui::Ui, scroll_to_selected: bool) {
        let snapshot = &self.search_snapshot;
        let mut clicked = None;
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 140.0),
            Layout::top_down(Align::Min),
            |ui| {
                if self.ime_composing {
                    ui.label(
                        RichText::new("输入完成后显示匹配结果")
                            .size(11.0)
                            .color(MUTED),
                    );
                } else if snapshot.loading {
                    ui.label(RichText::new("正在搜索…").size(11.0).color(MUTED));
                } else if let Some(error) = &snapshot.error {
                    ui.label(RichText::new(error).size(11.0).color(AMBER));
                    if ui.small_button("重试搜索").clicked() {
                        self.search.request(&self.input);
                    }
                } else if snapshot.matches.is_empty() {
                    ui.label(
                        RichText::new("未找到匹配项，请换个名称或输入完整代码")
                            .size(11.0)
                            .color(MUTED),
                    );
                } else {
                    let caption = if snapshot.matches.len() == MAX_RESULTS {
                        format!("显示前 {MAX_RESULTS} 项 · 继续输入可缩小范围")
                    } else {
                        format!(
                            "{} 项匹配 · 点击添加 · ↑↓ 选择 · Enter 确认",
                            snapshot.matches.len()
                        )
                    };
                    ui.label(RichText::new(caption).size(10.0).color(MUTED));
                    let selected = self
                        .selected_match
                        .or_else(|| normalize_symbol(&self.input).is_err().then_some(0));
                    egui::ScrollArea::vertical()
                        .id_salt(("stock-search-results", &snapshot.query))
                        .max_height(120.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (index, item) in snapshot.matches.iter().enumerate() {
                                let added = self.settings.symbols.contains(&item.symbol);
                                let label = format!(
                                    "{} {}  {}{}",
                                    item.symbol[..2].to_ascii_uppercase(),
                                    &item.symbol[2..],
                                    item.name,
                                    if added { " · 已添加" } else { "" },
                                );
                                let response = ui
                                    .add_enabled_ui(!added, |ui| {
                                        ui.add_sized(
                                            [ui.available_width(), 26.0],
                                            egui::Button::new(RichText::new(&label).size(12.0))
                                                .selected(selected == Some(index))
                                                .truncate(),
                                        )
                                    })
                                    .inner
                                    .on_hover_text(&label);
                                if response.clicked() {
                                    clicked = Some(item.symbol.clone());
                                }
                                if scroll_to_selected && selected == Some(index) {
                                    response.scroll_to_me(Some(Align::Center));
                                }
                            }
                        });
                }
            },
        );
        if let Some(symbol) = clicked {
            self.add_symbol(&symbol);
        }
    }

    fn watchlist(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        let add_height = self.add_height();
        let height = (ui.available_height() - 64.0 - add_height).max(45.0);
        self.drag_regions.push((
            Rect::from_min_size(ui.cursor().min, vec2(ui.available_width() - 12.0, height)),
            ui.layer_id(),
        ));
        let symbols = self.settings.symbols.clone();
        egui::ScrollArea::vertical()
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if symbols.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("让关心的行情，留在眼前。")
                            .size(16.0)
                            .color(TEXT),
                    );
                    ui.label(
                        RichText::new("输入股票名称或代码即可开始")
                            .size(12.0)
                            .color(MUTED),
                    );
                }
                for (index, symbol) in symbols.iter().enumerate() {
                    let quote = snapshot.quotes.get(symbol);
                    let error = snapshot.errors.get(symbol);
                    let response = quote_row(
                        ui,
                        symbol,
                        quote,
                        error,
                        self.settings.compact,
                        snapshot.phase,
                    );
                    if !cfg!(windows) && response.drag_started_by(egui::PointerButton::Primary) {
                        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
                    }
                    if response.clicked()
                        && !self.window_drag.as_ref().is_some_and(|drag| drag.moved)
                    {
                        self.chart_symbol = Some(symbol.clone());
                        self.close_add();
                    }
                    response.context_menu(|ui| {
                        ui.label(quote.map(|q| q.name.as_str()).unwrap_or(symbol));
                        ui.separator();
                        if ui.button("查看分时走势").clicked() {
                            self.chart_symbol = Some(symbol.clone());
                            self.close_add();
                            ui.close();
                        }
                        if ui
                            .add_enabled(index > 0, egui::Button::new("上移"))
                            .clicked()
                        {
                            self.settings.symbols.swap(index, index - 1);
                            self.changed(false);
                            ui.close();
                        }
                        if ui
                            .add_enabled(index + 1 < symbols.len(), egui::Button::new("下移"))
                            .clicked()
                        {
                            self.settings.symbols.swap(index, index + 1);
                            self.changed(false);
                            ui.close();
                        }
                        if ui.button(RichText::new("移除自选").color(RED)).clicked() {
                            self.settings.symbols.retain(|s| s != symbol);
                            self.changed(true);
                            ui.close();
                        }
                    });
                }
            });
        if self.add_open {
            self.add_form(ui);
        }
        ui.add_space(7.0);
        ui.horizontal(|ui| {
            let caption = if self.add_open {
                "收起添加"
            } else {
                "＋  添加自选"
            };
            if ui
                .add_sized(
                    [ui.available_width() - 34.0, 27.0],
                    egui::Button::new(RichText::new(caption).size(12.0).color(MUTED))
                        .fill(Color32::from_white_alpha(5)),
                )
                .clicked()
            {
                if self.add_open {
                    self.close_add();
                } else {
                    self.open_add();
                }
            }
            let (rect, _) = ui.allocate_exact_size(vec2(26.0, 27.0), Sense::hover());
            if icon_button(ui, rect, Glyph::Refresh, "重新连接行情 (Ctrl+R)", false).clicked()
            {
                self.reconnect();
            }
        });
        ui.add_space(5.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("点击看分时 · 拖动移动 · 右键管理")
                    .size(10.0)
                    .color(MUTED),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let latest = snapshot.quotes.values().map(|quote| quote.quote_time).max();
                let stamp = latest
                    .map(|time| time.format("%H:%M:%S").to_string())
                    .unwrap_or_else(|| "--:--:--".into());
                ui.label(RichText::new(stamp).monospace().size(10.0).color(MUTED))
                    .on_hover_text("最近一条行情的源时间；各股票时间见悬停详情");
            });
        });
    }

    fn intraday_panel(&mut self, ui: &mut egui::Ui, symbol: &str, quotes: &Snapshot) {
        let snapshot = self.intraday.latest();
        let series = snapshot
            .series
            .as_deref()
            .filter(|series| series.symbol == symbol);
        ui.horizontal(|ui| {
            if ui.button("‹ 自选列表").clicked() {
                self.chart_symbol = None;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let index = self
                    .settings
                    .symbols
                    .iter()
                    .position(|item| item == symbol)
                    .unwrap_or(0);
                if ui
                    .add_enabled(
                        index + 1 < self.settings.symbols.len(),
                        egui::Button::new("下一只 ›"),
                    )
                    .clicked()
                {
                    self.chart_symbol = self.settings.symbols.get(index + 1).cloned();
                }
                if ui
                    .add_enabled(index > 0, egui::Button::new("‹ 上一只"))
                    .clicked()
                {
                    self.chart_symbol = self.settings.symbols.get(index - 1).cloned();
                }
            });
        });
        ui.add_space(7.0);
        let quote = quotes.quotes.get(symbol);
        let name = series
            .map(|s| s.name.as_str())
            .or_else(|| quote.map(|q| q.name.as_str()))
            .unwrap_or(symbol);
        ui.add(egui::Label::new(RichText::new(name).size(17.0).strong()).truncate())
            .on_hover_text(name);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} {} · 分时",
                    symbol[..2].to_uppercase(),
                    &symbol[2..]
                ))
                .size(10.5)
                .color(MUTED),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(series) = series {
                    ui.label(
                        RichText::new(series.date.format("%Y-%m-%d").to_string())
                            .monospace()
                            .size(10.5)
                            .color(MUTED),
                    );
                }
            });
        });
        let decimals = price_decimals(symbol);
        let price = series.and_then(|s| s.points.last()).map(|p| p.price);
        let previous = series.and_then(|s| s.previous_close);
        let change = price
            .zip(previous)
            .map(|(price, previous)| (price / previous - 1.0) * 100.0);
        let color = match change {
            Some(value) if value > 0.0 => RED,
            Some(value) if value < 0.0 => GREEN,
            _ => MUTED,
        };
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(
                    price
                        .map(|price| format!("{price:.decimals$}"))
                        .unwrap_or_else(|| "—".into()),
                )
                .monospace()
                .size(25.0)
                .color(color),
            );
            ui.label(
                RichText::new(
                    change
                        .map(|change| format!("{change:+.2}%"))
                        .unwrap_or_else(|| "—".into()),
                )
                .monospace()
                .size(13.0)
                .color(color),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(
                        previous
                            .map(|p| format!("昨收 {p:.decimals$}"))
                            .unwrap_or_else(|| "昨收 —".into()),
                    )
                    .size(10.5)
                    .color(MUTED),
                );
            });
        });
        ui.add_space(4.0);
        if let Some(series) = series {
            intraday_chart::show(ui, series);
        } else {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 216.0), Sense::hover());
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                if self.paused {
                    "已暂停更新"
                } else if snapshot.loading || snapshot.symbol.as_deref() != Some(symbol) {
                    "正在加载分时走势…"
                } else {
                    "暂时无法显示分时走势"
                },
                FontId::proportional(13.0),
                MUTED,
            );
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let label = if self.paused {
                "已暂停更新"
            } else if snapshot.loading {
                "正在刷新…"
            } else {
                "腾讯分时 · 盘中约 15 秒刷新"
            };
            ui.label(RichText::new(label).size(10.5).color(MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(!self.paused && !snapshot.loading, egui::Button::new("刷新"))
                    .clicked()
                {
                    self.intraday.request(Some(symbol), false);
                }
            });
        });
        if let Some(error) = &snapshot.error {
            ui.label(
                RichText::new(format!(
                    "{error}{}",
                    if series.is_some() {
                        " · 显示上次数据"
                    } else {
                        ""
                    }
                ))
                .size(10.5)
                .color(AMBER),
            );
        } else {
            let today = Utc::now()
                .with_timezone(&FixedOffset::east_opt(28800).expect("UTC+8"))
                .date_naive();
            ui.label(
                RichText::new(if series.is_some_and(|s| s.date < today) {
                    "显示最近交易日数据 · 以图中日期为准"
                } else if symbol.starts_with("hk") {
                    "原币报价 · 午休已折叠 · 含收市竞价"
                } else {
                    "午休已折叠 · 虚线为昨收 · Esc 返回"
                })
                .size(10.0)
                .color(MUTED),
            );
        }
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        ui.add_space(6.0);
        ui.label(RichText::new("窗口外观").size(15.0).strong());
        ui.add_space(5.0);
        if ui
            .checkbox(&mut self.settings.always_on_top, "始终置顶")
            .changed()
        {
            self.apply_pin(ui.ctx());
            self.changed(false);
        }
        if ui
            .checkbox(&mut self.settings.compact, "紧凑显示")
            .changed()
        {
            self.changed(false);
        }
        ui.add_space(8.0);
        ui.label(RichText::new("背景不透明度").size(12.0).color(MUTED));
        let mut percent = self.settings.opacity * 100.0;
        if ui
            .add(
                egui::Slider::new(&mut percent, 0.0..=100.0)
                    .suffix(" %")
                    .integer(),
            )
            .changed()
        {
            self.settings.opacity = percent / 100.0;
            self.changed(false);
        }
        ui.label(
            RichText::new("0% 为全透明，文字保持清晰可见")
                .size(11.0)
                .color(MUTED),
        );
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(RichText::new("行情更新").size(15.0).strong());
        ui.add_space(5.0);
        let previous_mode = self.settings.feed_mode;
        ui.radio_value(
            &mut self.settings.feed_mode,
            FeedMode::Auto,
            "自动 · 新浪推送优先",
        );
        ui.radio_value(
            &mut self.settings.feed_mode,
            FeedMode::Tencent,
            "腾讯 · 定时刷新",
        );
        if previous_mode != self.settings.feed_mode {
            self.changed(true);
        }
        ui.add_space(8.0);
        let refresh_slider = ui.add(
            egui::Slider::new(&mut self.settings.poll_seconds, 2..=60)
                .text("刷新间隔")
                .suffix(" 秒"),
        );
        if refresh_slider.changed() {
            self.changed(false);
        }
        if refresh_slider.drag_stopped() || (refresh_slider.changed() && !refresh_slider.dragged())
        {
            self.reconnect();
        }
        ui.label(
            RichText::new("用于定时刷新和备用行情；休市时降为 60 秒")
                .size(10.5)
                .color(MUTED),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui
                .button(if self.paused {
                    "恢复更新"
                } else {
                    "暂停更新"
                })
                .clicked()
            {
                self.paused = !self.paused;
                self.reconnect();
            }
            if ui.button("重新连接").clicked() {
                self.reconnect();
            }
        });
        ui.add_space(8.0);
        egui::CollapsingHeader::new("连接详情").show(ui, |ui| {
            ui.label(RichText::new(&snapshot.detail).size(11.0).color(MUTED));
            if let Some(time) = snapshot.last_received {
                let china = time.with_timezone(&FixedOffset::east_opt(28800).expect("UTC+8"));
                ui.label(
                    RichText::new(format!("最近接收 {}", china.format("%m-%d %H:%M:%S")))
                        .size(11.0)
                        .color(MUTED),
                );
            }
            ui.label(
                RichText::new("连接状态不代表行情时效，请留意每只股票的行情时间。")
                    .size(11.0)
                    .color(MUTED),
            );
        });
        ui.add_space(10.0);
        ui.label(
            RichText::new("设置自动保存 · Ctrl+, 返回 · Ctrl+R 重连")
                .size(10.5)
                .color(MUTED),
        );
    }
}

impl eframe::App for StockApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if !cfg!(windows) {
            return;
        }
        if !input.focused {
            self.window_drag = None;
            self.drag_released = false;
            return;
        }
        // Windows native move loops can miss a fast gesture when mouse-down and
        // mouse-up are delivered in one egui frame. Retain the raw press event
        // and move using physical screen coordinates instead of a late StartDrag.
        for event in &input.events {
            if let egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                ..
            } = event
            {
                if *pressed {
                    let allowed = self.drag_regions.iter().any(|(rect, layer)| {
                        rect.contains(*pos) && ctx.layer_id_at(*pos) == Some(*layer)
                    });
                    if allowed && let Some([x, y]) = self.settings.position {
                        let offset = pos.to_vec2() * ctx.pixels_per_point();
                        self.window_drag = Some(WindowDrag {
                            offset,
                            press_screen: vec2(x as f32, y as f32) + offset,
                            moved: false,
                        });
                        self.drag_released = false;
                    }
                } else if self.window_drag.is_some() {
                    self.drag_released = true;
                }
            }
        }
    }

    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let events: Vec<_> = self
            .tray
            .as_ref()
            .map(|tray| tray.events.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            self.action(event, ctx);
        }
        if let Some(position) = platform::position(frame)
            && self.settings.position != Some(position)
        {
            self.settings.position = Some(position);
            self.changed(false);
        }
        if let Some(since) = self.dirty_since {
            if since.elapsed() >= Duration::from_millis(700) {
                self.save();
            } else {
                ctx.request_repaint_after(Duration::from_millis(750));
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(drag) = self.window_drag.as_mut()
            && let Some([x, y]) = platform::cursor_position()
        {
            let pointer = vec2(x as f32, y as f32);
            if drag.moved || (pointer - drag.press_screen).length() > 3.0 {
                drag.moved = true;
                let position = (pointer - drag.offset) / ctx.pixels_per_point();
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(position.to_pos2()));
            }
        }
        self.drag_regions.clear();
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Comma)) {
            self.settings_open = !self.settings_open;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::R)) {
            self.reconnect();
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Q)) {
            self.action(Action::Exit, &ctx);
        }
        if !self.ime_composing
            && !ctx.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Ime(_)))
            })
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            if self.settings_open {
                self.settings_open = false;
            } else {
                self.chart_symbol = None;
            }
            self.close_add();
        }

        self.search_snapshot = self.search.latest();
        let warning_height = if self.warning.is_some() { 34.0 } else { 0.0 };
        let desired_height = if self.settings_open {
            566.0
        } else if self.chart_symbol.is_some() {
            500.0
        } else {
            initial_height(&self.settings) + self.add_height()
        } + warning_height;
        let desired = vec2(380.0, desired_height);
        if self.last_size != desired {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(desired));
            self.last_size = desired;
        }
        let snapshot = self.feed.latest();
        let alpha = (self.settings.opacity * 255.0).round() as u8;
        egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(17, 23, 34, alpha))
            .stroke(Stroke::new(
                1.0,
                Color32::from_white_alpha((self.settings.opacity * 30.0) as u8),
            ))
            .corner_radius(12)
            .inner_margin(16)
            .show(ui, |ui| {
                ui.set_min_size(desired - vec2(34.0, 34.0));
                let background_drag = ui.interact(
                    ui.max_rect(),
                    ui.id().with("background-drag"),
                    Sense::drag(),
                );
                self.header(ui);
                ui.add_space(7.0);
                self.status(ui, &snapshot);
                ui.add_space(9.0);
                if let Some(warning) = self.warning.clone() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(warning).size(10.5).color(AMBER));
                        if ui.small_button("知道了").clicked() {
                            self.warning = None;
                        }
                    });
                }
                if self.settings_open {
                    self.settings_panel(ui, &snapshot);
                } else if let Some(symbol) = self.chart_symbol.clone() {
                    self.intraday_panel(ui, &symbol, &snapshot);
                } else {
                    self.watchlist(ui, &snapshot);
                }
                if !cfg!(windows) && background_drag.drag_started_by(egui::PointerButton::Primary) {
                    ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
                }
            });
        if self.drag_released {
            self.window_drag = None;
            self.drag_released = false;
        }
        let request = (self.chart_symbol.clone(), self.paused || self.settings_open);
        if self.chart_request.as_ref() != Some(&request) {
            self.intraday.request(request.0.as_deref(), request.1);
            self.chart_request = Some(request);
            ctx.request_repaint();
        }
        ctx.request_repaint_after(Duration::from_secs(30));
    }

    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        self.save();
    }
}

fn quote_row(
    ui: &mut egui::Ui,
    symbol: &str,
    quote: Option<&Quote>,
    error: Option<&String>,
    compact: bool,
    phase: Phase,
) -> egui::Response {
    let height = if compact { 42.0 } else { 62.0 };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, 7, Color32::from_white_alpha(8));
    }
    let color = match quote.and_then(Quote::change) {
        Some(change) if change > 0.0 => RED,
        Some(change) if change < 0.0 => GREEN,
        _ => MUTED,
    };
    let name = quote
        .map(|q| q.name.as_str())
        .unwrap_or(if error.is_some() {
            "未找到行情"
        } else if matches!(phase, Phase::Connecting) {
            "等待行情"
        } else {
            "暂无行情"
        });
    let top = rect.top() + if compact { 12.0 } else { 17.0 };
    let price = quote
        .and_then(|quote| {
            quote
                .price
                .map(|price| format!("{:.*}", quote.decimals(), price))
        })
        .unwrap_or_else(|| "—".into());
    let percent = quote
        .and_then(Quote::change_percent)
        .map(|change| format!("{change:+.2}%"))
        .unwrap_or_else(|| "—".into());
    painter.text(
        pos2(rect.left() + 6.0, top),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(if compact { 13.0 } else { 15.0 }),
        TEXT,
    );
    painter.text(
        pos2(rect.right() - 109.0, top),
        Align2::RIGHT_CENTER,
        price,
        FontId::monospace(if compact { 16.0 } else { 20.0 }),
        color,
    );
    let badge = Rect::from_center_size(
        pos2(rect.right() - 50.0, top),
        vec2(88.0, if compact { 22.0 } else { 26.0 }),
    );
    painter.rect_filled(badge, 5, color.gamma_multiply(0.13));
    painter.text(
        badge.center(),
        Align2::CENTER_CENTER,
        &percent,
        FontId::monospace(13.0),
        color,
    );
    let lower = rect.top() + if compact { 31.0 } else { 43.0 };
    painter.text(
        pos2(rect.left() + 6.0, lower),
        Align2::LEFT_CENTER,
        format!("{} {}", symbol[..2].to_uppercase(), &symbol[2..]),
        FontId::monospace(10.0),
        MUTED,
    );
    if !compact {
        let change = quote
            .and_then(|quote| {
                quote
                    .change()
                    .map(|change| format!("{change:+.*}", quote.decimals()))
            })
            .unwrap_or_else(|| "—".into());
        painter.text(
            pos2(rect.right() - 109.0, lower),
            Align2::RIGHT_CENTER,
            change,
            FontId::monospace(10.5),
            MUTED,
        );
    }
    let time = quote
        .map(|q| {
            let today = Utc::now().with_timezone(q.quote_time.offset()).date_naive();
            q.quote_time
                .format(if q.quote_time.date_naive() == today {
                    "%H:%M:%S"
                } else {
                    "%m-%d %H:%M"
                })
                .to_string()
        })
        .unwrap_or_else(|| "等待更新".into());
    painter.text(
        pos2(rect.right() - 6.0, lower),
        Align2::RIGHT_CENTER,
        time,
        FontId::monospace(10.0),
        MUTED,
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Label,
            true,
            format!("{name} {symbol} {percent}"),
        )
    });
    let tooltip = if let Some(quote) = quote {
        let age = (Utc::now() - quote.quote_time.with_timezone(&Utc))
            .num_seconds()
            .max(0);
        let age_text = if age < 60 {
            format!("{age} 秒前")
        } else if age < 3600 {
            format!("{} 分钟前", age / 60)
        } else {
            format!("{} 小时前", age / 3600)
        };
        let value = |number: Option<f64>| {
            number
                .map(|v| format!("{:.*}", quote.decimals(), v))
                .unwrap_or_else(|| "—".into())
        };
        format!(
            "{} · {}\n{}\n行情时间：{}（{}）\n昨收 {}    今开 {}\n最高 {}    最低 {}\n{}\n点击查看分时；右键可调整顺序或移除",
            quote.name,
            symbol,
            if symbol.starts_with("hk") {
                "港股 · 原币报价（未作汇率换算）"
            } else {
                "沪深 · 原币报价"
            },
            quote.quote_time.format("%Y-%m-%d %H:%M:%S"),
            age_text,
            value(quote.previous_close),
            value(quote.open),
            value(quote.high),
            value(quote.low),
            quote.source.label()
        )
    } else {
        error
            .cloned()
            .unwrap_or_else(|| "正在等待有效行情；代码无效或网络中断时不会显示虚构价格。".into())
    };
    response.on_hover_text(tooltip)
}

#[derive(Clone, Copy)]
enum Glyph {
    Pin,
    Settings,
    Close,
    Minimize,
    Back,
    Refresh,
}

fn icon_button(
    ui: &mut egui::Ui,
    rect: Rect,
    glyph: Glyph,
    label: &str,
    active: bool,
) -> egui::Response {
    let response = ui.interact(rect, ui.id().with(label), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let painter = ui.painter();
    if response.hovered() || active {
        painter.rect_filled(
            rect,
            5,
            Color32::from_white_alpha(if active { 12 } else { 8 }),
        );
    }
    let color = if active {
        GREEN
    } else if response.hovered() {
        TEXT
    } else {
        MUTED
    };
    let stroke = Stroke::new(1.4, color);
    let c = rect.center();
    let line = |a: [f32; 2], b: [f32; 2]| {
        painter.line_segment([c + vec2(a[0], a[1]), c + vec2(b[0], b[1])], stroke);
    };
    match glyph {
        Glyph::Close => {
            line([-4.0, -4.0], [4.0, 4.0]);
            line([-4.0, 4.0], [4.0, -4.0]);
        }
        Glyph::Minimize => line([-5.0, 3.0], [5.0, 3.0]),
        Glyph::Back => {
            line([-4.0, 0.0], [5.0, 0.0]);
            line([-4.0, 0.0], [0.0, -4.0]);
            line([-4.0, 0.0], [0.0, 4.0]);
        }
        Glyph::Pin => {
            line([-3.0, -6.0], [3.0, -6.0]);
            line([-2.0, -6.0], [-2.0, 0.0]);
            line([2.0, -6.0], [2.0, 0.0]);
            line([-2.0, 0.0], [-5.0, 3.0]);
            line([2.0, 0.0], [5.0, 3.0]);
            line([-5.0, 3.0], [5.0, 3.0]);
            line([0.0, 3.0], [0.0, 7.0]);
        }
        Glyph::Settings => {
            painter.circle_stroke(c, 4.0, stroke);
            painter.circle_filled(c, 1.3, color);
            for angle in 0..8 {
                let theta = angle as f32 * std::f32::consts::FRAC_PI_4;
                let direction = vec2(theta.cos(), theta.sin());
                painter.line_segment([c + direction * 5.0, c + direction * 7.0], stroke);
            }
        }
        Glyph::Refresh => {
            let points: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let a = i as f32 / 24.0 * 5.0 + 0.4;
                    c + vec2(a.cos(), a.sin()) * 5.5
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
            line([5.0, -5.0], [5.0, 0.0]);
            line([5.0, 0.0], [0.0, 0.0]);
        }
    }
    response
        .on_hover_text(label)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn configure_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals(egui::Visuals::dark());
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = vec2(8.0, 4.0);
        style.spacing.button_padding = vec2(10.0, 5.0);
        style.spacing.slider_width = 190.0;
        style.visuals.override_text_color = Some(TEXT);
        style.visuals.panel_fill = Color32::TRANSPARENT;
        style.visuals.window_fill = Color32::from_rgb(25, 33, 46);
        style.visuals.extreme_bg_color = Color32::from_rgb(12, 18, 27);
        style.visuals.selection.bg_fill = Color32::from_rgb(39, 89, 76);
        style.visuals.widgets.inactive.corner_radius = CornerRadius::same(5);
        style.visuals.widgets.hovered.corner_radius = CornerRadius::same(5);
        style.visuals.widgets.active.corner_radius = CornerRadius::same(5);
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(12.0));
    });
}

fn load_chinese_font(ctx: &egui::Context) {
    let windows_dir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    let candidates = [
        std::path::PathBuf::from(&windows_dir).join("Fonts/msyh.ttc"),
        std::path::PathBuf::from(&windows_dir).join("Fonts/simhei.ttf"),
        "/System/Library/Fonts/PingFang.ttc".into(),
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into(),
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "chinese".into(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            // Keep built-in Latin fonts first for compact, crisp prices; fall back to CJK.
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("chinese".into());
            }
            ctx.set_fonts(fonts);
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mini_stock_monitor::search::StockMatch;

    fn matches_for(query: &str) -> SearchSnapshot {
        let mut snapshot = SearchSnapshot::default();
        snapshot.query = query.into();
        snapshot.matches = vec![
            StockMatch {
                symbol: "sh000001".into(),
                name: "上证指数".into(),
            },
            StockMatch {
                symbol: "sz000001".into(),
                name: "平安银行".into(),
            },
        ];
        snapshot
    }

    #[test]
    fn code_entry_keeps_its_market_unless_a_result_is_explicitly_selected() {
        let snapshot = matches_for("000001");
        assert_eq!(
            symbol_to_add("000001", &snapshot, None).as_deref(),
            Some("sz000001")
        );
        assert_eq!(
            symbol_to_add("000001", &snapshot, Some(0)).as_deref(),
            Some("sh000001")
        );
        assert_eq!(
            symbol_to_add("600519.SH", &SearchSnapshot::default(), None).as_deref(),
            Some("sh600519")
        );
    }

    #[test]
    fn name_entry_uses_current_results_and_keyboard_selection() {
        let snapshot = matches_for("银行");
        assert_eq!(
            symbol_to_add(" 银行 ", &snapshot, None).as_deref(),
            Some("sh000001")
        );
        assert_eq!(
            symbol_to_add("银行", &snapshot, Some(1)).as_deref(),
            Some("sz000001")
        );
        assert_eq!(symbol_to_add("茅台", &snapshot, Some(0)), None);
    }

    #[test]
    fn unfinished_or_failed_search_cannot_add_a_name_but_codes_still_work() {
        let mut snapshot = matches_for("银行");
        snapshot.loading = true;
        assert_eq!(symbol_to_add("银行", &snapshot, Some(0)), None);
        snapshot.loading = false;
        snapshot.error = Some("连接失败".into());
        assert_eq!(symbol_to_add("银行", &snapshot, Some(0)), None);
        assert_eq!(
            symbol_to_add("sz000001", &snapshot, None).as_deref(),
            Some("sz000001")
        );
    }

    #[test]
    fn hk_codes_and_name_matches_resolve_to_the_same_watchlist_symbol() {
        let mut snapshot = SearchSnapshot::default();
        snapshot.query = "腾讯".into();
        snapshot.matches = vec![StockMatch {
            symbol: "hk00700".into(),
            name: "腾讯控股".into(),
        }];
        assert_eq!(
            symbol_to_add("腾讯", &snapshot, None).as_deref(),
            Some("hk00700")
        );
        for code in ["00700", "hk00700", "700.HK"] {
            assert_eq!(
                symbol_to_add(code, &SearchSnapshot::default(), None).as_deref(),
                Some("hk00700")
            );
        }
    }
}
