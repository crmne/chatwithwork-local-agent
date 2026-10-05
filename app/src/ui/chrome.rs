//! Window controls on the app's own background. macOS keeps its native
//! traffic lights, positioned by fastframe-macos; other platforms draw
//! accessible controls and hand moving/resizing back to the window manager.

use egui::{Color32, Rect, ResizeDirection, Sense, Stroke, Ui, ViewportCommand, pos2, vec2};

use super::{Page, Theme};
use crate::model::Platform;

pub const HEIGHT: f32 = 40.0;
const BUTTON_WIDTH: f32 = 46.0;

pub fn show(ui: &mut Ui, theme: &Theme, page: Page) -> bool {
    let mac = theme.platform == Platform::MacOs;
    let fullscreen = ui.input(|i| i.viewport().fullscreen.unwrap_or(false));
    if fullscreen {
        return false;
    }
    let p = theme.palette;
    let fill = if page == Page::Chat {
        super::chat::tokens::Palette::new(theme.dark).canvas
    } else {
        p.window
    };
    let mut close = false;
    egui::Panel::top("window_header")
        .exact_size(HEIGHT)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(fill))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            if page != Page::Welcome && page != Page::Chat {
                let mut sidebar = rect;
                sidebar.max.x = sidebar.min.x + theme.metrics.sidebar_width;
                ui.painter().rect_filled(sidebar, 0, p.sidebar);
            }
            let mut drag_rect = rect;
            if mac {
                drag_rect.min.x += fastframe_macos::traffic_light_inset(ui.ctx());
            } else {
                drag_rect.max.x -= 3.0 * BUTTON_WIDTH;
            }
            let drag = ui.interact(
                drag_rect,
                ui.id().with("move_window"),
                Sense::click_and_drag(),
            );
            if drag.double_clicked() {
                match if mac {
                    fastframe_macos::double_click_action()
                } else {
                    fastframe_macos::DoubleClick::Zoom
                } {
                    fastframe_macos::DoubleClick::Zoom | fastframe_macos::DoubleClick::Fill => {
                        toggle_maximized(ui);
                    }
                    fastframe_macos::DoubleClick::Minimize => {
                        ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
                    }
                    fastframe_macos::DoubleClick::Nothing => {}
                }
            } else if drag.drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
            }
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Chat with Work",
                egui::FontId::proportional(12.0),
                p.weak,
            );
            if !mac {
                let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                let names = [
                    "Close window",
                    if maximized {
                        "Restore window"
                    } else {
                        "Maximize window"
                    },
                    "Minimize window",
                ];
                for (index, name) in names.into_iter().enumerate() {
                    let right = rect.right() - index as f32 * BUTTON_WIDTH;
                    let button = Rect::from_min_max(
                        pos2(right - BUTTON_WIDTH, rect.top()),
                        pos2(right, rect.bottom()),
                    );
                    let response = ui.interact(button, ui.id().with(name), Sense::click());
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name)
                    });
                    let hovered = response.hovered() || response.has_focus();
                    let ink = if hovered && index == 0 {
                        Color32::WHITE
                    } else {
                        p.text
                    };
                    if hovered {
                        ui.painter().rect_filled(
                            button,
                            0,
                            if index == 0 {
                                Color32::from_rgb(196, 43, 28)
                            } else {
                                p.control_hover
                            },
                        );
                    }
                    let icon = Rect::from_center_size(button.center(), vec2(10.0, 10.0));
                    let stroke = Stroke::new(1.0, ink);
                    match index {
                        0 => {
                            ui.painter()
                                .line_segment([icon.left_top(), icon.right_bottom()], stroke);
                            ui.painter()
                                .line_segment([icon.right_top(), icon.left_bottom()], stroke);
                        }
                        1 => {
                            if maximized {
                                let back = icon.translate(vec2(2.0, -2.0)).shrink(1.0);
                                ui.painter()
                                    .line_segment([back.left_top(), back.right_top()], stroke);
                                ui.painter()
                                    .line_segment([back.right_top(), back.right_bottom()], stroke);
                            }
                            ui.painter()
                                .rect_stroke(icon, 0, stroke, egui::StrokeKind::Inside);
                        }
                        _ => {
                            ui.painter().hline(icon.x_range(), icon.center().y, stroke);
                        }
                    }
                    if response.on_hover_text(name).clicked() {
                        match index {
                            0 => close = true,
                            1 => toggle_maximized(ui),
                            _ => ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true)),
                        }
                    }
                }
            }
        });
    close
}

fn toggle_maximized(ui: &Ui) {
    let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
    ui.ctx()
        .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
}

/// Borderless windows still need edge and corner resizing, including on
/// Wayland where only the compositor can perform it.
pub fn resize(ui: &mut Ui, platform: Platform) {
    if platform == Platform::MacOs
        || ui.input(|i| {
            i.viewport().fullscreen.unwrap_or(false) || i.viewport().maximized.unwrap_or(false)
        })
    {
        return;
    }
    let rect = ui.ctx().content_rect();
    let x = [
        rect.left(),
        rect.left() + 5.0,
        rect.right() - 5.0,
        rect.right(),
    ];
    let y = [
        rect.top(),
        rect.top() + 5.0,
        rect.bottom() - 5.0,
        rect.bottom(),
    ];
    let directions = [
        (
            0,
            0,
            ResizeDirection::NorthWest,
            egui::CursorIcon::ResizeNwSe,
        ),
        (
            1,
            0,
            ResizeDirection::North,
            egui::CursorIcon::ResizeVertical,
        ),
        (
            2,
            0,
            ResizeDirection::NorthEast,
            egui::CursorIcon::ResizeNeSw,
        ),
        (
            0,
            1,
            ResizeDirection::West,
            egui::CursorIcon::ResizeHorizontal,
        ),
        (
            2,
            1,
            ResizeDirection::East,
            egui::CursorIcon::ResizeHorizontal,
        ),
        (
            0,
            2,
            ResizeDirection::SouthWest,
            egui::CursorIcon::ResizeNeSw,
        ),
        (
            1,
            2,
            ResizeDirection::South,
            egui::CursorIcon::ResizeVertical,
        ),
        (
            2,
            2,
            ResizeDirection::SouthEast,
            egui::CursorIcon::ResizeNwSe,
        ),
    ];
    for (column, row, direction, cursor) in directions {
        let target = Rect::from_min_max(pos2(x[column], y[row]), pos2(x[column + 1], y[row + 1]));
        let response = ui
            .interact(
                target,
                ui.id().with(("resize_window", column, row)),
                Sense::drag(),
            )
            .on_hover_cursor(cursor);
        if response.is_pointer_button_down_on() {
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::BeginResize(direction));
        }
    }
}
