//! The composer (`composer.css`): a surface with a 2-point rainbow edge in
//! a soft rainbow glow, the field, and a bar with the paperclip and send,
//! which turns into stop with a turning rainbow ring while an answer is
//! written. Locked, it dims and says why, with the way out. Files picked
//! for the question show as chips in the field, as Lexxy draws them
//! (`lexxy.css`), and the model picker sits beside send
//! (`chats/_model_picker`).

use egui::text::{CCursor, CCursorRange};
use egui::{Color32, Id, Key, Rect, Sense, Ui, pos2, vec2};

use cww::tui::chat::{Asset, Model};

use super::controls;
use super::icons::{Icon, Images};
use super::paint;
use super::state::Attached;
use super::tokens::{self, Palette, Type, scale};
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
    /// Files picked for the question.
    pub attachments: Vec<Attached>,
    /// The models to pick from, when the server offers the choice.
    pub models: Vec<Model>,
    /// The model the next question goes to: its ID and name.
    pub model: Option<(String, String)>,
    /// Its maker's logo, as the chat names the model, for one the picker
    /// doesn't list.
    pub model_logo: Option<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Send,
    Stop,
    /// The locked composer's way out.
    Action,
    /// The paperclip: pick files.
    Attach,
    /// Take a file off the question.
    Detach(u64),
    /// A model picked in the picker, by ID.
    PickModel(String),
}

/// The chips' column, as Lexxy's `.attachment`: at most 320 wide, 56 high,
/// 6 apart and 6 from the text.
const CHIP_HEIGHT: f32 = 56.0;
const CHIP_WIDTH: f32 = 320.0;

/// The size of a file, as Lexxy writes it: "47.08 KB".
pub fn human_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".into();
    }
    let sizes = ["B", "KB", "MB", "GB", "TB"];
    let i = ((bytes as f64).ln() / 1024f64.ln()).floor() as usize;
    let i = i.min(sizes.len() - 1);
    format!("{:.2} {}", bytes as f64 / 1024f64.powi(i as i32), sizes[i])
}

/// A file's icon and its tint, for a content type: Phosphor's file icons,
/// tinted as the web's mime images are colored.
pub fn file_icon(content_type: &str, p: &Palette) -> (Icon, Color32) {
    let t = content_type;
    if t == "application/pdf" {
        (Icon::FilePdf, p.orange)
    } else if t.contains("spreadsheet") || t.contains("excel") || t == "text/csv" {
        (Icon::FileXls, p.green)
    } else if t.contains("presentation") || t.contains("powerpoint") {
        (Icon::FilePpt, p.orange)
    } else if t.contains("word") || t.contains("document") || t.contains("rtf") {
        (Icon::FileDoc, p.blue)
    } else if t.starts_with("image/") {
        (Icon::FileImage, p.violet)
    } else if t.starts_with("audio/") {
        (Icon::FileAudio, p.violet)
    } else if t.starts_with("video/") {
        (Icon::FileVideo, p.violet)
    } else if t.contains("json")
        || t.contains("xml")
        || t.contains("html")
        || t.contains("javascript")
    {
        (Icon::FileCode, p.ink_muted)
    } else if t.starts_with("text/") {
        (Icon::FileTxt, p.ink_muted)
    } else {
        (Icon::File, p.ink_muted)
    }
}

