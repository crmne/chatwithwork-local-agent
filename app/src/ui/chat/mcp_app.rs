//! Where a tool's own view (MCP Apps) goes in the conversation: a row of
//! the activity log, the width of the column, framed as the web's
//! `.mcp-app` frames it.
//!
//! For now the slot draws a placeholder card. The view itself will come
//! from a helper process that renders the app's HTML with Servo and sends
//! RGBA frames: it will hand the slot a texture for [`McpAppSlot::frame`]
//! and take the input the slot forwards. The slot already reserves the
//! view's height and reports its rectangle, so the conversation's layout
//! won't move when the view arrives.

use egui::{Id, Rect, Sense, TextureId, Ui, pos2, vec2};

use cww::tui::chat::StepApp;

use super::icons::{Icon, Images};
use super::paint;
use super::tokens::{self, Palette, scale};
use super::widgets;

/// `.mcp-app__frame`'s height until the view says how tall it is: 20rem.
pub const DEFAULT_HEIGHT: f32 = 320.0;

pub struct McpAppSlot<'a> {
    pub app: &'a StepApp,
    /// The view's latest frame, once something renders it.
    pub frame: Option<TextureId>,
    /// The height the view asked for (`size-changed`).
    pub height: Option<f32>,
}

/// What happened in the slot this frame.
#[expect(
    dead_code,
    reason = "read by the app view's renderer, which comes next"
)]
pub struct SlotOutput {
    /// Where the view is drawn, in points: where a renderer sends frames
    /// for, and where pointer input is forwarded from.
    pub rect: Rect,
    pub response: egui::Response,
    /// "Open in browser" on the placeholder.
    pub open_in_browser: bool,
}

impl McpAppSlot<'_> {
    pub fn show(&self, ui: &mut Ui, id: Id, images: &Images, p: &Palette) -> SlotOutput {
        let width = ui.available_width();
        let height = self.height.unwrap_or(DEFAULT_HEIGHT).clamp(80.0, 640.0);
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        let service = if self.app.service.is_empty() {
            "This tool"
        } else {
            self.app.service.as_str()
        };
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, true, format!("{service}'s view"))
        });
        let painter = ui.painter();
        painter.rect_filled(rect, tokens::RADIUS_CARD, p.surface);
        painter.rect_stroke(
            rect.shrink(0.5),
            tokens::RADIUS_CARD,
            egui::Stroke::new(1.0, p.line),
            egui::StrokeKind::Middle,
        );
        let mut open_in_browser = false;
        match self.frame {
            Some(texture) => {
                // The view's pixels, clipped to the card's corners by the
                // renderer (it draws a transparent margin).
                painter.image(
                    texture,
                    rect.shrink(1.0),
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            None => {
                paint::dot_grid(painter, rect.shrink(1.0), 18.0, p.dot, false);
                let center = rect.center();
                images.icon_at(
                    painter,
                    center - vec2(0.0, 36.0),
                    24.0,
                    Icon::AppWindow,
                    p.ink_faint,
                );
                let title = format!("{service}'s view");
                let galley = paint::layout(
                    painter,
                    paint::job(&title, scale::SMALL.weight(550.0), p.ink, f32::INFINITY),
                );
                painter.galley(center - vec2(galley.size().x / 2.0, 14.0), galley, p.ink);
                let note = "It opens here in a coming version. Use it in the browser for now.";
                let galley = paint::layout(
                    painter,
                    paint::job(note, scale::MICRO, p.ink_faint, width - 48.0),
                );
                painter.galley(
                    center + vec2(-galley.size().x / 2.0, 12.0),
                    galley,
                    p.ink_faint,
                );
                let button = Rect::from_center_size(center + vec2(0.0, 56.0), vec2(132.0, 28.0));
                let open = widgets::text_button(ui, id.with("open"), button, "Open in browser", p);
                open_in_browser = open.clicked();
            }
        }
        SlotOutput {
            rect,
            response,
            open_in_browser,
        }
    }
}
