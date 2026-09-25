//! The few controls egui doesn't have, drawn to each platform's
//! conventions: grouped rows, switches, primary buttons, status dots, and
//! dialog buttons in the platform's order.

use egui::{
    Color32, CornerRadius, Margin, Response, RichText, Sense, Stroke, Ui, Vec2, WidgetInfo,
    WidgetType,
};

use super::theme::Theme;
use crate::model::Platform;

/// Line icons for the sidebar, drawn rather than taken from an emoji font
/// so they match each other and scale cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Folder,
    Lock,
    Clock,
    Person,
    Gear,
}

pub fn paint_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    icon: Icon,
    color: Color32,
) {
    use egui::{Pos2, Shape, pos2, vec2};
    let s = size / 16.0;
    let stroke = Stroke::new(1.4 * s.max(1.0), color);
    let p = |x: f32, y: f32| -> Pos2 { center + vec2((x - 8.0) * s, (y - 8.0) * s) };
    match icon {
        Icon::Folder => {
            let outline = vec![
                p(1.5, 3.5),
                p(6.0, 3.5),
                p(7.5, 5.0),
                p(14.5, 5.0),
                p(14.5, 13.0),
                p(1.5, 13.0),
            ];
            painter.add(Shape::closed_line(outline, stroke));
            painter.line_segment([p(1.5, 7.0), p(14.5, 7.0)], stroke);
        }
        Icon::Lock => {
            let body = egui::Rect::from_min_max(p(3.0, 7.0), p(13.0, 14.5));
            painter.rect_stroke(body, 1.5 * s, stroke, egui::StrokeKind::Middle);
            let shackle: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::PI * (1.0 + i as f32 / 16.0);
                    p(8.0 + 3.2 * a.cos(), 5.0 + 3.2 * a.sin())
                })
                .collect();
            painter.add(Shape::line(shackle, stroke));
            painter.line_segment([p(4.8, 5.0), p(4.8, 7.0)], stroke);
            painter.line_segment([p(11.2, 5.0), p(11.2, 7.0)], stroke);
            painter.circle_filled(p(8.0, 10.5), 1.1 * s, color);
        }
        Icon::Clock => {
            painter.circle_stroke(p(8.0, 8.0), 6.3 * s, stroke);
            painter.line_segment([p(8.0, 8.0), p(8.0, 4.3)], stroke);
            painter.line_segment([p(8.0, 8.0), p(10.8, 9.6)], stroke);
        }
        Icon::Person => {
            painter.circle_stroke(p(8.0, 5.0), 2.8 * s, stroke);
            let shoulders: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::PI * (1.0 + i as f32 / 16.0);
                    p(8.0 + 5.5 * a.cos(), 14.5 + 5.0 * a.sin())
                })
                .collect();
            painter.add(Shape::line(shoulders, stroke));
        }
        Icon::Gear => {
            painter.circle_stroke(p(8.0, 8.0), 4.2 * s, stroke);
            painter.circle_stroke(p(8.0, 8.0), 1.7 * s, stroke);
            for i in 0..8 {
                let a = std::f32::consts::TAU * i as f32 / 8.0;
                let (sin, cos) = a.sin_cos();
                painter.line_segment(
                    [
                        pos2(center.x + 4.2 * s * cos, center.y + 4.2 * s * sin),
                        pos2(center.x + 6.6 * s * cos, center.y + 6.6 * s * sin),
                    ],
                    Stroke::new(2.2 * s, color),
                );
            }
        }
    }
}

/// A page title and one line about it.
pub fn page_header(ui: &mut Ui, theme: &Theme, title: &str, subtitle: &str) {
    ui.add(egui::Label::new(
        RichText::new(title).heading().color(theme.palette.text),
    ));
    if !subtitle.is_empty() {
        ui.add_space(-2.0);
        ui.label(theme.weak(subtitle));
    }
    ui.add_space(10.0);
}

/// A heading above a group.
pub fn section(ui: &mut Ui, theme: &Theme, title: &str) {
    ui.add_space(6.0);
    let text = match theme.platform {
        // GNOME and macOS set group titles in a bold, smaller face.
        Platform::Linux | Platform::MacOs => theme.strong(title).size(theme.metrics.body - 0.5),
        Platform::Windows => theme.strong(title),
    };
    ui.label(text);
    ui.add_space(2.0);
}

/// Rows inside one rounded, bordered group.
pub fn group<R>(ui: &mut Ui, theme: &Theme, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme.palette.group)
        .stroke(Stroke::new(1.0, theme.palette.group_stroke))
        .corner_radius(CornerRadius::same(theme.metrics.group_radius))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A hairline between rows of a group.
pub fn row_separator(ui: &mut Ui, theme: &Theme) {
    ui.add_space(2.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, theme.palette.separator),
    );
    ui.add_space(2.0);
}