impl Composer<'_> {
    /// The height of the chips above the text, with their spacing.
    fn chips_height(&self) -> f32 {
        let n = self.attachments.len() as f32;
        if n == 0.0 {
            0.0
        } else {
            6.0 + n * CHIP_HEIGHT + (n - 1.0) * 6.0 + 6.0
        }
    }

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
        8.0 + 10.0 + self.chips_height() + lines + 10.0 + lock + 48.0
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

        // The files, then the field.
        let chips_top = rect.top() + 8.0 + 10.0;
        if let Some(key) = self.chips(
            ui,
            rect.left() + 8.0 + 12.0,
            chips_top,
            rect.width() - 40.0,
            images,
            p,
        ) {
            event = Some(Event::Detach(key));
        }
        let field_top = chips_top + self.chips_height();
        let field = Rect::from_min_max(
            pos2(rect.left() + 8.0 + 12.0, field_top),
            pos2(rect.right() - 8.0 - 12.0, field_top),
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
        let mut bar_top = field_top + lines_height + 10.0;
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
        if let Some(e) = self.paperclip(
            ui,
            id.with("attach"),
            attach_rect,
            control_radius,
            images,
            p,
        ) {
            event = Some(e);
        }

        let send_rect = Rect::from_center_size(
            pos2(rect.right() - 10.0 - control / 2.0, bar_center),
            vec2(control, control),
        );
        if let Some(e) = self.send_button(ui, id.with("send"), send_rect, control_radius, images, p)
        {
            event = Some(e);
        }
        if let Some(e) = self.picker(
            ui,
            id.with("model"),
            send_rect.left() - 6.0,
            bar_center,
            images,
            p,
        ) {
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

    /// `.chat-prompt__attach`: inked with a ring while files are attached,
    /// with a turning spinner while they upload.
    fn paperclip(
        &self,
        ui: &mut Ui,
        id: Id,
        rect: Rect,
        radius: f32,
        images: &Images,
        p: &Palette,
    ) -> Option<Event> {
        let locked = self.locked.is_some();
        let present = !self.attachments.is_empty();
        let uploading = self.attachments.iter().any(|a| a.signed_id.is_none());
        if present {
            ui.painter().rect_stroke(
                rect.shrink(0.5),
                radius,
                egui::Stroke::new(1.0, p.line_strong),
                egui::StrokeKind::Middle,
            );
        }
        let response = widgets::IconButton {
            icon: Icon::Paperclip,
            icon_size: 16.0,
            label: if uploading {
                "Attach files (uploading)"
            } else {
                "Attach files"
            },
            tooltip: Some("Attach files"),
            color: p.ink,
            hover_color: p.ink,
            wash: 7.0,
            radius,
            enabled: !locked,
        }
        .show(ui, id, rect, images, p);
        if uploading {
            // `.chat-prompt__spinner`: a 2-wide `line` circle 1 inside the
            // button's outer edge, its top quarter `live`, turning every 0.8 s.
            let center = rect.center();
            let r = (rect.width() + 6.2) / 2.0 - 1.0;
            ui.painter()
                .circle_stroke(center, r, egui::Stroke::new(2.0, p.line));
            let start = (self.time / 0.8).fract() as f32 * std::f32::consts::TAU
                - std::f32::consts::FRAC_PI_2
                - std::f32::consts::FRAC_PI_4;
            let points: Vec<egui::Pos2> = (0..=12)
                .map(|i| {
                    let a = start + std::f32::consts::FRAC_PI_2 * i as f32 / 12.0;
                    center + r * vec2(a.cos(), a.sin())
                })
                .collect();
            ui.painter()
                .add(egui::Shape::line(points, egui::Stroke::new(2.0, p.live())));
        }
        response.clicked().then_some(Event::Attach)
    }

    /// The files picked, as Lexxy's chips: a file icon, the name and size,
    /// a thin live bar while it uploads, and a way to take it off.
    fn chips(
        &self,
        ui: &mut Ui,
        left: f32,
        top: f32,
        width: f32,
        images: &Images,
        p: &Palette,
    ) -> Option<u64> {
        let mut removed = None;
        let width = width.min(CHIP_WIDTH);
        let mut y = top + 6.0;
        for attached in &self.attachments {
            let rect = Rect::from_min_size(pos2(left, y), vec2(width, CHIP_HEIGHT));
            y += CHIP_HEIGHT + 6.0;
            let id = Id::new(("chat-chip", attached.key));
            let hovered = ui.rect_contains_pointer(rect);
            let painter = ui.painter().clone();
            painter.rect_filled(rect, tokens::RADIUS_CONTROL, p.canvas_sunken);
            painter.rect_stroke(
                rect.shrink(0.5),
                tokens::RADIUS_CONTROL,
                egui::Stroke::new(1.0, p.line),
                egui::StrokeKind::Middle,
            );
            // Lexxy's `.attachment__icon--app`: the file type's image, 36
            // square, once the server named it.
            let icon_center = pos2(rect.left() + 11.0 + 18.0, rect.center().y);
            if !images.logos.draw(
                &painter,
                attached.icon.as_ref(),
                Rect::from_center_size(icon_center, vec2(36.0, 36.0)),
                0.0,
                p,
                1.0,
            ) {
                let (icon, tint) = file_icon(&attached.content_type, p);
                images.icon_at(&painter, icon_center, 34.0, icon, tint);
            }
            let uploading = attached.signed_id.is_none();
            let caption_left = rect.left() + 11.0 + 36.0 + 10.0;
            let caption_max = rect.right() - 11.0 - caption_left - 28.0;
            let name_ty = scale::SMALLER.weight(550.0);
            let natural = paint::layout(
                &painter,
                paint::job(&attached.name, name_ty, p.ink, f32::INFINITY),
            )
            .size()
            .x;
            let caption_w = if uploading {
                natural.min(caption_max * 0.6)
            } else {
                caption_max
            };
            let name = paint::layout(
                &painter,
                paint::line_job(&attached.name, name_ty, p.ink, caption_w),
            );
            let size = paint::layout(
                &painter,
                paint::job(
                    &human_size(attached.size),
                    Type::mono(11.0, 16.5),
                    p.ink_faint,
                    f32::INFINITY,
                ),
            );
            let caption_top = rect.center().y - (19.5 + 2.0 + 16.5) / 2.0;
            painter.galley(pos2(caption_left, caption_top), name, p.ink);
            painter.galley(
                pos2(caption_left, caption_top + 19.5 + 2.0),
                size,
                p.ink_faint,
            );
            let label = if uploading {
                format!("{}, uploading", attached.name)
            } else {
                format!("{}, {}", attached.name, human_size(attached.size))
            };
            widgets::label(ui, rect, &label);
            if uploading {
                // The size of the upload isn't reported as it goes, so the
                // bar runs without saying how far.
                let bar_left = caption_left + caption_w + 10.0;
                let bar = Rect::from_min_max(
                    pos2(bar_left, rect.center().y - 1.5),
                    pos2(rect.right() - 11.0 - 28.0, rect.center().y + 1.5),
                );
                painter.rect_filled(bar, 3.0, p.line);
                let phase = (self.time / 1.4).fract() as f32;
                let seg = bar.width() * 0.4;
                let x0 = bar.left() - seg + (bar.width() + seg) * phase;
                let fill = Rect::from_min_max(
                    pos2(x0.max(bar.left()), bar.top()),
                    pos2((x0 + seg).min(bar.right()), bar.bottom()),
                );
                if fill.width() > 0.0 {
                    painter.rect_filled(fill, 3.0, p.live());
                }
            }
            // Lexxy's remove button, on hover.
            let shown = widgets::hover(ui, id.with("remove"), hovered, tokens::FAST);
            let button = Rect::from_center_size(
                pos2(rect.right() - 6.0 - 12.0, rect.center().y),
                vec2(24.0, 24.0),
            );
            let mut faded = ui.new_child(egui::UiBuilder::new().max_rect(button));
            faded.set_opacity(shown.max(0.0));
            let remove_label = format!("Remove {}", attached.name);
            let response = widgets::IconButton {
                icon: Icon::X,
                icon_size: 14.0,
                label: &remove_label,
                tooltip: None,
                color: p.ink_muted,
                hover_color: p.ink,
                wash: 7.0,
                radius: tokens::RADIUS_TAG,
                enabled: true,
            }
            .show(&mut faded, id.with("x"), button, images, p);
            if response.clicked() {
                removed = Some(attached.key);
            }
        }
        removed
    }

    /// `.model-picker`: the model's mark and name and a caret, opening the
    /// list of models above it. `right` is where it ends.
    fn picker(
        &self,
        ui: &mut Ui,
        id: Id,
        right: f32,
        center_y: f32,
        images: &Images,
        p: &Palette,
    ) -> Option<Event> {
        let (model_id, name) = self.model.clone()?;
        if self.models.is_empty() {
            return None;
        }
        let locked = self.locked.is_some();
        let ty = scale::SMALLER;
        let label = paint::layout(
            ui.painter(),
            paint::line_job(
                &name,
                ty,
                p.ink_muted,
                240.0 - 8.0 - 16.0 - 7.0 - 7.0 - 12.0 - 7.0,
            ),
        );
        let width = 8.0 + 16.0 + 7.0 + label.size().x + 7.0 + 12.0 + 7.0;
        let rect = Rect::from_min_size(pos2(right - width, center_y - 16.0), vec2(width, 32.0));
        let open_id = id.with("open");
        let mut open = ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false)) && !locked;
        let response = ui.interact(
            rect,
            id,
            if locked {
                Sense::hover()
            } else {
                Sense::click()
            },
        );
        let label_text = format!("Model: {name}");
        response.widget_info(|| {
            let mut info =
                egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, !locked, &label_text);
            info.selected = Some(open);
            info
        });
        let t = widgets::hover(
            ui,
            id,
            (response.hovered() || open) && !locked,
            tokens::FAST,
        );
        let opacity = if locked { 0.6 } else { 1.0 };
        let painter = ui.painter().clone();
        if t > 0.0 {
            painter.rect_filled(rect, 16.0, p.ink_wash(6.0 * t));
        }
        let color = tokens::lerp_rgb(p.ink_muted, p.ink, t).gamma_multiply(opacity);
        let logo = self
            .models
            .iter()
            .find(|m| m.id == model_id)
            .and_then(|m| m.logo.as_ref())
            .or(self.model_logo.as_ref());
        model_mark(
            &painter,
            images,
            pos2(rect.left() + 8.0 + 8.0, rect.center().y),
            16.0,
            (&name, logo),
            p,
            opacity,
        );
        painter.galley(
            pos2(
                rect.left() + 8.0 + 16.0 + 7.0,
                rect.center().y - label.size().y / 2.0,
            ),
            label,
            color,
        );
        images.icon_at(
            &painter,
            pos2(rect.right() - 7.0 - 6.0, rect.center().y),
            12.0,
            Icon::CaretUpDown,
            p.ink_faint.gamma_multiply(opacity),
        );
        let rate = self
            .models
            .iter()
            .find(|m| m.id == model_id)
            .and_then(|m| m.rate.clone())
            .unwrap_or_else(|| "Model to use".into());
        if !open {
            widgets::tooltip(ui, &response, &rate, p, true);
        }
        if response.has_focus() {
            widgets::focus_ring(ui, rect, 16.0, p);
        }
        if !locked && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            open = !open;
        }
        let mut event = None;
        if open {
            let (picked, menu) = self.model_menu(ui, id, rect, &model_id, images, p);
            if let Some(picked) = picked {
                event = Some(Event::PickModel(picked));
                open = false;
            } else if (controls::pressed_outside(ui, &[menu, rect]) && !response.clicked())
                || ui.input(|i| i.key_pressed(Key::Escape))
            {
                open = false;
            }
        }
        ui.data_mut(|d| d.insert_temp(open_id, open));
        event
    }

    /// `.model-picker__menu`: every model, with its description and rate,
    /// the one in use ticked. Returns the one picked, and the menu's rect.
    fn model_menu(
        &self,
        ui: &mut Ui,
        id: Id,
        anchor: Rect,
        current: &str,
        images: &Images,
        p: &Palette,
    ) -> (Option<String>, Rect) {
        let screen = ui.ctx().content_rect();
        let width = 336.0f32.min(screen.width() - 32.0);
        let text_w = width - 10.0 - 24.0 - 20.0 - 12.0 - 16.0 - 12.0;
        let painter = ui.painter().clone();
        // Each option: 10 + name 21 + (2 + description) + 2 + rate 16.8 + 10.
        let rows: Vec<RowGalleys> = self
            .models
            .iter()
            .map(|m| {
                let desc = m.description.as_deref().filter(|d| !d.is_empty()).map(|d| {
                    paint::layout(
                        &painter,
                        paint::job(d, Type::sans(12.0, 16.8), p.ink_muted, text_w),
                    )
                });
                // What a question costs, or why it can't be asked now.
                let note = if m.selectable {
                    m.rate.as_deref()
                } else {
                    m.reason.as_deref().or(m.rate.as_deref())
                };
                let note = note.map(|n| {
                    paint::layout(&painter, paint::job(n, scale::MICRO, p.ink_faint, text_w))
                });
                let h = 10.0
                    + 21.0
                    + desc.as_ref().map_or(0.0, |g| 2.0 + g.size().y)
                    + note.as_ref().map_or(0.0, |g| 2.0 + g.size().y)
                    + 10.0;
                (h, desc, note)
            })
            .collect();
        let content: f32 = rows.iter().map(|(h, ..)| h).sum();
        let height = (5.0 + content + 5.0).min(384.0f32.min(screen.height() * 0.6));
        let rect = Rect::from_min_size(
            pos2(
                (anchor.right() - width).max(screen.left() + 16.0),
                anchor.top() - 8.0 - height,
            ),
            vec2(width, height),
        );
        let mut picked = None;
        egui::Area::new(id.with("menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ui.ctx(), |ui| {
                controls::menu_panel(ui.painter(), rect, p);
                let inner = rect.shrink(5.0);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                child.set_clip_rect(inner);
                egui::ScrollArea::vertical()
                    .id_salt(id.with("menu-scroll"))
                    .max_height(inner.height())
                    .show(&mut child, |ui| {
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        for (model, (h, desc, note)) in self.models.iter().zip(&rows) {
                            let (row, response) =
                                ui.allocate_exact_size(vec2(inner.width(), *h), Sense::click());
                            let active = model.id == current;
                            let name = model.name.clone();
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::SelectableLabel,
                                    true,
                                    active,
                                    &name,
                                )
                            });
                            let t = widgets::hover(
                                ui,
                                Id::new(("model-option", &model.id)),
                                response.hovered(),
                                tokens::INSTANT,
                            );
                            let radius = tokens::RADIUS_CONTROL - 2.0;
                            if active {
                                ui.painter().rect_filled(row, radius, p.surface_selected);
                            } else if t > 0.0 {
                                ui.painter().rect_filled(row, radius, p.ink_wash(6.0 * t));
                            }
                            // One that can't be asked now is dimmed.
                            let mut faded = ui.painter().clone();
                            faded.multiply_opacity(if model.selectable { 1.0 } else { 0.55 });
                            let painter = &faded;
                            let dim = 1.0;
                            model_mark(
                                painter,
                                images,
                                pos2(row.left() + 12.0 + 10.0, row.top() + 11.0 + 10.0),
                                20.0,
                                (&model.name, model.logo.as_ref()),
                                p,
                                dim,
                            );
                            let x = row.left() + 12.0 + 20.0 + 12.0;
                            let mut y = row.top() + 10.0;
                            let name_galley = paint::layout(
                                painter,
                                paint::line_job(
                                    &model.name,
                                    scale::SMALL.weight(550.0),
                                    p.ink,
                                    text_w,
                                ),
                            );
                            painter.galley(pos2(x, y), name_galley, p.ink.gamma_multiply(dim));
                            y += 21.0;
                            if let Some(desc) = desc {
                                y += 2.0;
                                painter.galley(
                                    pos2(x, y),
                                    desc.clone(),
                                    p.ink_muted.gamma_multiply(dim),
                                );
                                y += desc.size().y;
                            }
                            if let Some(note) = note {
                                y += 2.0;
                                painter.galley(pos2(x, y), note.clone(), p.ink_faint);
                            }
                            if active {
                                images.icon_at(
                                    painter,
                                    pos2(row.right() - 12.0 - 8.0, row.top() + 10.0 + 8.0),
                                    16.0,
                                    Icon::Check,
                                    p.ink,
                                );
                            }
                            if response.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if response.clicked() {
                                picked = Some(model.id.clone());
                            }
                        }
                    });
            });
        (picked, rect)
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

