//! The web's dialogs over the chat page: retrying a message
//! (`messages/_retry_button`), sharing a chat (`chats/share_links/_dialog`),
//! deleting one (the sidebar's "Are you sure?"), and a file the composer
//! won't take (`chats/_unsupported_attachment_modal`).

use egui::{Id, Rect, pos2, vec2};

use super::controls::{self, Button, Kind, Size};
use super::icons::{Icon, Images};
use super::paint;
use super::state::{ChatAction, ChatState, Command, Dialog};
use super::tokens::{Palette, Type, scale};
use super::widgets;

/// What a dialog asked of the page.
pub enum Outcome {
    Commands(Vec<Command>),
    Copy(String),
}

/// Draw the open dialog, if any.
pub fn show(
    ctx: &egui::Context,
    state: &mut ChatState,
    p: &Palette,
    images: &Images,
) -> Vec<Outcome> {
    let Some(dialog) = state.dialog.clone() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut close = false;
    let id = Id::new("chat-dialog");
    let open = match dialog {
        Dialog::Retry {
            chat,
            message,
            user,
        } => {
            let (title, text) = if user {
                (
                    "Retry this question?",
                    "The assistant will answer this question again. Any later messages will be removed.",
                )
            } else {
                (
                    "Retry this answer?",
                    "Everything after the question, including this reply, will be removed, and the assistant will answer it again.",
                )
            };
            let width = 448.0f32.min(ctx.content_rect().width() - 32.0) - 48.0;
            let header = header_height(ctx, width, title, &[(text, false)]);
            controls::modal(
                ctx,
                id,
                448.0,
                24.0 + header + 20.0 + 4.0 + 40.0 + 24.0,
                p,
                |ui, inner| {
                    controls::dialog_header(
                        ui,
                        inner.min,
                        inner.width(),
                        title,
                        &[(text, false)],
                        p,
                    );
                    let y = inner.bottom() - 40.0;
                    let retry = Button::new(Kind::Primary, Size::Regular, "Retry");
                    let cancel = Button::new(Kind::Subtle, Size::Regular, "Cancel");
                    let rw = retry.width(ui);
                    let cw = cancel.width(ui);
                    if retry
                        .show_at(ui, id.with("retry"), pos2(inner.right() - rw, y), p, images)
                        .clicked()
                    {
                        out.push(Outcome::Commands(
                            state.act(chat, ChatAction::Retry(message)),
                        ));
                        close = true;
                    }
                    if cancel
                        .show_at(
                            ui,
                            id.with("cancel"),
                            pos2(inner.right() - rw - 8.0 - cw, y),
                            p,
                            images,
                        )
                        .clicked()
                    {
                        close = true;
                    }
                },
            )
        }
        Dialog::Delete { chat } => {
            let title = "Are you sure?";
            let name = state
                .summary(chat)
                .map(|c| c.title.clone())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "This chat".into());
            let text = format!("{name} will be deleted.");
            let parts = [(text.as_str(), false)];
            let width = 448.0f32.min(ctx.content_rect().width() - 32.0) - 48.0;
            let header = header_height(ctx, width, title, &parts);
            let busy = state
                .acting
                .as_ref()
                .is_some_and(|(c, a)| *c == chat && *a == ChatAction::Delete);
            controls::modal(
                ctx,
                id,
                448.0,
                24.0 + header + 20.0 + 4.0 + 40.0 + 24.0,
                p,
                |ui, inner| {
                    controls::dialog_header(ui, inner.min, inner.width(), title, &parts, p);
                    let y = inner.bottom() - 40.0;
                    let delete = Button::new(Kind::Error, Size::Regular, "Delete").enabled(!busy);
                    let cancel = Button::new(Kind::Subtle, Size::Regular, "Cancel");
                    let dw = delete.width(ui);
                    let cw = cancel.width(ui);
                    if delete
                        .show_at(
                            ui,
                            id.with("delete"),
                            pos2(inner.right() - dw, y),
                            p,
                            images,
                        )
                        .clicked()
                    {
                        out.push(Outcome::Commands(state.act(chat, ChatAction::Delete)));
                    }
                    if cancel
                        .show_at(
                            ui,
                            id.with("cancel"),
                            pos2(inner.right() - dw - 8.0 - cw, y),
                            p,
                            images,
                        )
                        .clicked()
                    {
                        close = true;
                    }
                },
            )
        }
        Dialog::Unsupported { name } => {
            let title = "That file can't be attached";
            let parts = [
                (name.as_str(), true),
                (
                    " isn't a supported file type. Try a PDF, an image, a text file, or an Office document.",
                    false,
                ),
            ];
            let width = 448.0f32.min(ctx.content_rect().width() - 32.0) - 48.0;
            let header = header_height(ctx, width, title, &parts);
            controls::modal(
                ctx,
                id,
                448.0,
                24.0 + header + 20.0 + 4.0 + 40.0 + 24.0,
                p,
                |ui, inner| {
                    controls::dialog_header(ui, inner.min, inner.width(), title, &parts, p);
                    let ok = Button::new(Kind::Primary, Size::Regular, "OK");
                    let w = ok.width(ui);
                    if ok
                        .show_at(
                            ui,
                            id.with("ok"),
                            pos2(inner.right() - w, inner.bottom() - 40.0),
                            p,
                            images,
                        )
                        .clicked()
                    {
                        close = true;
                    }
                },
            )
        }
        Dialog::Share { chat, copied } => {
            share(ctx, state, chat, copied, p, images, &mut out, &mut close)
        }
    };
    if !open || close {
        state.dialog = None;
    }
    out
}

