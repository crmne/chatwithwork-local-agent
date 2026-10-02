//! What an answer stopped at, in the activity log, as the web draws it
//! (`approval.css`, `messages/tool_calls/_approval` and `_input_request`):
//! a change to approve, a question from a tool's server as a form or a page
//! to visit, or, for everyone but the person it waits for, one line saying
//! whose it is. The line above the composer says the answer waits
//! (`.thinking--approval`).

use std::collections::HashMap;

use egui::{Color32, Id, Rect, Sense, Ui, pos2, vec2};

use cww::tui::chat::{Approval, Question};

use super::controls::{self, Button, FieldLook, Kind, Size};
use super::icons::{Icon, Images};
use super::paint;
use super::state::{ApprovalInput, Decision, FieldValue, default_form, form_input};
use super::tokens::{self, Palette, Type, scale};
use super::widgets;

/// Something the answer waits for.
#[derive(Debug, Clone, Copy)]
pub enum Card<'a> {
    Approval(&'a Approval),
    Question(&'a Question),
}

impl Card<'_> {
    pub fn id(&self) -> u64 {
        match self {
            Card::Approval(a) => a.id,
            Card::Question(q) => q.id,
        }
    }

    /// The service's logo; none for a person's own MCP server, which the
    /// web shows as `plugs-connected`.
    pub fn logo(&self) -> Option<&cww::tui::chat::Asset> {
        match self {
            Card::Approval(a) => a.logo.as_ref(),
            Card::Question(q) => q.logo.as_ref(),
        }
    }

    pub fn decidable(&self) -> bool {
        match self {
            Card::Approval(a) => a.decidable,
            Card::Question(q) => q.decidable,
        }
    }
}

/// What's filled in on the cards, by tool call.
#[derive(Default)]
pub struct CardInputs {
    pub approvals: HashMap<u64, ApprovalInput>,
    pub forms: HashMap<u64, Vec<FieldValue>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardEvent {
    Decide(Decision),
    /// The page a question asks the person to visit.
    OpenPage(String),
}

/// The card's colors: `--approval-*` for an open decision, in the
/// attention tint, or the negative one for a destructive change.
struct Tint {
    wash: Color32,
    edge: Color32,
    paper: Color32,
    ink: Color32,
    tone: Color32,
}

impl Tint {
    fn new(p: &Palette, destructive: bool) -> Self {
        if destructive {
            let paper_base = if p.dark { p.canvas_sunken } else { p.surface };
            Self {
                wash: p.negative_wash(),
                edge: p.negative_edge(),
                paper: tokens::mix(p.negative(), paper_base, 0.03),
                ink: p.negative_ink(),
                tone: p.negative(),
            }
        } else {
            Self {
                wash: p.attention_wash(),
                edge: p.attention_edge(),
                paper: p.attention_paper(),
                ink: p.attention_ink,
                tone: p.attention,
            }
        }
    }

    /// `--approval-rule`: the line between rows on the paper.
    fn rule(&self, p: &Palette) -> Color32 {
        tokens::mix(self.tone, p.line, 0.22)
    }

    /// Quiet text on the wash: `color-mix(approval-ink 35%, ink-muted)`.
    fn quiet(&self, p: &Palette) -> Color32 {
        tokens::mix(self.ink, p.ink_muted, 0.35)
    }
}

pub struct Cards<'a> {
    pub palette: &'a Palette,
    pub images: &'a Images,
    /// A decision on this tool call is on its way.
    pub deciding: Option<u64>,
}

