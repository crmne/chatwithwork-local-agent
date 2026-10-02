//! The web's form controls and overlays as the chat page draws them:
//! daisyUI's buttons, fields, selects and checkboxes as Live Wire restyles
//! them (`buttons.css`, `inputs.css`), menus (`menus.css`), modal dialogs
//! (`dialogs.css`) and the quiet state chip (`state.css`). Sizes are the
//! compiled CSS's, measured in Chromium.

use egui::{Color32, Id, Pos2, Rect, Response, Sense, Ui, WidgetInfo, WidgetType, pos2, vec2};

use super::icons::{Icon, Images};
use super::paint;
use super::tokens::{self, Palette, Type};
use super::widgets;

/// `.btn`'s kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The one main action: solid ink.
    Primary,
    /// Secondary actions: a surface with a hairline.
    Subtle,
    /// Destructive, quiet until hovered: orange ink.
    SubtleNegative,
    /// A solid negative button (`.btn-error`).
    Error,
}

/// `.btn`, `.btn-sm`, `.btn-xs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Regular,
    Small,
    Tiny,
}

impl Size {
    fn height(self) -> f32 {
        match self {
            Size::Regular => 40.0,
            Size::Small => 32.0,
            Size::Tiny => 24.0,
        }
    }

    fn padding(self) -> f32 {
        match self {
            Size::Regular => 16.0,
            Size::Small => 12.0,
            Size::Tiny => 8.0,
        }
    }

    fn ty(self) -> Type {
        let (size, line) = match self {
            Size::Regular => (14.0, 21.0),
            Size::Small => (12.0, 18.0),
            Size::Tiny => (11.0, 16.5),
        };
        Type::sans(size, line).weight(560.0).tracking(-0.006)
    }
}

/// A labeled button, as daisyUI's `.btn` draws it.
pub struct Button<'a> {
    pub kind: Kind,
    pub size: Size,
    pub label: &'a str,
    pub icon: Option<Icon>,
    /// What a screen reader says, when not the label.
    pub name: Option<&'a str>,
    pub enabled: bool,
    /// The fill and ring of a subtle button on a tinted card
    /// (`.approval .btn-subtle`).
    pub paper: Option<(Color32, Color32)>,
}

