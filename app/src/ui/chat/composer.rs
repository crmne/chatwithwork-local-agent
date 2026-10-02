//! The composer (`composer.css`): a surface with a 2-point rainbow edge in
//! a soft rainbow glow, the field, and a bar with the paperclip and send,
//! which turns into stop with a turning rainbow ring while an answer is
//! written. Locked, it dims and says why, with the way out.

use egui::text::{CCursor, CCursorRange};
use egui::{Color32, Id, Key, Rect, Sense, Ui, pos2, vec2};

use super::icons::{Icon, Images};
use super::paint;
use super::tokens::{self, Palette, scale};
use super::widgets;

pub const ID: &str = "chat-composer";
/// The tallest the field grows before it scrolls, in lines.
const MAX_LINES: f32 = 9.0;
const RADIUS: f32 = tokens::RADIUS_PANEL;

pub struct Composer<'a> {
    pub placeholder: &'a str,
    /// Why nothing can be asked, and the way out.
    pub locked: Option<&'a str>,
    pub action: Option<&'a str>,
    /// An answer is being written: the edge flows and send becomes stop.
    pub working: bool,
    pub stopping: bool,
    pub sending: bool,
    /// Whether what's typed can go now.
    pub can_send: bool,
    pub time: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Send,
    Stop,
    /// The locked composer's way out.
    Action,
    /// The paperclip: attachments are added in the browser.
    Attach,
}