/// The button for the main action of a page or dialog.
pub fn primary_button(ui: &mut Ui, theme: &Theme, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(theme.palette.on_accent))
            .fill(theme.palette.accent)
            .stroke(Stroke::NONE),
    )
}

/// A button for an action that removes something.
pub fn destructive_button(ui: &mut Ui, theme: &Theme, text: &str) -> Response {
    match theme.platform {
        // GNOME's destructive-action style fills the button.
        Platform::Linux => ui.add(
            egui::Button::new(RichText::new(text).color(Color32::WHITE))
                .fill(theme.palette.danger)
                .stroke(Stroke::NONE),
        ),
        Platform::MacOs | Platform::Windows => ui.add(egui::Button::new(
            RichText::new(text).color(theme.palette.danger),
        )),
    }
}

/// Cancel and a confirming action, in the platform's order: the action
/// last on macOS and GNOME, first on Windows. Returns (cancel, confirm).
pub fn dialog_buttons(
    ui: &mut Ui,
    theme: &Theme,
    confirm: &str,
    destructive: bool,
) -> (bool, bool) {
    let mut cancelled = false;
    let mut confirmed = false;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let mut confirm_button = |ui: &mut Ui| {
            let response = if destructive {
                destructive_button(ui, theme, confirm)
            } else {
                primary_button(ui, theme, confirm)
            };
            confirmed |= response.clicked();
        };
        // Right to left: the first button added is the rightmost.
        match theme.platform {
            Platform::Windows => {
                cancelled |= ui.button("Cancel").clicked();
                confirm_button(ui);
            }
            Platform::MacOs | Platform::Linux => {
                confirm_button(ui);
                cancelled |= ui.button("Cancel").clicked();
            }
        }
    });
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        cancelled = true;
    }
    (cancelled, confirmed)
}

/// An on/off switch, as all three platforms use in settings.
pub fn switch(ui: &mut Ui, theme: &Theme, on: &mut bool, label: &str) -> Response {
    let height = (theme.metrics.control_height * 0.7).clamp(18.0, 24.0);
    let size = Vec2::new(height * 1.8, height);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *on, label));
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let p = &theme.palette;
        let radius = rect.height() / 2.0;
        let off_fill = if theme.dark {
            Color32::from_gray(90)
        } else {
            Color32::from_gray(200)
        };
        let fill = lerp_color(off_fill, p.accent, t);
        let painter = ui.painter();
        // Windows draws the off state as an outline with a dark knob.
        let windows_off = theme.platform == Platform::Windows && !*on;
        if windows_off {
            painter.rect(
                rect.shrink(0.5),
                radius,
                Color32::TRANSPARENT,
                Stroke::new(1.0, p.weak),
                egui::StrokeKind::Inside,
            );
        } else {
            painter.rect_filled(rect, radius, fill);
        }
        let knob_radius = if windows_off {
            radius * 0.5
        } else {
            radius - 2.5
        };
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        let knob = if windows_off { p.weak } else { Color32::WHITE };
        painter.circle(
            egui::pos2(x, rect.center().y),
            knob_radius,
            knob,
            Stroke::new(
                if theme.dark { 0.0 } else { 0.5 },
                Color32::from_black_alpha(30),
            ),
        );
        if response.has_focus() {
            painter.rect_stroke(
                rect.expand(2.0),
                radius + 2.0,
                Stroke::new(2.0, p.accent.gamma_multiply(0.6)),
                egui::StrokeKind::Outside,
            );
        }
    }
    response
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

/// A settings row: a title and description on the left, a control on the
/// right.
pub fn setting_row(
    ui: &mut Ui,
    theme: &Theme,
    title: &str,
    description: &str,
    control: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        let control_width = 90.0;
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - control_width).max(160.0));
            ui.label(RichText::new(title).color(theme.palette.text));
            if !description.is_empty() {
                ui.add_space(-4.0);
                ui.add(egui::Label::new(theme.small(description)).wrap());
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
}

/// A small colored dot with a label, for states.
pub fn status_dot(ui: &mut Ui, color: Color32, text: RichText) -> Response {
    ui.horizontal(|ui| {
        let size = 8.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
        ui.painter().circle_filled(rect.center(), size / 2.0, color);
        ui.label(text)
    })
    .inner
}

/// A small tinted pill, for index states.
pub fn badge(ui: &mut Ui, theme: &Theme, text: &str, color: Color32) -> Response {
    let fill = color.gamma_multiply(if theme.dark { 0.25 } else { 0.14 });
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(theme.metrics.small)
                    .color(if theme.dark {
                        color
                    } else {
                        color.gamma_multiply(0.9)
                    }),
            )
        })
        .inner
}

/// An inline message: an error, or a note.
pub fn notice(ui: &mut Ui, theme: &Theme, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(if theme.dark { 0.18 } else { 0.10 }))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.35)))
        .corner_radius(CornerRadius::same(theme.metrics.radius))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text).color(theme.palette.text)).wrap());
        });
}