impl<'a> Button<'a> {
    pub fn new(kind: Kind, size: Size, label: &'a str) -> Self {
        Self {
            kind,
            size,
            label,
            icon: None,
            name: None,
            enabled: true,
            paper: None,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn paper(mut self, fill: Color32, ring: Color32) -> Self {
        self.paper = Some((fill, ring));
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn name(mut self, name: &'a str) -> Self {
        self.name = Some(name);
        self
    }

    pub fn height(&self) -> f32 {
        self.size.height()
    }

    fn icon_size(&self) -> f32 {
        match self.size {
            Size::Tiny => 14.0,
            _ => 16.0,
        }
    }

    /// How wide it is with its label.
    pub fn width(&self, ui: &Ui) -> f32 {
        let galley = paint::layout(
            ui.painter(),
            paint::job(self.label, self.size.ty(), Color32::WHITE, f32::INFINITY),
        );
        let icon = self.icon.map_or(0.0, |_| self.icon_size() + 6.0);
        // The 1-point border on each side.
        2.0 * self.size.padding() + galley.size().x + icon + 2.0
    }

    /// Draw it with its top-left corner at `at`.
    pub fn show_at(&self, ui: &mut Ui, id: Id, at: Pos2, p: &Palette, images: &Images) -> Response {
        let rect = Rect::from_min_size(at, vec2(self.width(ui), self.height()));
        self.show(ui, id, rect, p, images)
    }

    pub fn show(&self, ui: &mut Ui, id: Id, rect: Rect, p: &Palette, images: &Images) -> Response {
        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let response = ui.interact(rect, id, sense);
        let name = self.name.unwrap_or(self.label);
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, self.enabled, name));
        let t = widgets::hover(ui, id, self.enabled && response.hovered(), tokens::FAST);
        let (fill, ring, fg) = self.colors(p, t);
        let rect = rect.translate(vec2(
            0.0,
            if response.is_pointer_button_down_on() {
                0.5
            } else {
                0.0
            },
        ));
        let painter = ui.painter();
        let radius = tokens::RADIUS_CONTROL;
        let opacity = if self.enabled { 1.0 } else { 0.5 };
        painter.rect_filled(rect, radius, fill.gamma_multiply(opacity));
        painter.rect_stroke(
            rect.shrink(0.5),
            radius,
            egui::Stroke::new(1.0, ring.gamma_multiply(opacity)),
            egui::StrokeKind::Middle,
        );
        let galley = paint::layout(
            painter,
            paint::job(self.label, self.size.ty(), fg, f32::INFINITY),
        );
        let icon_w = self.icon.map_or(0.0, |_| self.icon_size() + 6.0);
        let x = rect.center().x - (galley.size().x + icon_w) / 2.0;
        if let Some(icon) = self.icon {
            images.icon_at(
                painter,
                pos2(x + self.icon_size() / 2.0, rect.center().y),
                self.icon_size(),
                icon,
                fg.gamma_multiply(opacity),
            );
        }
        painter.galley(
            pos2(x + icon_w, rect.center().y - galley.size().y / 2.0),
            galley,
            fg.gamma_multiply(opacity),
        );
        if response.has_focus() {
            widgets::focus_ring(ui, rect, radius, p);
        }
        if self.enabled && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response
    }

    fn colors(&self, p: &Palette, t: f32) -> (Color32, Color32, Color32) {
        match self.kind {
            Kind::Primary => {
                let fill = tokens::lerp_rgb(p.ink, tokens::mix(p.ink, p.canvas, 0.84), t);
                (fill, fill, p.ink_inverted)
            }
            Kind::Subtle | Kind::SubtleNegative => {
                let (fill, ring) = self.paper.unwrap_or((p.surface, p.line_strong));
                let hover_fill = if self.kind == Kind::SubtleNegative {
                    tokens::mix(p.negative(), p.surface, 0.10)
                } else {
                    p.surface_hover
                };
                let hover_ring = if self.kind == Kind::SubtleNegative {
                    tokens::alpha(p.negative(), 0.45)
                } else {
                    tokens::alpha(p.ink, 0.28)
                };
                let fg = if self.kind == Kind::SubtleNegative {
                    p.negative_ink()
                } else {
                    p.ink
                };
                (
                    tokens::lerp_rgb(fill, hover_fill, t),
                    tokens::lerp_rgb(ring, hover_ring, t),
                    fg,
                )
            }
            Kind::Error => {
                let fill =
                    tokens::lerp_rgb(p.negative(), tokens::mix(p.negative(), p.canvas, 0.86), t);
                let fg = if p.dark {
                    Color32::from_rgb(6, 7, 11)
                } else {
                    Color32::WHITE
                };
                (fill, fill, fg)
            }
        }
    }
}

/// The colors of a field: its fill and its resting hairline.
#[derive(Debug, Clone, Copy)]
pub struct FieldLook {
    pub fill: Color32,
    pub line: Color32,
}

impl FieldLook {
    pub fn new(p: &Palette) -> Self {
        Self {
            fill: p.surface,
            line: p.line_strong,
        }
    }
}

/// Draw a field's frame (`.input`): a surface with a strong hairline, a
/// darker one on hover, and the soft blue ring with focus.
pub fn field_frame(
    ui: &Ui,
    id: Id,
    rect: Rect,
    focused: bool,
    hovered: bool,
    look: FieldLook,
    p: &Palette,
) {
    let focus_t = ui
        .ctx()
        .animate_bool_with_time(id.with("focus-ring"), focused, tokens::FAST);
    let hover_t = widgets::hover(ui, id.with("field"), hovered, tokens::FAST);
    let painter = ui.painter();
    let radius = tokens::RADIUS_CONTROL;
    painter.rect_filled(rect, radius, look.fill);
    if focus_t > 0.0 {
        painter.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(3.0, p.focus().gamma_multiply(0.22 * focus_t)),
            egui::StrokeKind::Outside,
        );
    }
    let line = tokens::lerp_rgb(
        tokens::lerp_rgb(look.line, tokens::alpha(p.ink, 0.26), hover_t),
        p.focus(),
        focus_t,
    );
    painter.rect_stroke(
        rect.shrink(0.5),
        radius,
        egui::Stroke::new(1.0, line),
        egui::StrokeKind::Middle,
    );
}