impl Composer<'_> {
    /// The composer's height at `width` for what's typed.
    pub fn height(&self, ui: &Ui, input: &str, width: f32) -> f32 {
        let text_width = width - 16.0 - 24.0;
        let text = if input.is_empty() { " " } else { input };
        let galley = paint::layout(
            ui.painter(),
            paint::job(text, scale::BODY, Color32::WHITE, text_width),
        );
        let lines = galley
            .size()
            .y
            .clamp(scale::BODY.line_height, MAX_LINES * scale::BODY.line_height);
        let lock = self
            .locked
            .map_or(0.0, |reason| self.lock_height(ui, reason, width));
        8.0 + 10.0 + lines + 10.0 + lock + 48.0
    }

    fn lock_height(&self, ui: &Ui, reason: &str, width: f32) -> f32 {
        let galley = paint::layout(
            ui.painter(),
            self.lock_job(reason, &Palette::new(false), width),
        );
        4.0 + galley.size().y
    }

    fn lock_job(&self, reason: &str, p: &Palette, width: f32) -> egui::text::LayoutJob {
        let ty = scale::SMALLER;
        let mut job = paint::job(reason, ty, p.ink_muted, width - 40.0 - 24.0);
        if let Some(action) = self.action {
            job.append(" ", 0.0, ty.format(p.ink_muted));
            job.append(action, 0.0, ty.weight(550.0).format(p.ink));
        }
        job
    }

    /// Draw the composer at `rect` (as tall as [`Self::height`] says).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &self,
        ui: &mut Ui,
        rect: Rect,
        input: &mut String,
        images: &Images,
        p: &Palette,
        focus: bool,
    ) -> Option<Event> {
        let mut event = None;
        let id = Id::new(ID);
        let locked = self.locked.is_some();
        let focused = ui.memory(|m| m.has_focus(id));
        let focus_t = ui
            .ctx()
            .animate_bool_with_time(id.with("focus"), focused, tokens::BASE);

        // The glow: `.composer.rainbow-glow::before`.
        let glow_rect = rect.expand(6.4);
        let (rest, active) = if locked { (0.05, 0.08) } else { (0.14, 0.30) };
        let opacity = rest + (active - rest) * focus_t;
        paint::glow(ui.painter(), glow_rect, 28.0, 20.0, opacity, |pos| {
            tokens::rainbow((pos.x - glow_rect.left()) / glow_rect.width())
        });
        // The box, its shadow and its top highlight.
        paint::float_shadow(ui.painter(), rect, RADIUS, p);
        ui.painter().rect_filled(rect, RADIUS, p.surface);
        ui.painter().hline(
            (rect.left() + RADIUS)..=(rect.right() - RADIUS),
            rect.top() + 0.5,
            egui::Stroke::new(1.0, p.shadow_inset),
        );

        // The field.
        let field = Rect::from_min_max(
            rect.min + vec2(8.0 + 12.0, 8.0 + 10.0),
            pos2(rect.right() - 8.0 - 12.0, rect.top() + 8.0 + 10.0),
        );
        let lines_height = {
            let text = if input.is_empty() {
                " "
            } else {
                input.as_str()
            };
            let galley = paint::layout(
                ui.painter(),
                paint::job(text, scale::BODY, p.ink, field.width()),
            );
            galley
                .size()
                .y
                .clamp(scale::BODY.line_height, MAX_LINES * scale::BODY.line_height)
        };
        let field = Rect::from_min_size(field.min, vec2(field.width(), lines_height));
        // Enter sends; Shift+Enter starts a new line, as on the web.
        let enter = focused
            && !locked
            && ui.input_mut(|i| {
                !i.modifiers.shift
                    && !i.modifiers.command
                    && i.consume_key(egui::Modifiers::NONE, Key::Enter)
            });
        if enter && self.can_send {
            event = Some(Event::Send);
        }
        let placeholder = egui::RichText::new(self.placeholder)
            .font(scale::BODY.font_id())
            .color(p.ink_faint);
        let mut field_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(field)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        if locked {
            field_ui.disable();
            field_ui.set_opacity(0.6);
        }
        let output = egui::ScrollArea::vertical()
            .id_salt(id.with("scroll"))
            .max_height(lines_height)
            .stick_to_bottom(true)
            .show(&mut field_ui, |ui| {
                egui::TextEdit::multiline(input)
                    .id(id)
                    .frame(egui::Frame::NONE)
                    .margin(egui::Margin::ZERO)
                    .font(scale::BODY.font_id())
                    .text_color(p.ink)
                    .hint_text(placeholder)
                    .desired_rows(1)
                    .desired_width(field.width())
                    .show(ui)
            });
        let response = output.inner.response;
        if focus && !locked {
            response.request_focus();
            // Put the cursor at the end, as focusing a field does.
            let mut state = output.inner.state;
            let end = CCursor::new(input.chars().count());
            state.cursor.set_char_range(Some(CCursorRange::one(end)));
            state.store(ui.ctx(), id);
        }

        // The shortcut hint, until the field has focus.
        let hint_shown = !focused && !locked && rect.width() >= 400.0 && input.is_empty();
        let hint = ui
            .ctx()
            .animate_bool_with_time(id.with("hint"), hint_shown, tokens::FAST);
        paint::kbd_hint(
            ui.painter(),
            pos2(rect.right() - 20.0, rect.top() + 18.0 + 12.0),
            &[ctrl_key(), "/"],
            p,
            hint,
        );

        // Why it's locked, and the way out.
        let mut bar_top = rect.top() + 8.0 + 10.0 + lines_height + 10.0;
        if let Some(reason) = self.locked {
            let galley = paint::layout(ui.painter(), self.lock_job(reason, p, rect.width()));
            let text_pos = pos2(rect.left() + 20.0 + 16.0 + 8.0, bar_top + 4.0);
            images.icon_at(
                ui.painter(),
                pos2(
                    rect.left() + 20.0 + 8.0,
                    text_pos.y + scale::SMALLER.line_height / 2.0,
                ),
                16.0,
                Icon::LockSimple,
                p.ink_faint,
            );
            let text_rect = Rect::from_min_size(text_pos, galley.size());
            let lock = ui.interact(text_rect, id.with("lock"), Sense::click());
            lock.widget_info(|| {
                let text = match self.action {
                    Some(action) => format!("{reason} {action}"),
                    None => reason.to_string(),
                };
                egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text)
            });
            if self.action.is_some() {
                // The way out is underlined, as a link.
                if let Some(last) = galley.rows.last() {
                    let right = text_pos.x + last.pos.x + last.row.size.x;
                    let action_w = self.action.map_or(0.0, |a| {
                        paint::layout(
                            ui.painter(),
                            paint::job(a, scale::SMALLER.weight(550.0), p.ink, f32::INFINITY),
                        )
                        .size()
                        .x
                    });
                    let y = text_pos.y + last.pos.y + last.row.size.y - 3.0;
                    let underline = if lock.hovered() { p.ink } else { p.line_strong };
                    ui.painter().hline(
                        (right - action_w)..=right,
                        y,
                        egui::Stroke::new(1.0, underline),
                    );
                    let action_rect = Rect::from_min_max(
                        pos2(right - action_w, text_pos.y + last.pos.y),
                        pos2(right, text_pos.y + last.pos.y + last.row.size.y),
                    );
                    let button = ui.interact(action_rect, id.with("action"), Sense::click());
                    button.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Link,
                            true,
                            self.action.unwrap_or_default(),
                        )
                    });
                    if button.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if button.clicked() {
                        event = Some(Event::Action);
                    }
                }
            }
            ui.painter().galley(text_pos, galley, p.ink_muted);
            bar_top += 4.0 + text_rect.height();
        }

        // The bar: paperclip on the left, send or stop on the right, their
        // corners concentric with the box's.
        let control = 32.0;
        let control_radius = RADIUS - 10.0;
        let bar_center = bar_top + 6.0 + control / 2.0;
        let attach_rect = Rect::from_center_size(
            pos2(rect.left() + 10.0 + control / 2.0, bar_center),
            vec2(control, control),
        );
        let attach = widgets::IconButton {
            icon: Icon::Paperclip,
            icon_size: 16.0,
            label: "Attach files",
            tooltip: Some(if locked {
                "Attach files"
            } else {
                "Attach files in the browser"
            }),
            color: p.ink,
            hover_color: p.ink,
            wash: 7.0,
            radius: control_radius,
            enabled: !locked,
        }
        .show(ui, id.with("attach"), attach_rect, images, p);
        if attach.clicked() {
            event = Some(Event::Attach);
        }

        let send_rect = Rect::from_center_size(
            pos2(rect.right() - 10.0 - control / 2.0, bar_center),
            vec2(control, control),
        );
        if let Some(e) = self.send_button(ui, id.with("send"), send_rect, control_radius, images, p)
        {
            event = Some(e);
        }

        // The edge, over everything: `.chat-prompt::before`.
        let (rest, active) = if locked { (0.3, 0.3) } else { (0.7, 1.0) };
        let edge_opacity = rest + (active - rest) * focus_t;
        let flow = if self.working && !locked {
            (self.time / 5.0).fract() as f32
        } else {
            0.0
        };
        let working = self.working && !locked;
        paint::ring(ui.painter(), rect, RADIUS, 2.0, |pos| {
            let u = (pos.x - rect.left()) / rect.width();
            let color = if working {
                // `--rainbow-loop` at 200% width, sliding right.
                tokens::rainbow_loop(u / 2.0 - flow)
            } else {
                tokens::rainbow(u)
            };
            let color = if locked {
                desaturate(color, 0.3)
            } else {
                color
            };
            color.gamma_multiply(edge_opacity)
        });
        if !locked && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        event
    }

    fn send_button(
        &self,
        ui: &mut Ui,
        id: Id,
        rect: Rect,
        radius: f32,
        images: &Images,
        p: &Palette,
    ) -> Option<Event> {
        let stop = self.working && self.locked.is_none();
        let enabled = if stop { !self.stopping } else { self.can_send };
        let response = ui.interact(
            rect,
            id,
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let label = if stop {
            if self.stopping {
                "Stopping response"
            } else {
                "Stop response"
            }
        } else if self.sending {
            "Sending"
        } else {
            "Send message"
        };
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
        let t = widgets::hover(ui, id, enabled && response.hovered(), tokens::FAST);
        let painter = ui.painter();
        let pressed = if response.is_pointer_button_down_on() {
            0.5
        } else {
            0.0
        };
        let rect = rect.translate(vec2(0.0, pressed));
        if enabled || stop {
            let fill = tokens::lerp_rgb(p.ink, tokens::mix(p.ink, p.canvas, 0.84), t);
            painter.rect_filled(rect, radius, fill);
        } else {
            painter.rect_filled(rect, radius, p.ink.gamma_multiply(0.1));
        }
        if stop {
            // `.chat-prompt__ring`: 3 out, 2 wide, turning every 2.4 s, or
            // every 6 s and gray while stopping.
            let period = if self.stopping { 6.0 } else { 2.4 };
            let turn = (self.time / period).fract() as f32;
            paint::conic_ring(
                painter,
                rect.expand(3.0),
                radius + 3.0,
                2.0,
                turn,
                self.stopping.then_some(0.5),
            );
            images.icon_at(painter, rect.center(), 14.0, Icon::Stop, p.ink_inverted);
        } else {
            let color = if enabled {
                p.ink_inverted
            } else {
                p.ink.gamma_multiply(0.2)
            };
            images.icon_at(painter, rect.center(), 16.0, Icon::ArrowUp, color);
        }
        if response.has_focus() {
            widgets::focus_ring(ui, rect, radius, p);
        }
        let tip = if stop {
            if self.stopping { "Stopping" } else { "Stop" }
        } else if let Some(reason) = self.locked {
            reason
        } else if self.sending {
            "Sending"
        } else {
            "Send"
        };
        widgets::tooltip(ui, &response, tip, p, true);
        if enabled && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if enabled && response.clicked() {
            return Some(if stop { Event::Stop } else { Event::Send });
        }
        None
    }
}

/// `filter: saturate(amount)`.
fn desaturate(color: Color32, amount: f32) -> Color32 {
    let l = 0.2126 * f32::from(color.r())
        + 0.7152 * f32::from(color.g())
        + 0.0722 * f32::from(color.b());
    let mix = |c: u8| (l + (f32::from(c) - l) * amount).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_premultiplied(mix(color.r()), mix(color.g()), mix(color.b()), color.a())
}

/// "Ctrl", or "⌘" on a Mac, for shortcut hints.
pub fn ctrl_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl"
    }
}