/// A painter to lay text out with before anything is drawn.
fn measure(ctx: &egui::Context) -> egui::Painter {
    ctx.layer_painter(egui::LayerId::background())
}

fn header_height(ctx: &egui::Context, width: f32, title: &str, text: &[(&str, bool)]) -> f32 {
    controls::dialog_header_height(&measure(ctx), width, title, text)
}

/// `chats/share_links/_dialog`: private, with "Create public link", or
/// shared, with the link to copy, its expiry, and "Stop sharing".
#[allow(clippy::too_many_arguments)]
fn share(
    ctx: &egui::Context,
    state: &mut ChatState,
    chat: u64,
    copied: bool,
    p: &Palette,
    images: &Images,
    out: &mut Vec<Outcome>,
    close: &mut bool,
) -> bool {
    let id = Id::new("chat-share-dialog");
    let link = state.summary(chat).and_then(|c| c.share.clone());
    let busy = state.acting.as_ref().is_some_and(|(c, _)| *c == chat);
    let title = "Share this chat";
    let text = if link.is_some() {
        "Anyone with this link, inside or outside your company, can read this conversation, its citations, and any messages you add later."
    } else {
        "A public link makes the whole conversation, including citations and document titles, readable by anyone who has it, even people outside your company."
    };
    let note = if link.is_some() {
        "You can stop sharing at any time, here or in Settings."
    } else {
        "The link expires after 30 days, and you can stop sharing at any time, here or in Settings."
    };
    let width = 512.0f32.min(ctx.content_rect().width() - 32.0) - 48.0;
    let header = header_height(ctx, width, title, &[(text, false)]);
    let note_h = paint::layout(
        &measure(ctx),
        paint::job(note, scale::SMALLER, p.ink_muted, width),
    )
    .size()
    .y;
    let body = if link.is_some() {
        36.0 + 10.0 + 18.0 + 10.0 + note_h
    } else {
        18.0 + 10.0 + note_h
    };
    let height = 24.0 + header + 20.0 + body + 24.0 + 40.0 + 24.0;
    controls::modal(ctx, id, 512.0, height, p, |ui, inner| {
        controls::dialog_header(ui, inner.min, inner.width(), title, &[(text, false)], p);
        let mut y = inner.top() + header + 20.0;
        let painter = ui.painter().clone();
        if let Some(link) = &link {
            // `.share-link__field`: the link, and Copy.
            let field = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), 36.0));
            painter.rect_filled(field, 10.0, p.canvas_sunken);
            painter.rect_stroke(
                field.shrink(0.5),
                9.5,
                egui::Stroke::new(1.0, p.line),
                egui::StrokeKind::Middle,
            );
            images.icon_at(
                &painter,
                pos2(field.left() + 12.0 + 8.0, field.center().y),
                16.0,
                Icon::GlobeSimple,
                p.ink_faint,
            );
            let copy = Button::new(
                Kind::Primary,
                Size::Tiny,
                if copied { "Copied" } else { "Copy" },
            )
            .icon(if copied { Icon::Check } else { Icon::Copy })
            .name(if copied { "Copied" } else { "Copy link" });
            let cw = copy.width(ui);
            let code_left = field.left() + 12.0 + 16.0 + 10.0;
            let code_w = field.right() - 6.0 - cw - 10.0 - code_left;
            let code = paint::layout(
                &painter,
                paint::line_job(
                    &link.url,
                    Type::mono(12.0, 18.0).tracking(0.0),
                    p.ink,
                    code_w,
                ),
            );
            widgets::label(
                ui,
                Rect::from_min_size(pos2(code_left, field.center().y - 9.0), vec2(code_w, 18.0)),
                &link.url,
            );
            painter.galley(
                pos2(code_left, field.center().y - code.size().y / 2.0),
                code,
                p.ink,
            );
            if copy
                .show_at(
                    ui,
                    id.with("copy"),
                    pos2(field.right() - 6.0 - cw, field.center().y - 12.0),
                    p,
                    images,
                )
                .clicked()
            {
                out.push(Outcome::Copy(link.url.clone()));
                state.dialog = Some(Dialog::Share { chat, copied: true });
            }
            y += 36.0 + 10.0;
            // `.share-link__meta`: Public, and when it expires.
            let w = controls::state_chip(
                &painter,
                pos2(inner.left(), y + 9.0),
                "Public",
                Some(p.positive()),
                p,
            );
            widgets::label(
                ui,
                Rect::from_min_size(pos2(inner.left(), y), vec2(w, 18.0)),
                "Public",
            );
            if let Some(expires) = link.expires_at.as_deref().and_then(expiry) {
                let text = format!("Expires {expires}");
                let galley = paint::layout(
                    &painter,
                    paint::job(&text, scale::MICRO, p.ink_faint, f32::INFINITY),
                );
                let pos = pos2(inner.left() + w + 12.0, y + 9.0 - galley.size().y / 2.0);
                widgets::label(ui, Rect::from_min_size(pos, galley.size()), &text);
                painter.galley(pos, galley, p.ink_faint);
            }
            y += 18.0 + 10.0;
        } else {
            let w = controls::state_chip(&painter, pos2(inner.left(), y + 9.0), "Private", None, p);
            widgets::label(
                ui,
                Rect::from_min_size(pos2(inner.left(), y), vec2(w, 18.0)),
                "Private",
            );
            y += 18.0 + 10.0;
        }
        let galley = paint::layout(
            &painter,
            paint::job(note, scale::SMALLER, p.ink_muted, inner.width()),
        );
        widgets::label(
            ui,
            Rect::from_min_size(pos2(inner.left(), y), galley.size()),
            note,
        );
        painter.galley(pos2(inner.left(), y), galley, p.ink_muted);
        let y = inner.bottom() - 40.0;
        if link.is_some() {
            let stop =
                Button::new(Kind::SubtleNegative, Size::Regular, "Stop sharing").enabled(!busy);
            if stop
                .show_at(ui, id.with("stop"), pos2(inner.left(), y), p, images)
                .clicked()
            {
                out.push(Outcome::Commands(state.act(chat, ChatAction::Unshare)));
            }
            let done = Button::new(Kind::Subtle, Size::Regular, "Done");
            let w = done.width(ui);
            if done
                .show_at(ui, id.with("done"), pos2(inner.right() - w, y), p, images)
                .clicked()
            {
                *close = true;
            }
        } else {
            let create =
                Button::new(Kind::Primary, Size::Regular, "Create public link").enabled(!busy);
            let cancel = Button::new(Kind::Subtle, Size::Regular, "Cancel");
            let (w, cw) = (create.width(ui), cancel.width(ui));
            if create
                .show_at(ui, id.with("create"), pos2(inner.right() - w, y), p, images)
                .clicked()
            {
                out.push(Outcome::Commands(state.act(chat, ChatAction::Share)));
            }
            if cancel
                .show_at(
                    ui,
                    id.with("cancel"),
                    pos2(inner.right() - w - 8.0 - cw, y),
                    p,
                    images,
                )
                .clicked()
            {
                *close = true;
            }
        }
    })
}

/// "November 1, 2026", as `l(date, format: :long)` writes it.
fn expiry(at: &str) -> Option<String> {
    let date = at
        .parse::<jiff::Timestamp>()
        .ok()?
        .to_zoned(jiff::tz::TimeZone::system())
        .date();
    Some(date.strftime("%B %-d, %Y").to_string())
}