/// A one-line text field (`.input`), `padding` in from the sides. Returns
/// the text edit's response.
#[allow(clippy::too_many_arguments)]
pub fn text_field(
    ui: &mut Ui,
    id: Id,
    rect: Rect,
    text: &mut String,
    placeholder: &str,
    ty: Type,
    padding: f32,
    look: FieldLook,
    p: &Palette,
    label: &str,
) -> Response {
    let focused = ui.memory(|m| m.has_focus(id));
    let hovered = ui.rect_contains_pointer(rect);
    field_frame(ui, id, rect, focused, hovered, look, p);
    let inner = Rect::from_min_max(
        pos2(
            rect.left() + padding,
            rect.center().y - ty.line_height / 2.0,
        ),
        pos2(
            rect.right() - padding,
            rect.center().y + ty.line_height / 2.0,
        ),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
    let output = egui::TextEdit::singleline(text)
        .id(id)
        .frame(egui::Frame::NONE)
        .margin(egui::Margin::ZERO)
        .font(ty.font_id())
        .text_color(p.ink)
        .hint_text(
            egui::RichText::new(placeholder)
                .font(ty.font_id())
                .color(p.ink_faint),
        )
        .desired_width(inner.width())
        .show(&mut child);
    let response = output.response.response;
    let label = label.to_string();
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(WidgetType::TextEdit, true, &label);
        info.current_text_value = None;
        info
    });
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    response
}

