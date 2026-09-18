use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, pos2, vec2};
use mini_stock_monitor::{
    intraday::{IntradaySeries, session_position},
    quote::price_decimals,
};

use super::{GREEN, MUTED, RED, TEXT};

pub(super) fn show(ui: &mut egui::Ui, series: &IntradaySeries) {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 216.0), Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Label,
            true,
            "分时走势图，鼠标悬停查看分钟价格",
        )
    });
    let plot = Rect::from_min_max(rect.min + vec2(57.0, 31.0), rect.max - vec2(49.0, 23.0));
    let painter = ui.painter();
    let (low, high) = series.price_range();
    let decimals = price_decimals(&series.symbol);
    let y = |price: f64| plot.bottom() - ((price - low) / (high - low)) as f32 * plot.height();
    let x = |minute| {
        plot.left() + session_position(&series.symbol, minute).unwrap_or(0.0) * plot.width()
    };
    let change_color = |price| match series.previous_close {
        Some(previous) if price > previous => RED,
        Some(previous) if price < previous => GREEN,
        _ => MUTED,
    };
    for step in 0..=4 {
        let price = high - (high - low) * f64::from(step) / 4.0;
        let line_y = y(price);
        painter.hline(
            plot.x_range(),
            line_y,
            Stroke::new(0.5, Color32::from_white_alpha(22)),
        );
        painter.text(
            pos2(plot.left() - 6.0, line_y),
            Align2::RIGHT_CENTER,
            format!("{price:.decimals$}"),
            FontId::monospace(9.5),
            change_color(price),
        );
        if let Some(previous) = series.previous_close {
            painter.text(
                pos2(plot.right() + 5.0, line_y),
                Align2::LEFT_CENTER,
                format!("{:+.2}%", (price / previous - 1.0) * 100.0),
                FontId::monospace(9.0),
                change_color(price),
            );
        }
    }
    if let Some(previous) = series.previous_close {
        dashed_line(
            painter,
            pos2(plot.left(), y(previous)),
            pos2(plot.right(), y(previous)),
            MUTED,
        );
    }
    let hk = series.symbol.starts_with("hk");
    let noon = if hk { 720 } else { 690 };
    let close = if hk { 970 } else { 900 };
    for (minute, label, align) in [
        (570, "09:30", Align2::LEFT_TOP),
        (
            noon,
            if hk { "12/13" } else { "11:30/13" },
            Align2::CENTER_TOP,
        ),
        (close, if hk { "16:10" } else { "15:00" }, Align2::RIGHT_TOP),
    ] {
        painter.vline(
            x(minute),
            plot.y_range(),
            Stroke::new(0.5, Color32::from_white_alpha(20)),
        );
        painter.text(
            pos2(x(minute), plot.bottom() + 7.0),
            align,
            label,
            FontId::monospace(9.5),
            MUTED,
        );
    }
    if hk {
        let auction = Rect::from_min_max(pos2(x(960), plot.top()), plot.max);
        painter.rect_filled(auction, 0, Color32::from_white_alpha(6));
    }
    if series.points.is_empty() {
        painter.text(
            plot.center(),
            Align2::CENTER_CENTER,
            "暂无分钟成交数据",
            FontId::proportional(12.0),
            MUTED,
        );
        return;
    }
    let last = series.points.last().expect("nonempty");
    let color = change_color(last.price);
    let points: Vec<_> = series
        .points
        .iter()
        .map(|p| pos2(x(p.minute), y(p.price)))
        .collect();
    if points.len() > 1 {
        // Clip to the plot so future rendering changes cannot overwrite labels.
        painter
            .with_clip_rect(plot.expand(1.0))
            .add(egui::Shape::line(points, Stroke::new(1.5, color)));
    }
    painter.circle_filled(pos2(x(last.minute), y(last.price)), 2.5, color);
    let hovered = response
        .hover_pos()
        .filter(|pos| plot.contains(*pos))
        .and_then(|pos| {
            series.points.iter().min_by(|a, b| {
                (x(a.minute) - pos.x)
                    .abs()
                    .total_cmp(&(x(b.minute) - pos.x).abs())
            })
        });
    let current = hovered.unwrap_or(last);
    if hovered.is_some() {
        dashed_line(
            painter,
            pos2(x(current.minute), plot.top()),
            pos2(x(current.minute), plot.bottom()),
            MUTED,
        );
        dashed_line(
            painter,
            pos2(plot.left(), y(current.price)),
            pos2(plot.right(), y(current.price)),
            MUTED,
        );
        painter.circle_filled(pos2(x(current.minute), y(current.price)), 3.5, TEXT);
    }
    let percent = series
        .previous_close
        .map(|previous| format!("  {:+.2}%", (current.price / previous - 1.0) * 100.0))
        .unwrap_or_default();
    painter.text(
        pos2(rect.left(), rect.top() + 10.0),
        Align2::LEFT_CENTER,
        format!(
            "{}  {:.decimals$}{percent}",
            current.time_label(),
            current.price
        ),
        FontId::monospace(11.0),
        TEXT,
    );
    painter.text(
        pos2(rect.right(), rect.top() + 10.0),
        Align2::RIGHT_CENTER,
        if hovered.is_some() {
            "分钟价格"
        } else {
            "悬停查看"
        },
        FontId::proportional(10.0),
        MUTED,
    );
}

fn dashed_line(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, color: Color32) {
    let delta = to - from;
    let length = delta.length();
    let direction = delta.normalized();
    let mut offset = 0.0;
    while offset < length {
        painter.line_segment(
            [
                from + direction * offset,
                from + direction * (offset + 3.0).min(length),
            ],
            Stroke::new(0.7, color),
        );
        offset += 6.0;
    }
}