/// A model option's height and its description and note, laid out.
type RowGalleys = (
    f32,
    Option<std::sync::Arc<egui::Galley>>,
    Option<std::sync::Arc<egui::Galley>>,
);

/// A model's mark: its maker's logo, round, as the web's picker shows it;
/// a round chip with its name's first letter until it's in, or without one.
fn model_mark(
    painter: &egui::Painter,
    images: &Images,
    center: egui::Pos2,
    size: f32,
    (name, logo): (&str, Option<&Asset>),
    p: &Palette,
    opacity: f32,
) {
    let r = size / 2.0;
    let rect = Rect::from_center_size(center, vec2(size, size));
    if images.logos.draw(painter, logo, rect, r, p, opacity) {
        return;
    }
    painter.circle_filled(center, r, p.surface.gamma_multiply(opacity));
    painter.circle_stroke(
        center,
        r - 0.5,
        egui::Stroke::new(1.0, p.line_strong.gamma_multiply(opacity)),
    );
    let initial: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    let ty = Type::sans(size * 0.55, size * 0.7).weight(600.0);
    let galley = paint::layout(
        painter,
        paint::job(&initial, ty, p.ink_muted, f32::INFINITY),
    );
    painter.galley(
        center - galley.size() / 2.0,
        galley,
        p.ink_muted.gamma_multiply(opacity),
    );
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