/// A checkbox (`.checkbox.checkbox-xs`), 16 square, with its label beside
/// it in `ty`, 8 apart. The whole row toggles it. Returns the response.
#[allow(clippy::too_many_arguments)]
pub fn checkbox(
    ui: &mut Ui,
    id: Id,
    at: Pos2,
    checked: &mut bool,
    label: &str,
    ty: Type,
    color: Color32,
    look: FieldLook,
    p: &Palette,
    wrap: f32,
) -> Response {
    let galley = paint::layout(ui.painter(), paint::job(label, ty, color, wrap - 24.0));
    let height = galley.size().y.max(16.0);
    let rect = Rect::from_min_size(at, vec2(24.0 + galley.size().x, height));
    let response = ui.interact(rect, id, Sense::click());
    if response.clicked() {
        *checked = !*checked;
    }
    let on = *checked;
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, on, label));
    let first_line = galley.rows.first().map_or(height, |r| r.row.size.y);
    let bx = Rect::from_min_size(
        pos2(at.x, at.y + (first_line - 16.0) / 2.0),
        vec2(16.0, 16.0),
    );
    let hover = widgets::hover(ui, id, response.hovered(), tokens::FAST);
    let painter = ui.painter();
    if on {
        painter.rect_filled(bx, 5.0, p.ink);
        // The tick, in the page's color.
        let s = 16.0 / 24.0;
        let points = [
            bx.min + vec2(6.0, 12.5) * s,
            bx.min + vec2(10.0, 16.5) * s,
            bx.min + vec2(18.0, 8.0) * s,
        ];
        painter.add(egui::Shape::line(
            points.to_vec(),
            egui::Stroke::new(2.0, p.canvas),
        ));
    } else {
        painter.rect_filled(bx, 5.0, look.fill);
        let line = tokens::lerp_rgb(look.line, tokens::alpha(p.ink, 0.26), hover);
        painter.rect_stroke(
            bx.shrink(0.5),
            4.5,
            egui::Stroke::new(1.0, line),
            egui::StrokeKind::Middle,
        );
    }
    painter.galley(pos2(at.x + 24.0, at.y), galley, color);
    if response.has_focus() {
        widgets::focus_ring(ui, bx, tokens::RADIUS_TAG, p);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A menu's panel (`.dropdown-content`): a raised surface, a hairline ring,
/// and the menu shadow.
pub fn menu_panel(painter: &egui::Painter, rect: Rect, p: &Palette) {
    paint::menu_shadow(painter, rect, tokens::RADIUS_CARD, p);
    painter.rect_filled(rect, tokens::RADIUS_CARD, p.surface_raised);
    painter.rect_stroke(
        rect.expand(0.5),
        tokens::RADIUS_CARD + 0.5,
        egui::Stroke::new(1.0, p.line),
        egui::StrokeKind::Middle,
    );
}

/// One row of a menu: an icon and a label, washed on hover. Negative rows
/// (`.menu__item--negative`) are orange.
#[allow(clippy::too_many_arguments)]
pub fn menu_item(
    ui: &mut Ui,
    id: Id,
    rect: Rect,
    icon: Option<Icon>,
    label: &str,
    negative: bool,
    active: bool,
    p: &Palette,
    images: &Images,
) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    let t = widgets::hover(ui, id, response.hovered(), tokens::INSTANT);
    let painter = ui.painter();
    let radius = tokens::RADIUS_CONTROL - 2.0;
    if active {
        painter.rect_filled(rect, radius, p.surface_selected);
    } else if t > 0.0 {
        painter.rect_filled(rect, radius, p.ink_wash(6.0 * t));
    }
    let color = if negative { p.negative_ink() } else { p.ink };
    let mut x = rect.left() + 12.0;
    if let Some(icon) = icon {
        images.icon_at(painter, pos2(x + 8.0, rect.center().y), 16.0, icon, color);
        x += 16.0 + 8.0;
    }
    let galley = paint::layout(
        painter,
        paint::line_job(label, tokens::scale::SMALL, color, rect.right() - 12.0 - x),
    );
    painter.galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    if response.has_focus() {
        widgets::focus_ring(ui, rect, radius, p);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Whether a press this frame landed outside every rectangle in `inside`,
/// to close a menu.
pub fn pressed_outside(ui: &Ui, inside: &[Rect]) -> bool {
    ui.input(|i| i.pointer.any_pressed())
        && ui
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|pos| !inside.iter().any(|r| r.contains(pos)))
}

/// `.state`: a dot in the state's color and a muted label. Returns its
/// width.
pub fn state_chip(
    painter: &egui::Painter,
    left_center: Pos2,
    label: &str,
    color: Option<Color32>,
    p: &Palette,
) -> f32 {
    let dot = pos2(left_center.x + 3.0, left_center.y);
    if let Some(color) = color {
        painter.circle_filled(dot, 3.0 + 3.0, tokens::alpha(color, 0.16));
        painter.circle_filled(dot, 3.0, color);
    } else {
        painter.circle_filled(dot, 3.0, p.line_strong);
    }
    let galley = paint::layout(
        painter,
        paint::job(label, tokens::scale::CAPTION, p.ink_muted, f32::INFINITY),
    );
    let x = left_center.x + 6.0 + 6.4;
    let w = galley.size().x;
    painter.galley(
        pos2(x, left_center.y - galley.size().y / 2.0),
        galley,
        p.ink_muted,
    );
    6.0 + 6.4 + w
}

/// A modal dialog (`.modal` with a `.modal-box`): the page dimmed behind
/// it, the box centered, `width` wide (at most the window less 32) and
/// `height` high. Clicking the backdrop or pressing Escape closes it, as
/// the web's `<form method="dialog" class="modal-backdrop">` does: the
/// result is false then. `content` draws inside the box's padding.
pub fn modal(
    ctx: &egui::Context,
    id: Id,
    width: f32,
    height: f32,
    p: &Palette,
    content: impl FnOnce(&mut Ui, Rect),
) -> bool {
    let screen = ctx.content_rect();
    let width = width.min(screen.width() - 32.0);
    let rect = Rect::from_center_size(screen.center(), vec2(width, height));
    let mut open = true;
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let backdrop = ui.interact(screen, id.with("backdrop"), Sense::click());
            ui.painter().rect_filled(screen, 0.0, p.backdrop());
            if backdrop.clicked()
                && !rect.contains(backdrop.interact_pointer_pos().unwrap_or_default())
            {
                open = false;
            }
            // Swallow clicks on the box itself.
            ui.interact(rect, id.with("box"), Sense::click());
            let painter = ui.painter();
            paint::float_shadow(painter, rect, tokens::RADIUS_PANEL, p);
            painter.rect_filled(rect, tokens::RADIUS_PANEL, p.surface_raised);
            painter.rect_stroke(
                rect.expand(0.5),
                tokens::RADIUS_PANEL + 0.5,
                egui::Stroke::new(1.0, p.line),
                egui::StrokeKind::Middle,
            );
            painter.hline(
                (rect.left() + tokens::RADIUS_PANEL)..=(rect.right() - tokens::RADIUS_PANEL),
                rect.top() + 0.5,
                egui::Stroke::new(1.0, p.shadow_inset),
            );
            let inner = rect.shrink(24.0);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
            content(&mut child, inner);
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        open = false;
    }
    open
}

/// `.dialog__header`: the title (17, 600) over a muted sentence, 6 apart.
/// Returns the header's height, without its 20 below.
pub fn dialog_header(
    ui: &mut Ui,
    at: Pos2,
    width: f32,
    title: &str,
    text: &[(&str, bool)],
    p: &Palette,
) -> f32 {
    let painter = ui.painter().clone();
    let title_ty = Type::sans(17.0, 25.5).weight(600.0).tracking(-0.025);
    let title_galley = paint::layout(&painter, paint::job(title, title_ty, p.ink, width));
    widgets::label(ui, Rect::from_min_size(at, title_galley.size()), title);
    let title_h = title_galley.size().y;
    painter.galley(at, title_galley, p.ink);
    if text.is_empty() {
        return title_h;
    }
    // The sentence, with any part in strong ink.
    let ty = tokens::scale::SMALL;
    let mut job = paint::job("", ty, p.ink_muted, width);
    let mut plain = String::new();
    for (part, strong) in text {
        let format = if *strong {
            ty.weight(700.0).format(p.ink)
        } else {
            ty.format(p.ink_muted)
        };
        job.append(part, 0.0, format);
        plain.push_str(part);
    }
    let galley = paint::layout(&painter, job);
    let pos = pos2(at.x, at.y + title_h + 6.0);
    widgets::label(ui, Rect::from_min_size(pos, galley.size()), &plain);
    let h = galley.size().y;
    painter.galley(pos, galley, p.ink_muted);
    title_h + 6.0 + h
}

