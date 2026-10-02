//! Small pieces the chat page's parts share: ghost icon buttons with the
//! web's tooltips, hover fades, service avatars, and labels for screen
//! readers on painted text.

use egui::{Color32, Id, Pos2, Rect, Response, Sense, Ui, WidgetInfo, WidgetType, vec2};

use super::icons::{Icon, Images};
use super::paint;
use super::tokens::{self, Palette};

/// How far a hover has faded in, 0 to 1, over `seconds`.
pub fn hover(ui: &Ui, id: Id, hovered: bool, seconds: f32) -> f32 {
    ui.ctx()
        .animate_bool_with_time(id.with("hover"), hovered, seconds)
}

/// Show the web's tooltip over `response` while it's hovered or focused:
/// after 75 ms, fading and sliding in over 200 ms.
pub fn tooltip(ui: &Ui, response: &Response, text: &str, palette: &Palette, above: bool) {
    let shown = response.hovered() || response.has_focus();
    let ctx = ui.ctx();
    let since_id = response.id.with("tooltip-since");
    let now = ctx.input(|i| i.time);
    let since = ctx.data_mut(|d| {
        if shown {
            *d.get_temp_mut_or_insert_with(since_id, || now)
        } else {
            d.remove_temp::<f64>(since_id);
            now
        }
    });
    let delayed = shown && now - since >= 0.075;
    if shown && !delayed {
        ctx.request_repaint_after(std::time::Duration::from_millis(80));
    }
    let t = ctx.animate_bool_with_time(response.id.with("tooltip"), delayed, 0.2);
    paint::tooltip(ctx, response.rect, text, palette, t, above);
}

/// A square ghost button with an icon (`.btn-ghost.btn-square`,
/// `.message__action`): no chrome until hovered.
pub struct IconButton<'a> {
    pub icon: Icon,
    pub icon_size: f32,
    pub label: &'a str,
    pub tooltip: Option<&'a str>,
    pub color: Color32,
    pub hover_color: Color32,
    /// The wash on hover, as a percentage of ink.
    pub wash: f32,
    pub radius: f32,
    pub enabled: bool,
}

impl IconButton<'_> {
    pub fn show(
        &self,
        ui: &mut Ui,
        id: Id,
        rect: Rect,
        images: &Images,
        palette: &Palette,
    ) -> Response {
        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let response = ui.interact(rect, id, sense);
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, self.enabled, self.label));
        let t = hover(ui, id, self.enabled && response.hovered(), tokens::FAST);
        let painter = ui.painter();
        if t > 0.0 {
            painter.rect_filled(rect, self.radius, palette.ink_wash(self.wash * t));
        }
        let color = if self.enabled {
            tokens::lerp_rgb(self.color, self.hover_color, t)
        } else {
            self.color.gamma_multiply(0.45)
        };
        let pressed = if response.is_pointer_button_down_on() {
            0.5
        } else {
            0.0
        };
        images.icon_at(
            painter,
            rect.center() + vec2(0.0, pressed),
            self.icon_size,
            self.icon,
            color,
        );
        if response.has_focus() {
            focus_ring(ui, rect, self.radius, palette);
        }
        if let Some(text) = self.tooltip {
            tooltip(ui, &response, text, palette, true);
        }
        if self.enabled && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response
    }
}

/// The focus ring everything reached by keyboard gets: 2 points of live
/// blue, 2 points out.
pub fn focus_ring(ui: &Ui, rect: Rect, radius: f32, palette: &Palette) {
    ui.painter().rect_stroke(
        rect.expand(3.0),
        radius + 3.0,
        egui::Stroke::new(2.0, palette.focus()),
        egui::StrokeKind::Middle,
    );
}

/// A service's avatar: a round surface with a hairline and its initial,
/// cut out from what's behind it (`.avatar-stack__item`).
pub fn avatar(
    painter: &egui::Painter,
    center: Pos2,
    size: f32,
    name: &str,
    palette: &Palette,
    cutout: Color32,
) {
    let r = size / 2.0;
    painter.circle_filled(center, r + 2.0, cutout);
    painter.circle_filled(center, r, palette.surface);
    painter.circle_stroke(center, r - 0.5, egui::Stroke::new(1.0, palette.line_strong));
    let initial: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    let ty = tokens::Type::sans(size * 0.48, size * 0.6).weight(600.0);
    let galley = paint::layout(
        painter,
        paint::job(&initial, ty, palette.ink_muted, f32::INFINITY),
    );
    painter.galley(center - galley.size() / 2.0, galley, palette.ink_muted);
}

/// A screen reader's label for text drawn with the painter.
pub fn label(ui: &mut Ui, rect: Rect, text: &str) {
    let key = (rect.min.x as i32, rect.min.y as i32);
    let response = ui.interact(rect, ui.id().with(("label", text, key)), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
}

/// A small subtle button with a label (`.btn.btn-sm`): a surface with a
/// strong hairline, darker on hover.
pub fn text_button(ui: &mut Ui, id: Id, rect: Rect, text: &str, palette: &Palette) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, text));
    let t = hover(ui, id, response.hovered(), tokens::FAST);
    let p = palette;
    let painter = ui.painter();
    let fill = tokens::lerp_rgb(p.surface, p.surface_hover, t);
    let border = tokens::lerp_rgb(p.line_strong, tokens::alpha(p.ink, 0.28), t);
    let rect = rect.translate(vec2(
        0.0,
        if response.is_pointer_button_down_on() {
            0.5
        } else {
            0.0
        },
    ));
    painter.rect_filled(rect, tokens::RADIUS_CONTROL - 2.0, fill);
    painter.rect_stroke(
        rect.shrink(0.5),
        tokens::RADIUS_CONTROL - 2.0,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Middle,
    );
    let ty = tokens::Type::sans(12.0, 16.0)
        .weight(560.0)
        .tracking(-0.006);
    let galley = paint::layout(painter, paint::job(text, ty, p.ink, f32::INFINITY));
    painter.galley(rect.center() - galley.size() / 2.0, galley, p.ink);
    if response.has_focus() {
        focus_ring(ui, rect, tokens::RADIUS_CONTROL - 2.0, p);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}