impl Cards<'_> {
    /// Draw `card` across the log's width. `review` scrolls it into view.
    pub fn show(
        &self,
        ui: &mut Ui,
        card: Card,
        inputs: &mut CardInputs,
        review: bool,
    ) -> Option<CardEvent> {
        if !card.decidable() {
            self.waiting(ui, card);
            return None;
        }
        let p = self.palette;
        let destructive = matches!(card, Card::Approval(a) if a.effect == "destructive");
        let tint = Tint::new(p, destructive);
        let width = ui.available_width();
        // `margin-block: 0.375rem`, inside the log's own 6.
        ui.add_space(6.0);
        let top = ui.cursor().top();
        let left = ui.cursor().left();
        let background = ui.painter().add(egui::Shape::Noop);
        let inner_left = left + 18.0;
        let inner_w = width - 36.0;
        let mut y = top + 16.0;
        let mut event = None;
        let (service, kicker, summary) = match card {
            Card::Approval(a) => (
                a.service.as_str(),
                "Needs your approval".to_string(),
                a.summary.clone(),
            ),
            Card::Question(q) => (
                q.service.as_str(),
                "Asks you".to_string(),
                if q.message.trim().is_empty() {
                    format!("{} needs something from you to carry on.", q.service)
                } else {
                    q.message.clone()
                },
            ),
        };
        y = self.header(
            ui,
            inner_left,
            y,
            inner_w,
            (service, card.logo()),
            &kicker,
            &summary,
            &tint,
        );
        let id = Id::new(("chat-card", card.id()));
        let busy = self.deciding == Some(card.id());
        match card {
            Card::Approval(approval) => {
                y += 14.0;
                y = self.fields(ui, inner_left, y, inner_w, approval, &tint);
                if destructive {
                    y += 14.0;
                    y = self.warning(ui, inner_left, y, inner_w);
                }
                let input = inputs.approvals.entry(approval.id).or_default();
                y += 14.0;
                y = self.reason(ui, id, inner_left, y, inner_w, input, &tint);
                y += 14.0;
                let (bottom, decided) = self
                    .approval_actions(ui, id, inner_left, y, inner_w, approval, input, &tint, busy);
                y = bottom;
                event = decided.map(CardEvent::Decide);
            }
            Card::Question(question) => {
                y += 14.0;
                let values = inputs
                    .forms
                    .entry(question.id)
                    .or_insert_with(|| default_form(question));
                let (bottom, open) = if question.is_url() {
                    self.visit(ui, id, inner_left, y, inner_w, question)
                } else {
                    (
                        self.form(ui, id, inner_left, y, inner_w, question, values),
                        None,
                    )
                };
                y = bottom;
                if let Some(url) = open {
                    event = Some(CardEvent::OpenPage(url));
                }
                if let Some(note) = question.note.as_deref().filter(|n| !n.is_empty()) {
                    y += 14.0;
                    let galley = paint::layout(
                        ui.painter(),
                        paint::job(note, scale::SMALLER, p.ink_muted, inner_w),
                    );
                    widgets::label(
                        ui,
                        Rect::from_min_size(pos2(inner_left, y), galley.size()),
                        note,
                    );
                    let h = galley.size().y;
                    ui.painter()
                        .galley(pos2(inner_left, y), galley, p.ink_muted);
                    y += h;
                }
                y += 14.0;
                let send_label = if question.is_url() {
                    "I've done it"
                } else {
                    "Send"
                };
                let send = Button::new(Kind::Primary, Size::Small, send_label).enabled(!busy);
                let decline = Button::new(Kind::Subtle, Size::Small, "Decline")
                    .paper(tint.paper, tint.edge)
                    .enabled(!busy);
                let send_w = send.width(ui);
                let decline_w = decline.width(ui);
                let right = inner_left + inner_w;
                if send
                    .show_at(ui, id.with("send"), pos2(right - send_w, y), p, self.images)
                    .clicked()
                {
                    let input = if question.is_url() {
                        None
                    } else {
                        Some(form_input(question, inputs.forms.get(&question.id)))
                    };
                    event = Some(CardEvent::Decide(Decision::Answer {
                        tool_call: question.id,
                        input,
                    }));
                }
                if decline
                    .show_at(
                        ui,
                        id.with("decline"),
                        pos2(right - send_w - 8.0 - decline_w, y),
                        p,
                        self.images,
                    )
                    .clicked()
                {
                    event = Some(CardEvent::Decide(Decision::Decline {
                        tool_call: question.id,
                    }));
                }
                y += 32.0;
            }
        }
        let rect = Rect::from_min_max(pos2(left, top), pos2(left + width, y + 18.0));
        ui.painter().set(background, self.background(rect, &tint));
        ui.allocate_rect(rect, Sense::hover());
        ui.add_space(6.0);
        if review {
            ui.scroll_to_rect(rect, Some(egui::Align::Center));
        }
        event
    }

    /// The card itself: the wash, a tinted ring, a soft tinted shadow.
    fn background(&self, rect: Rect, tint: &Tint) -> egui::Shape {
        let radius = tokens::RADIUS_CARD;
        let shadow = paint::drop_shadow_shape(
            rect,
            radius,
            vec2(0.0, 18.0),
            40.0,
            -28.0,
            tokens::alpha(tint.tone, 0.55),
        );
        let fill = egui::Shape::rect_filled(rect, radius, tint.wash);
        let ring = egui::Shape::rect_stroke(
            rect.shrink(0.5),
            radius - 0.5,
            egui::Stroke::new(1.0, tint.edge),
            egui::StrokeKind::Middle,
        );
        egui::Shape::Vec(vec![shadow, fill, ring])
    }

    /// `.approval__header`: the service's avatar, the kicker with a dot in
    /// the tone, and one line saying what it is. Returns its bottom.
    #[allow(clippy::too_many_arguments)]
    fn header(
        &self,
        ui: &mut Ui,
        left: f32,
        top: f32,
        width: f32,
        (service, logo): (&str, Option<&cww::tui::chat::Asset>),
        kicker: &str,
        summary: &str,
        tint: &Tint,
    ) -> f32 {
        let p = self.palette;
        let painter = ui.painter().clone();
        let center = pos2(left + 12.0, top + 12.0);
        painter.circle_filled(center, 12.0 + 2.0, tint.wash);
        let face = match logo {
            Some(asset) => widgets::Face::Logo {
                asset: Some(asset),
                name: service,
            },
            None => widgets::Face::Glyph(Icon::PlugsConnected),
        };
        widgets::avatar(
            &painter,
            self.images,
            center,
            24.0,
            0.56,
            face,
            p,
            tint.wash,
        );
        painter.circle_stroke(center, 11.5, egui::Stroke::new(1.0, tint.edge));
        let x = left + 24.0 + 12.0;
        let w = width - 36.0;
        // The kicker: mono, uppercase, after a dot.
        painter.circle_filled(pos2(x + 3.5, top + 8.25), 3.5, tint.tone);
        let text = format!("{} · {}", kicker, service).to_uppercase();
        let galley = paint::layout(
            &painter,
            paint::job(
                &text,
                Type::mono(11.0, 16.5).weight(500.0),
                tint.ink,
                w - 14.0,
            ),
        );
        painter.galley(pos2(x + 7.0 + 7.0, top), galley, tint.ink);
        let ty = Type::sans(15.0, 22.5).weight(550.0).tracking(-0.01);
        let galley = paint::layout(&painter, paint::job(summary, ty, p.ink, w));
        let pos = pos2(x, top + 16.5 + 2.0);
        widgets::label(
            ui,
            Rect::from_min_size(pos, galley.size()),
            &format!("{kicker} · {service}: {summary}"),
        );
        let bottom = pos.y + galley.size().y;
        painter.galley(pos, galley, p.ink);
        bottom.max(top + 24.0)
    }

    /// `.approval__fields`: what it will write, label and value, on paper.
    fn fields(
        &self,
        ui: &mut Ui,
        left: f32,
        top: f32,
        width: f32,
        approval: &Approval,
        tint: &Tint,
    ) -> f32 {
        let p = self.palette;
        let painter = ui.painter().clone();
        if approval.details.is_empty() {
            let galley = paint::layout(
                &painter,
                paint::job(
                    "It runs without any details.",
                    scale::SMALLER,
                    p.ink_muted,
                    width,
                ),
            );
            let h = galley.size().y;
            painter.galley(pos2(left, top), galley, p.ink_muted);
            return top + h;
        }
        // Two columns (the label at most 144 wide), or one when narrow.
        let stacked = width < 480.0 - 36.0;
        let label_w = if stacked { width - 24.0 } else { 144.0 };
        let value_w = if stacked {
            width - 24.0
        } else {
            width - 24.0 - 144.0 - 12.0
        };
        let background = painter.add(egui::Shape::Noop);
        let mut y = top;
        let mut shapes = Vec::new();
        for (n, detail) in approval.details.iter().enumerate() {
            if n > 0 {
                shapes.push(egui::Shape::hline(
                    left..=left + width,
                    y,
                    egui::Stroke::new(1.0, tint.rule(p)),
                ));
            }
            let label = paint::layout(
                &painter,
                paint::job(&detail.label, scale::SMALLER, p.ink_faint, label_w),
            );
            let value = paint::layout(
                &painter,
                paint::job(&detail.value, scale::SMALL, p.ink, value_w),
            );
            let (lx, ly) = (left + 12.0, y + 8.0);
            let (vx, vy) = if stacked {
                (left + 12.0, y + 8.0 + label.size().y + 4.0)
            } else {
                (left + 12.0 + 144.0 + 12.0, y + 8.0)
            };
            let row_h = if stacked {
                8.0 + label.size().y + 4.0 + value.size().y + 8.0
            } else {
                8.0 + label.size().y.max(value.size().y).max(21.0) + 8.0
            };
            widgets::label(
                ui,
                Rect::from_min_size(pos2(left, y), vec2(width, row_h)),
                &format!("{}: {}", detail.label, detail.value),
            );
            painter.galley(pos2(lx, ly + 1.0), label, p.ink_faint);
            painter.galley(pos2(vx, vy), value, p.ink);
            y += row_h;
        }
        let rect = Rect::from_min_max(pos2(left, top), pos2(left + width, y));
        let radius = tokens::RADIUS_CONTROL;
        let mut all = vec![
            egui::Shape::rect_filled(rect, radius, tint.paper),
            egui::Shape::rect_stroke(
                rect.shrink(0.5),
                radius - 0.5,
                egui::Stroke::new(1.0, tint.edge),
                egui::StrokeKind::Middle,
            ),
        ];
        all.extend(shapes);
        painter.set(background, egui::Shape::Vec(all));
        y
    }

    /// `.approval__warning`, for a change that can delete or overwrite.
    fn warning(&self, ui: &mut Ui, left: f32, top: f32, width: f32) -> f32 {
        let p = self.palette;
        let text = "This can delete or overwrite something, and may not be undone.";
        let ty = scale::SMALLER.weight(500.0);
        let galley = paint::layout(
            ui.painter(),
            paint::job(text, ty, p.negative_ink(), width - 24.0 - 24.0),
        );
        let rect = Rect::from_min_size(pos2(left, top), vec2(width, galley.size().y + 16.0));
        widgets::label(ui, rect, text);
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            tokens::RADIUS_CONTROL,
            tokens::alpha(p.negative(), 0.14),
        );
        self.images.icon_at(
            painter,
            pos2(rect.left() + 12.0 + 8.0, rect.center().y),
            16.0,
            Icon::Warning,
            p.negative_ink(),
        );
        painter.galley(
            pos2(rect.left() + 12.0 + 16.0 + 8.0, rect.top() + 8.0),
            galley,
            p.negative_ink(),
        );
        rect.bottom()
    }

    /// "Deny with a reason": a disclosure with one field.
    #[allow(clippy::too_many_arguments)]
    fn reason(
        &self,
        ui: &mut Ui,
        id: Id,
        left: f32,
        top: f32,
        width: f32,
        input: &mut ApprovalInput,
        tint: &Tint,
    ) -> f32 {
        let p = self.palette;
        let text = "Deny with a reason";
        let painter = ui.painter().clone();
        let size = paint::layout(
            &painter,
            paint::job(text, scale::SMALLER, p.ink, f32::INFINITY),
        )
        .size();
        let rect = Rect::from_min_size(pos2(left, top), size);
        let response = ui.interact(rect, id.with("reason-toggle"), Sense::click());
        let open = input.reason_open;
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text);
            info.selected = Some(open);
            info
        });
        let t = widgets::hover(
            ui,
            id.with("reason-toggle"),
            response.hovered(),
            tokens::FAST,
        );
        let color = tokens::lerp_rgb(tint.quiet(p), p.ink, t);
        let galley = paint::layout(
            &painter,
            paint::job(text, scale::SMALLER, color, f32::INFINITY),
        );
        painter.galley(rect.min, galley, color);
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let field_id = id.with("reason");
        if response.clicked() {
            input.reason_open = !input.reason_open;
            if input.reason_open {
                ui.memory_mut(|m| m.request_focus(field_id));
            }
        }
        let mut bottom = rect.bottom();
        if input.reason_open {
            let field = Rect::from_min_size(pos2(left, bottom + 8.0), vec2(width, 32.0));
            controls::text_field(
                ui,
                field_id,
                field,
                &mut input.reason,
                "Say what to do instead (optional)",
                scale::CAPTION,
                12.0,
                FieldLook::new(p),
                p,
                "Why you're denying it",
            );
            if input.reason.chars().count() > 500 {
                input.reason = input.reason.chars().take(500).collect();
            }
            bottom = field.bottom();
        }
        bottom
    }

    /// `.approval__actions`: "Allow … for the rest of this chat", then Deny
    /// and Approve. Returns the bottom and the decision made.
    #[allow(clippy::too_many_arguments)]
    fn approval_actions(
        &self,
        ui: &mut Ui,
        id: Id,
        left: f32,
        top: f32,
        width: f32,
        approval: &Approval,
        input: &mut ApprovalInput,
        tint: &Tint,
        busy: bool,
    ) -> (f32, Option<Decision>) {
        let p = self.palette;
        let approve = Button::new(Kind::Primary, Size::Small, "Approve").enabled(!busy);
        let deny = Button::new(Kind::Subtle, Size::Small, "Deny")
            .paper(tint.paper, tint.edge)
            .enabled(!busy);
        let (approve_w, deny_w) = (approve.width(ui), deny.width(ui));
        let buttons_w = deny_w + 8.0 + approve_w;
        let right = left + width;
        let mut decision = None;
        let mut buttons_top = top;
        if approval.allow_for_rest_of_chat {
            let label = format!(
                "Allow this in {} for the rest of this chat",
                approval.service
            );
            let room = width - buttons_w - 12.0;
            let look = FieldLook {
                fill: tint.paper,
                line: tint.edge,
            };
            // It wraps onto its own line when the buttons leave no room.
            let galley_w = paint::layout(
                ui.painter(),
                paint::job(&label, scale::SMALLER, p.ink, f32::INFINITY),
            )
            .size()
            .x + 24.0;
            let (at, wrap) = if galley_w <= room {
                (pos2(left, top + (32.0 - 19.5) / 2.0), room)
            } else {
                (pos2(left, top), width)
            };
            let response = controls::checkbox(
                ui,
                id.with("for-rest"),
                at,
                &mut input.for_rest_of_chat,
                &label,
                scale::SMALLER,
                tint.quiet(p),
                look,
                p,
                wrap,
            );
            if galley_w > room {
                buttons_top = response.rect.bottom() + 12.0;
            }
        }
        if approve
            .show_at(
                ui,
                id.with("approve"),
                pos2(right - approve_w, buttons_top),
                p,
                self.images,
            )
            .clicked()
        {
            decision = Some(Decision::Approve {
                tool_call: approval.id,
                for_rest_of_chat: approval.allow_for_rest_of_chat && input.for_rest_of_chat,
            });
        }
        if deny
            .show_at(
                ui,
                id.with("deny"),
                pos2(right - buttons_w, buttons_top),
                p,
                self.images,
            )
            .clicked()
        {
            let reason = input.reason.trim();
            decision = Some(Decision::Deny {
                tool_call: approval.id,
                reason: (input.reason_open && !reason.is_empty()).then(|| reason.to_string()),
            });
        }
        (buttons_top + 32.0, decision)
    }

    /// `.approval__visit`: the page a question asks the person to open.
    fn visit(
        &self,
        ui: &mut Ui,
        id: Id,
        left: f32,
        top: f32,
        width: f32,
        question: &Question,
    ) -> (f32, Option<String>) {
        let p = self.palette;
        let (left, top, width) = (left + 16.0, top + 14.0, width - 32.0);
        let painter = ui.painter().clone();
        let host = question.host.as_deref().filter(|_| question.url.is_some());
        let Some(host) = host else {
            let text = "It asks you to open a page, but the address it sent isn't a web page.";
            let galley = paint::layout(&painter, paint::job(text, scale::SMALL, p.ink, width));
            widgets::label(
                ui,
                Rect::from_min_size(pos2(left, top), galley.size()),
                text,
            );
            let h = galley.size().y;
            painter.galley(pos2(left, top), galley, p.ink);
            return (top + h + 14.0, None);
        };
        let ty = scale::SMALL;
        let mut job = paint::job("It asks you to open a page on ", ty, p.ink, width);
        job.append(host, 0.0, ty.weight(700.0).format(p.ink));
        job.append(", then come back.", 0.0, ty.format(p.ink));
        let galley = paint::layout(&painter, job);
        let button_label = format!("Open {host}");
        let button = Button::new(Kind::Subtle, Size::Small, &button_label);
        let bw = button.width(ui);
        let text_w = galley.size().x;
        let sentence = format!("It asks you to open a page on {host}, then come back.");
        // On one line with the button when it fits, as the flex row wraps.
        let (text_pos, button_pos, bottom) = if text_w + 12.0 + bw <= width {
            (
                pos2(left, top + (32.0 - galley.size().y) / 2.0),
                pos2(left + text_w + 12.0, top),
                top + 32.0,
            )
        } else {
            let h = galley.size().y;
            (
                pos2(left, top),
                pos2(left, top + h + 8.0),
                top + h + 8.0 + 32.0,
            )
        };
        widgets::label(ui, Rect::from_min_size(text_pos, galley.size()), &sentence);
        painter.galley(text_pos, galley, p.ink);
        let open = button
            .show_at(ui, id.with("open-page"), button_pos, p, self.images)
            .clicked()
            .then(|| question.url.clone())
            .flatten();
        (bottom + 14.0, open)
    }

    /// The question's form: each field as the web's form builds it.
    #[allow(clippy::too_many_arguments)]
    fn form(
        &self,
        ui: &mut Ui,
        id: Id,
        left: f32,
        top: f32,
        width: f32,
        question: &Question,
        values: &mut [FieldValue],
    ) -> f32 {
        let p = self.palette;
        let (left, width) = (left + 16.0, width - 32.0);
        let mut y = top + 14.0;
        let label_color = tokens::alpha(p.ink, 0.6);
        for (n, (field, value)) in question.fields.iter().zip(values.iter_mut()).enumerate() {
            if n > 0 {
                y += 12.0;
            }
            let fid = id.with(("field", n));
            let label = field.label().to_string();
            match value {
                FieldValue::Check(on) => {
                    let r = controls::checkbox(
                        ui,
                        fid,
                        pos2(left, y),
                        on,
                        &label,
                        scale::SMALL.weight(550.0),
                        p.ink,
                        FieldLook::new(p),
                        p,
                        width,
                    );
                    y = r.rect.bottom();
                }
                FieldValue::Picks(picks) => {
                    let galley = paint::layout(
                        ui.painter(),
                        paint::job(&label, scale::SMALL, label_color, width),
                    );
                    widgets::label(
                        ui,
                        Rect::from_min_size(pos2(left, y), galley.size()),
                        &label,
                    );
                    let h = galley.size().y;
                    ui.painter().galley(pos2(left, y), galley, label_color);
                    y += h;
                    let choices = field.choices.clone().unwrap_or_default();
                    for (k, (choice, on)) in choices.iter().zip(picks.iter_mut()).enumerate() {
                        if k > 0 {
                            y += 6.0;
                        }
                        let r = controls::checkbox(
                            ui,
                            fid.with(k),
                            pos2(left, y),
                            on,
                            choice,
                            scale::SMALL,
                            p.ink,
                            FieldLook::new(p),
                            p,
                            width,
                        );
                        y = r.rect.bottom();
                    }
                }
                FieldValue::Text(text) => {
                    let shown = if field.required {
                        label.clone()
                    } else {
                        format!("{label} (optional)")
                    };
                    let ty = scale::SMALLER.weight(550.0);
                    let galley =
                        paint::layout(ui.painter(), paint::job(&shown, ty, label_color, width));
                    let h = galley.size().y;
                    ui.painter().galley(pos2(left, y), galley, label_color);
                    y += h + 4.0;
                    let rect = Rect::from_min_size(pos2(left, y), vec2(width, 32.0));
                    if let Some(choices) = &field.choices {
                        let mut options = Vec::new();
                        if !field.required {
                            options.push(String::new());
                        }
                        options.extend(choices.iter().cloned());
                        if text.is_empty()
                            && field.required
                            && let Some(first) = choices.first()
                        {
                            // A required select starts on its first choice.
                            *text = first.clone();
                        }
                        controls::select(ui, fid, rect, text, &options, &shown, p, self.images);
                    } else {
                        controls::text_field(
                            ui,
                            fid,
                            rect,
                            text,
                            "",
                            scale::CAPTION,
                            12.0,
                            FieldLook::new(p),
                            p,
                            &shown,
                        );
                    }
                    y = rect.bottom();
                }
            }
            if let Some(hint) = field.description.as_deref().filter(|d| !d.is_empty()) {
                y += 4.0;
                let galley = paint::layout(
                    ui.painter(),
                    paint::job(hint, scale::CAPTION, p.ink_muted, width),
                );
                widgets::label(ui, Rect::from_min_size(pos2(left, y), galley.size()), hint);
                let h = galley.size().y;
                ui.painter().galley(pos2(left, y), galley, p.ink_muted);
                y += h;
            }
        }
        y + 14.0
    }

    /// `.step--waiting`: a step that waits for someone else, by name.
    fn waiting(&self, ui: &mut Ui, card: Card) {
        let p = self.palette;
        let (who, text) = match card {
            Card::Approval(a) => (
                a.waiting_for.as_deref(),
                format!("to approve a change in {}", a.service),
            ),
            Card::Question(q) => (
                q.waiting_for.as_deref(),
                format!("to answer a question from {}", q.service),
            ),
        };
        let line = format!("Waiting for {} {text}", who.unwrap_or("someone"));
        let width = ui.available_width();
        let (row, response) = ui.allocate_exact_size(vec2(width, 3.0 + 19.5 + 3.0), Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &line));
        let painter = ui.painter();
        self.images.icon_at(
            painter,
            pos2(row.left() + 8.0, row.center().y),
            16.0,
            Icon::HourglassMedium,
            p.attention,
        );
        let galley = paint::layout(
            painter,
            paint::line_job(&line, scale::SMALLER, p.ink_muted, width - 24.0),
        );
        painter.galley(
            pos2(row.left() + 24.0, row.center().y - galley.size().y / 2.0),
            galley,
            p.ink_muted,
        );
    }
}