/// The height of a [`dialog_header`].
pub fn dialog_header_height(
    painter: &egui::Painter,
    width: f32,
    title: &str,
    text: &[(&str, bool)],
) -> f32 {
    let title_ty = Type::sans(17.0, 25.5).weight(600.0).tracking(-0.025);
    let title_h = paint::layout(painter, paint::job(title, title_ty, Color32::WHITE, width))
        .size()
        .y;
    if text.is_empty() {
        return title_h;
    }
    let ty = tokens::scale::SMALL;
    let mut job = paint::job("", ty, Color32::WHITE, width);
    for (part, strong) in text {
        let format = if *strong {
            ty.weight(700.0).format(Color32::WHITE)
        } else {
            ty.format(Color32::WHITE)
        };
        job.append(part, 0.0, format);
    }
    title_h + 6.0 + paint::layout(painter, job).size().y
}

/// A select (`.select.select-sm`): the field with the value and daisyUI's
/// small arrow, opening its options in a menu below it. `options` may hold
/// an empty choice (no answer), shown blank. Returns true when it changed.
#[allow(clippy::too_many_arguments)]
pub fn select(
    ui: &mut Ui,
    id: Id,
    rect: Rect,
    value: &mut String,
    options: &[String],
    label: &str,
    p: &Palette,
    images: &Images,
) -> bool {
    let open_id = id.with("open");
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    let response = ui.interact(rect, id, Sense::click());
    let current = value.clone();
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(WidgetType::ComboBox, true, label);
        info.current_text_value = Some(current.clone());
        info
    });
    field_frame(
        ui,
        id,
        rect,
        open || response.has_focus(),
        response.hovered(),
        FieldLook::new(p),
        p,
    );
    let ty = tokens::scale::CAPTION;
    let painter = ui.painter().clone();
    let galley = paint::layout(
        &painter,
        paint::line_job(value, ty, p.ink, rect.width() - 12.0 - 28.0),
    );
    painter.galley(
        pos2(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.ink,
    );
    // daisyUI's arrow: a small triangle 16 in from the right.
    let c = pos2(rect.right() - 20.0, rect.center().y + 1.0);
    painter.add(egui::Shape::convex_polygon(
        vec![
            c + vec2(-4.0, -2.5),
            c + vec2(4.0, -2.5),
            c + vec2(0.0, 2.5),
        ],
        p.ink,
        egui::Stroke::NONE,
    ));
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() {
        open = !open;
    }
    let mut changed = false;
    if open {
        let item_h = 33.8;
        let menu = Rect::from_min_size(
            pos2(rect.left(), rect.bottom() + 4.0),
            vec2(rect.width(), 5.0 + options.len() as f32 * item_h + 5.0),
        );
        egui::Area::new(id.with("menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.min)
            .show(ui.ctx(), |ui| {
                menu_panel(ui.painter(), menu, p);
                for (n, option) in options.iter().enumerate() {
                    let row = Rect::from_min_size(
                        pos2(menu.left() + 5.0, menu.top() + 5.0 + n as f32 * item_h),
                        vec2(menu.width() - 10.0, item_h),
                    );
                    let shown = if option.is_empty() {
                        "None"
                    } else {
                        option.as_str()
                    };
                    let r = menu_item(
                        ui,
                        id.with(("option", n)),
                        row,
                        None,
                        shown,
                        false,
                        *option == *value,
                        p,
                        images,
                    );
                    if r.clicked() {
                        *value = option.clone();
                        changed = true;
                        open = false;
                    }
                }
            });
        if !changed
            && ((pressed_outside(ui, &[menu, rect]) && !response.clicked())
                || ui.input(|i| i.key_pressed(egui::Key::Escape)))
        {
            open = false;
        }
    }
    ui.data_mut(|d| d.insert_temp(open_id, open));
    changed
}