/// `.thinking--approval`: the hand in an attention chip, what the answer
/// waits for, and Review. True when Review was clicked.
pub fn waiting_line(ui: &mut Ui, text: &str, p: &Palette, images: &Images) -> bool {
    let width = ui.available_width();
    let (row, _) = ui.allocate_exact_size(vec2(width, 12.0 + 28.0 + 12.0), Sense::hover());
    let painter = ui.painter().clone();
    let center = pos2(row.left() + 14.0, row.center().y);
    painter.circle_filled(center, 14.0, p.attention_wash());
    painter.circle_stroke(center, 13.5, egui::Stroke::new(1.0, p.attention_edge()));
    images.icon_at(&painter, center, 14.0, Icon::HandPalm, p.attention_ink);
    let ty = scale::SMALL;
    let galley = paint::layout(&painter, paint::job(text, ty, p.ink, f32::INFINITY));
    let pos = pos2(
        row.left() + 28.0 + 12.0,
        row.center().y - galley.size().y / 2.0,
    );
    widgets::label(ui, Rect::from_min_size(pos, galley.size()), text);
    let text_w = galley.size().x;
    painter.galley(pos, galley, p.ink);
    let link = paint::layout(
        &painter,
        paint::job("Review", ty.weight(550.0), p.ink, f32::INFINITY),
    );
    let link_pos = pos2(pos.x + text_w + 4.0 + 4.0, pos.y);
    let rect = Rect::from_min_size(link_pos, link.size());
    let response = ui.interact(rect, Id::new("chat-review"), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, true, "Review"));
    let underline = if response.hovered() {
        p.ink
    } else {
        p.line_strong
    };
    painter.hline(
        rect.x_range(),
        rect.bottom() - 2.0,
        egui::Stroke::new(1.0, underline),
    );
    painter.galley(link_pos, link, p.ink);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}
