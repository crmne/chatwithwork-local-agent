//! The conversation (`conversation.css`, `activity.css`): your questions as
//! bubbles on the right, the tool work folded into one live line, the
//! answers in Markdown with their sources, notices, and "Thinking" while
//! nothing else says what's happening. Each thing rises in as it appears.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use egui::{Id, Pos2, Rect, Sense, Ui, UiBuilder, pos2, vec2};

use cww::tui::chat::{Entry, Source, Step};

use super::icons::{Icon, Images};
use super::markdown::{self, Block};
use super::mcp_app::McpAppSlot;
use super::paint;
use super::prose::{self, Clicked, Prose};
use super::state::ChatState;
use super::tokens::{self, Palette, Type, scale};
use super::widgets;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    OpenUrl(String),
    /// Open this chat in the browser (a tool's view, for now).
    OpenChat,
    Copy(String),
}

/// What the conversation remembers between frames.
#[derive(Default)]
pub struct ConversationView {
    /// Activities opened or closed by hand, by id.
    expanded: HashMap<u64, bool>,
    /// The answer whose sources menu is open.
    pub sources_open: Option<u64>,
    /// When each thing first showed, for rising in.
    seen: HashMap<String, f64>,
    /// Parsed answers, by message, with a hash of what they were parsed from.
    parsed: HashMap<u64, (u64, Arc<Vec<Block>>)>,
}

impl ConversationView {
    /// A different chat: nothing carries over.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn blocks(&mut self, id: u64, content: &str, sources: &[Source]) -> Arc<Vec<Block>> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        for s in sources {
            s.url.hash(&mut hasher);
        }
        let hash = hasher.finish();
        match self.parsed.get(&id) {
            Some((h, blocks)) if *h == hash => Arc::clone(blocks),
            _ => {
                let blocks = Arc::new(markdown::parse(content, sources));
                self.parsed.insert(id, (hash, Arc::clone(&blocks)));
                blocks
            }
        }
    }

    /// How far `key` has risen in: 0 when it first shows, 1 after 220 ms.
    fn rise(&mut self, key: String, now: f64) -> f32 {
        let since = *self.seen.entry(key).or_insert(now);
        ((now - since) as f32 / tokens::BASE).clamp(0.0, 1.0)
    }
}

pub struct Conversation<'a> {
    pub palette: &'a Palette,
    pub images: &'a Images,
    pub time: f64,
    /// Everything at rest, for snapshots.
    pub still: bool,
}

/// One thing to draw, in order.
enum Item<'a> {
    Entry(&'a Entry),
    /// Answer text that streamed in before the chat had the message.
    Streamed(u64, &'a str),
    Pending(&'a str),
    Thinking,
}

impl Conversation<'_> {
    pub fn show(
        &self,
        ui: &mut Ui,
        state: &ChatState,
        view: &mut ConversationView,
    ) -> (Vec<Event>, bool) {
        let mut events = Vec::new();
        let mut animating = false;
        let entries: &[Entry] = state.transcript.as_ref().map_or(&[], |t| &t.entries);
        let mut items: Vec<Item> = entries.iter().map(Item::Entry).collect();
        for (id, text) in &state.streamed {
            let known = entries
                .iter()
                .any(|e| matches!(e, Entry::Assistant { id: known, .. } if known == id));
            if !known && !text.is_empty() {
                items.push(Item::Streamed(*id, text));
            }
        }
        if let Some(question) = &state.pending_question {
            items.push(Item::Pending(question));
        }
        let working = state.working();
        let live_activity = entries
            .iter()
            .any(|e| matches!(e, Entry::Activity { pending: true, .. }));
        if working && !live_activity {
            items.push(Item::Thinking);
        }
        // The latest answer with text keeps its actions in view.
        let latest = items.iter().rposition(|item| match item {
            Item::Entry(Entry::Assistant { id, content, .. }) => {
                !content.trim().is_empty()
                    || state.streamed.get(id).is_some_and(|s| !s.trim().is_empty())
            }
            Item::Streamed(..) => true,
            _ => false,
        });
        let last_answer = items.iter().rposition(|item| {
            matches!(
                item,
                Item::Entry(Entry::Assistant { .. }) | Item::Streamed(..)
            )
        });
        let streaming = working && !state.stopping;
        // "Writing" once the answer streams in.
        let writing = state.streamed.values().any(|s| !s.is_empty())
            || matches!(entries.last(), Some(Entry::Assistant { content, .. }) if !content.trim().is_empty());
        let count = items.len();
        for (i, item) in items.into_iter().enumerate() {
            let key = match &item {
                Item::Entry(Entry::User { id, .. }) => format!("user-{id}"),
                Item::Entry(Entry::Assistant { id, .. }) | Item::Streamed(id, _) => {
                    format!("answer-{id}")
                }
                Item::Entry(Entry::Activity { id, .. }) => format!("activity-{id}"),
                Item::Entry(Entry::Notice { text, .. }) => format!("notice-{text}"),
                Item::Entry(Entry::Unknown) => format!("unknown-{i}"),
                Item::Pending(_) => format!("user-pending-{}", entries.len()),
                Item::Thinking => "thinking".into(),
            };
            let rise = if self.still {
                1.0
            } else {
                view.rise(key.clone(), self.time)
            };
            animating |= rise < 1.0;
            let eased = tokens::ease_snap(rise);
            // `message-in`: from transparent and 6 lower, without moving
            // what comes after.
            let top = ui.cursor().top();
            let width = ui.available_width();
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        pos2(ui.cursor().left(), top + 6.0 * (1.0 - eased)),
                        vec2(width, f32::INFINITY),
                    ))
                    .layout(egui::Layout::top_down(egui::Align::Min))
                    .id_salt(&key),
            );
            child.set_opacity(eased);
            let caret = streaming && Some(i) == last_answer && i + 2 >= count;
            match item {
                Item::Entry(Entry::User {
                    id,
                    content,
                    author,
                }) => {
                    self.user(
                        &mut child,
                        Id::new(("user", *id)),
                        content,
                        author.as_deref(),
                        &mut events,
                    );
                }
                Item::Pending(text) => {
                    self.user(&mut child, Id::new("user-pending"), text, None, &mut events)
                }
                Item::Entry(Entry::Assistant {
                    id,
                    content,
                    sources,
                }) => {
                    // The chat's text once it has it, what streamed in until then.
                    let text = if content.trim().is_empty() {
                        state.streamed.get(id).map_or("", String::as_str)
                    } else {
                        content.as_str()
                    };
                    if !text.trim().is_empty() || !sources.is_empty() {
                        let blocks = view.blocks(*id, text, sources);
                        self.answer(
                            &mut child,
                            *id,
                            text,
                            &blocks,
                            sources,
                            Some(i) == latest,
                            caret,
                            view,
                            &mut events,
                        );
                    }
                }
                Item::Streamed(id, text) => {
                    let blocks = view.blocks(id, text, &[]);
                    self.answer(
                        &mut child,
                        id,
                        text,
                        &blocks,
                        &[],
                        Some(i) == latest,
                        caret,
                        view,
                        &mut events,
                    );
                }
                Item::Entry(Entry::Activity {
                    id,
                    title,
                    details,
                    progress,
                    services,
                    pending,
                    steps,
                }) => {
                    let live_progress = if *pending {
                        state.progress.as_deref().or(progress.as_deref())
                    } else {
                        None
                    };
                    let detail = live_progress.or(details.as_deref());
                    animating |= self.activity(
                        &mut child,
                        *id,
                        title,
                        detail,
                        services,
                        *pending,
                        steps,
                        view,
                        &mut events,
                    );
                }
                Item::Entry(Entry::Notice { tone, text }) => self.notice(&mut child, tone, text),
                Item::Entry(Entry::Unknown) => {}
                Item::Thinking => {
                    let label = if state.stopping {
                        "Stopping..."
                    } else if writing {
                        "Writing"
                    } else {
                        "Thinking"
                    };
                    self.thinking(&mut child, label, state.stopping);
                }
            }
            let height = child.min_rect().height() - 6.0 * (1.0 - eased);
            ui.allocate_exact_size(vec2(width, height.max(0.0)), Sense::hover());
        }
        (events, animating)
    }

    fn user(
        &self,
        ui: &mut Ui,
        id: Id,
        content: &str,
        author: Option<&str>,
        events: &mut Vec<Event>,
    ) {
        let p = self.palette;
        let width = ui.available_width();
        ui.add_space(12.0);
        if let Some(author) = author {
            let galley = paint::layout(
                ui.painter(),
                paint::job(author, scale::MICRO, p.ink_faint, f32::INFINITY),
            );
            let (r, _) = ui.allocate_exact_size(vec2(width, galley.size().y + 4.0), Sense::hover());
            ui.painter().galley(
                pos2(r.right() - galley.size().x, r.top()),
                galley,
                p.ink_faint,
            );
        }
        let max = (width * 0.85).min(576.0);
        let galley = paint::layout(
            ui.painter(),
            paint::job(content.trim_end(), scale::BODY, p.ink, max - 32.0),
        );
        let size = galley.size() + vec2(32.0, 20.0);
        let (row, response) = ui.allocate_exact_size(vec2(width, size.y), Sense::hover());
        let bubble = Rect::from_min_size(pos2(row.right() - size.x, row.top()), size);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, content));
        let painter = ui.painter();
        // Corners 14, but 6 at the bottom right, towards you.
        let outline = paint::rounded_outline(bubble, [14.0, 14.0, tokens::RADIUS_TAG, 14.0], 10);
        painter.add(egui::Shape::convex_polygon(
            outline.clone(),
            p.surface_raised,
            egui::Stroke::NONE,
        ));
        let inner = paint::rounded_outline(
            bubble.shrink(0.5),
            [13.5, 13.5, tokens::RADIUS_TAG - 0.5, 13.5],
            10,
        );
        painter.add(egui::Shape::closed_line(
            inner,
            egui::Stroke::new(1.0, p.line),
        ));
        painter.galley(bubble.min + vec2(16.0, 10.0), galley, p.ink);
        // Copy, on hover.
        ui.add_space(6.0);
        let hovered =
            ui.rect_contains_pointer(Rect::from_min_max(row.min, row.max + vec2(0.0, 34.0)));
        let (actions, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
        let shown = widgets::hover(ui, id.with("actions"), hovered, tokens::FAST);
        if shown > 0.0 {
            let rect = Rect::from_min_size(
                pos2(actions.right() + 6.0 - 28.0, actions.top()),
                vec2(28.0, 28.0),
            );
            let mut faded = ui.new_child(UiBuilder::new().max_rect(rect));
            faded.set_opacity(shown);
            if self
                .action(&mut faded, id.with("copy"), rect, Icon::Copy, "Copy")
                .clicked()
            {
                events.push(Event::Copy(content.to_string()));
            }
        }
        ui.add_space(4.0);
    }

    fn action(&self, ui: &mut Ui, id: Id, rect: Rect, icon: Icon, label: &str) -> egui::Response {
        let p = self.palette;
        widgets::IconButton {
            icon,
            icon_size: 15.0,
            label,
            tooltip: Some(label),
            color: p.ink_faint,
            hover_color: p.ink,
            wash: 6.0,
            radius: tokens::RADIUS_CONTROL,
            enabled: true,
        }
        .show(ui, id, rect, self.images, p)
    }

    #[allow(clippy::too_many_arguments)]
    fn answer(
        &self,
        ui: &mut Ui,
        id: u64,
        text: &str,
        blocks: &[Block],
        sources: &[Source],
        latest: bool,
        caret: bool,
        view: &mut ConversationView,
        events: &mut Vec<Event>,
    ) {
        let p = self.palette;
        let width = ui.available_width();
        let top = ui.cursor().top();
        ui.add_space(12.0);
        let prose = Prose {
            palette: p,
            images: self.images,
            sources,
            caret,
            time: self.time,
            id: Id::new(("answer", id)),
        };
        let spacing = std::mem::replace(&mut ui.spacing_mut().item_spacing, vec2(0.0, 0.0));
        match prose.show(ui, blocks) {
            Some(Clicked::Link(url)) => events.push(Event::OpenUrl(url)),
            Some(Clicked::Source(i)) => {
                if let Some(url) = sources.get(i).and_then(|s| s.url.clone()) {
                    events.push(Event::OpenUrl(url));
                }
            }
            Some(Clicked::Copy(code)) => events.push(Event::Copy(code)),
            None => {}
        }
        ui.spacing_mut().item_spacing = spacing;
        // The footer: actions, and the sources.
        ui.add_space(8.0);
        let (footer, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
        let hovered =
            ui.rect_contains_pointer(Rect::from_min_max(pos2(footer.left(), top), footer.max));
        let shown = widgets::hover(
            ui,
            Id::new(("answer-actions", id)),
            latest || hovered,
            tokens::FAST,
        );
        if shown > 0.0 {
            let rect =
                Rect::from_min_size(pos2(footer.left() - 6.0, footer.top()), vec2(28.0, 28.0));
            let mut faded = ui.new_child(UiBuilder::new().max_rect(rect));
            faded.set_opacity(shown);
            if self
                .action(
                    &mut faded,
                    Id::new(("answer-copy", id)),
                    rect,
                    Icon::Copy,
                    "Copy",
                )
                .clicked()
            {
                events.push(Event::Copy(text.to_string()));
            }
        }
        if !sources.is_empty() {
            self.sources(ui, footer, id, sources, view, events);
        }
        ui.add_space(8.0);
    }

    /// The Sources pill, and its menu of numbered sources.
    fn sources(
        &self,
        ui: &mut Ui,
        footer: Rect,
        id: u64,
        sources: &[Source],
        view: &mut ConversationView,
        events: &mut Vec<Event>,
    ) {
        let p = self.palette;
        let label = paint::layout(
            ui.painter(),
            paint::job(
                "Sources",
                scale::CAPTION.weight(500.0),
                p.ink_muted,
                f32::INFINITY,
            ),
        );
        let count = sources.len().to_string();
        let count_galley = paint::layout(
            ui.painter(),
            paint::job(&count, Type::mono(11.0, 15.4), p.ink_faint, f32::INFINITY),
        );
        let shown = sources.len().min(3);
        let avatars = 20.8 + (shown.saturating_sub(1)) as f32 * (20.8 - 5.6);
        let width = 4.0 + avatars + 8.0 + label.size().x + 8.0 + count_galley.size().x + 10.0;
        let rect = Rect::from_min_size(
            pos2(footer.right() - width, footer.top()),
            vec2(width, 28.0),
        );
        let pill_id = Id::new(("sources", id));
        let response = ui.interact(rect, pill_id, Sense::click());
        let noun = if sources.len() == 1 {
            "source"
        } else {
            "sources"
        };
        let open = view.sources_open == Some(id);
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("{} {noun}", sources.len()),
            );
            info.selected = Some(open);
            info
        });
        let t = widgets::hover(ui, pill_id, response.hovered() || open, tokens::FAST);
        let painter = ui.painter();
        let ring = tokens::lerp_rgb(p.line, p.line_strong, t);
        painter.rect_stroke(
            rect.shrink(0.5),
            14.0,
            egui::Stroke::new(1.0, ring),
            egui::StrokeKind::Middle,
        );
        let mut x = rect.left() + 4.0;
        for source in sources.iter().take(3) {
            let center = pos2(x + 10.4, rect.center().y);
            painter.circle_filled(center, 10.4 + 2.0, p.canvas);
            painter.circle_filled(center, 10.4, p.surface);
            painter.circle_stroke(center, 9.9, egui::Stroke::new(1.0, p.line_strong));
            self.images.icon_at(
                painter,
                center,
                12.9,
                prose::source_icon(source),
                p.ink_muted,
            );
            x += 20.8 - 5.6;
        }
        let color = tokens::lerp_rgb(p.ink_muted, p.ink, t);
        let lx = rect.left() + 4.0 + avatars + 8.0;
        painter.galley(
            pos2(lx, rect.center().y - label.size().y / 2.0),
            label,
            color,
        );
        painter.galley(
            pos2(
                rect.right() - 10.0 - count_galley.size().x,
                rect.center().y - count_galley.size().y / 2.0,
            ),
            count_galley,
            p.ink_faint,
        );
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            view.sources_open = if open { None } else { Some(id) };
        }
        if view.sources_open == Some(id) {
            self.sources_menu(ui, rect, pill_id, sources, view, events);
        }
    }

    fn sources_menu(
        &self,
        ui: &mut Ui,
        anchor: Rect,
        id: Id,
        sources: &[Source],
        view: &mut ConversationView,
        events: &mut Vec<Event>,
    ) {
        let p = *self.palette;
        let screen = ui.ctx().content_rect();
        let width = 352.0f32.min(screen.width() - 32.0);
        let row_h = 32.0;
        let height = (5.0 + 28.0 + sources.len() as f32 * (row_h + 2.0) + 5.0).min(320.0);
        let left = (anchor.right() - width).max(screen.left() + 16.0);
        let top = anchor.top() - 6.0 - height;
        let rect = Rect::from_min_size(pos2(left, top), vec2(width, height));
        let mut close = false;
        let area = egui::Area::new(id.with("menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ui.ctx(), |ui| {
                let painter = ui.painter();
                paint::menu_shadow(painter, rect, tokens::RADIUS_CARD, &p);
                painter.rect_filled(rect, tokens::RADIUS_CARD, p.surface_raised);
                painter.rect_stroke(
                    rect.shrink(0.5),
                    tokens::RADIUS_CARD,
                    egui::Stroke::new(1.0, p.line),
                    egui::StrokeKind::Middle,
                );
                let noun = if sources.len() == 1 {
                    "source"
                } else {
                    "sources"
                };
                let title = format!("{} {noun}", sources.len());
                let galley = paint::layout(
                    painter,
                    paint::job(&title, Type::mono(11.0, 16.0), p.ink_faint, f32::INFINITY),
                );
                painter.galley(rect.min + vec2(5.0 + 12.0, 5.0 + 6.0), galley, p.ink_faint);
                let (_, body) = ui.allocate_exact_size(rect.size(), Sense::hover());
                let _ = body;
                let mut y = rect.top() + 5.0 + 28.0;
                for (n, source) in sources.iter().enumerate() {
                    let row =
                        Rect::from_min_size(pos2(rect.left() + 5.0, y), vec2(width - 10.0, row_h));
                    let row_id = id.with(("source", n));
                    let response = ui.interact(row, row_id, Sense::click());
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Link,
                            true,
                            format!("{}. {}", n + 1, source.title),
                        )
                    });
                    let t = widgets::hover(ui, row_id, response.hovered(), tokens::INSTANT);
                    let painter = ui.painter();
                    if t > 0.0 {
                        painter.rect_filled(row, tokens::RADIUS_CONTROL - 2.0, p.ink_wash(6.0 * t));
                    }
                    let number = paint::layout(
                        painter,
                        paint::job(
                            &(n + 1).to_string(),
                            Type::mono(11.0, 16.0),
                            p.ink_faint,
                            f32::INFINITY,
                        ),
                    );
                    painter.galley(
                        pos2(row.left() + 12.0, row.center().y - number.size().y / 2.0),
                        number,
                        p.ink_faint,
                    );
                    self.images.icon_at(
                        painter,
                        pos2(row.left() + 32.0 + 8.0, row.center().y),
                        16.0,
                        prose::source_icon(source),
                        p.ink_muted,
                    );
                    let has_url = source.url.as_deref().is_some_and(|u| !u.is_empty());
                    let title_w = row.width() - 58.0 - if has_url { 28.0 } else { 8.0 };
                    let galley = paint::layout(
                        painter,
                        paint::line_job(&source.title, scale::SMALL, p.ink, title_w),
                    );
                    painter.galley(
                        pos2(row.left() + 58.0, row.center().y - galley.size().y / 2.0),
                        galley,
                        p.ink,
                    );
                    if has_url {
                        self.images.icon_at(
                            painter,
                            pos2(row.right() - 12.0 - 6.0, row.center().y),
                            12.0,
                            Icon::ArrowUpRight,
                            p.ink_faint,
                        );
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if response.clicked() {
                            events.push(Event::OpenUrl(source.url.clone().unwrap_or_default()));
                            close = true;
                        }
                    }
                    y += row_h + 2.0;
                }
            });
        let clicked_elsewhere = ui.input(|i| i.pointer.any_pressed())
            && ui
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|pos| !area.response.rect.contains(pos) && !anchor.contains(pos));
        if close || clicked_elsewhere || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            view.sources_open = None;
        }
    }

    /// One activity: the summary line, and its log when open. True while
    /// something in it moves.
    #[allow(clippy::too_many_arguments)]
    fn activity(
        &self,
        ui: &mut Ui,
        id: u64,
        title: &str,
        details: Option<&str>,
        services: &[String],
        pending: bool,
        steps: &[Step],
        view: &mut ConversationView,
        events: &mut Vec<Event>,
    ) -> bool {
        let p = self.palette;
        let mut animating = false;
        let width = ui.available_width();
        let has_app = steps.iter().any(|s| s.app.is_some());
        let open = *view.expanded.get(&id).unwrap_or(&has_app);
        let egui_id = Id::new(("activity", id));
        ui.add_space(8.0);
        // The wire wires in while the line is live and retracts after.
        let wire =
            ui.ctx()
                .animate_bool_with_easing(egui_id.with("wire"), pending, tokens::ease_snap);
        let wire = if (0.0..1.0).contains(&wire) && wire > 0.0 {
            animating = true;
            wire
        } else {
            wire
        };
        let names: Vec<&str> = services.iter().map(String::as_str).take(4).collect();
        let avatar_w = if names.is_empty() {
            0.0
        } else {
            24.0 + (names.len() - 1) as f32 * (24.0 - 6.4)
        };
        let ty = scale::SMALL;
        let title_galley = paint::layout(
            ui.painter(),
            paint::job(title, ty.weight(550.0), p.ink, f32::INFINITY),
        );
        let detail_text = details.map(|d| format!(" · {d}"));
        let detail_galley = detail_text
            .as_deref()
            .map(|d| paint::layout(ui.painter(), paint::job(d, ty, p.ink_faint, f32::INFINITY)));
        let wire_w = (6.0 + 24.0 + 4.0) * wire;
        let text_max = (width - 4.0 - wire_w - avatar_w - 10.0 - 8.0 - 12.0 - 10.0).max(40.0);
        let text_w = (title_galley.size().x + detail_galley.as_ref().map_or(0.0, |g| g.size().x))
            .min(text_max);
        let pill_w = 4.0
            + wire_w
            + avatar_w
            + if avatar_w > 0.0 { 10.0 } else { 0.0 }
            + text_w
            + 8.0
            + 12.0
            + 10.0;
        let (row, _) = ui.allocate_exact_size(vec2(width, 32.0), Sense::hover());
        let pill = Rect::from_min_size(pos2(row.left() - 4.0, row.top()), vec2(pill_w, 32.0));
        let response = ui.interact(pill, egui_id, Sense::click());
        let label = match details {
            Some(d) => format!("{title} · {d}"),
            None => title.to_string(),
        };
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label);
            info.selected = Some(open);
            info
        });
        let t = widgets::hover(ui, egui_id, response.hovered(), tokens::FAST);
        let painter = ui.painter().clone();
        if t > 0.0 {
            painter.rect_filled(pill, 16.0, p.ink_wash(5.0 * t));
        }
        let cy = pill.center().y;
        let mut x = pill.left() + 4.0;
        if wire > 0.0 {
            x += 6.0 * wire;
            self.wire(&painter, pos2(x, cy), 24.0 * wire, wire);
            x += 24.0 * wire + 4.0 * wire;
        }
        if !names.is_empty() {
            let mut ax = x;
            for (n, name) in names.iter().enumerate() {
                let center = pos2(ax + 12.0, cy);
                let active = pending && n + 1 == names.len();
                if active {
                    // `avatar-live`: a live ring breathing out.
                    let phase = ((self.time / 1.6).fract()) as f32;
                    let k = tokens::ease_glide(if phase < 0.5 {
                        phase * 2.0
                    } else {
                        2.0 - phase * 2.0
                    });
                    painter.circle_filled(
                        center,
                        12.0 + 2.0 + 3.0 * k,
                        p.live().gamma_multiply(0.3 * k),
                    );
                }
                widgets::avatar(&painter, center, 24.0, name, p, p.canvas);
                if active {
                    painter.circle_stroke(
                        center,
                        11.5,
                        egui::Stroke::new(1.0, p.live().gamma_multiply(0.0)),
                    );
                }
                ax += 24.0 - 6.4;
            }
            x += avatar_w + 10.0;
        }
        let text_color = tokens::lerp_rgb(p.ink, p.ink, t);
        let mut job = paint::job(title, ty.weight(550.0), text_color, f32::INFINITY);
        if let Some(d) = &detail_text {
            job.append(d, 0.0, ty.format(p.ink_faint));
        }
        job.wrap.max_width = text_w + 0.5;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let line = paint::layout(&painter, job);
        let pos = pos2(x, cy - line.size().y / 2.0);
        if pending {
            paint::shimmer(&painter, pos, &line, p.ink_faint, self.time);
        } else {
            painter.galley(pos, line, p.ink);
        }
        let turn =
            ui.ctx()
                .animate_bool_with_easing(egui_id.with("caret"), open, tokens::ease_snap);
        self.images.icon_rotated(
            &painter,
            pos2(pill.right() - 10.0 - 6.0, cy),
            12.0,
            Icon::CaretRight,
            p.ink_faint,
            turn * std::f32::consts::FRAC_PI_2,
        );
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            view.expanded.insert(id, !open);
        }
        if open {
            ui.add_space(6.0);
            let log_left = row.left() + 14.4;
            let log_top = ui.cursor().top();
            let mut log = ui.new_child(
                UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        pos2(log_left + 18.0, log_top + 4.0),
                        vec2(width - 14.4 - 18.0, f32::INFINITY),
                    ))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            log.spacing_mut().item_spacing = vec2(0.0, 6.0);
            for (n, step) in steps.iter().enumerate() {
                self.step(&mut log, step, pending);
                if let Some(app) = &step.app {
                    let slot = McpAppSlot {
                        app,
                        frame: None,
                        height: None,
                    };
                    log.add_space(2.0);
                    let out = slot.show(&mut log, Id::new(("mcp-app", id, n)), self.images, p);
                    if out.open_in_browser {
                        events.push(Event::OpenChat);
                    }
                    log.add_space(2.0);
                }
            }
            let log_height = log.min_rect().height() + 8.0;
            let rail = Rect::from_min_size(pos2(log_left, log_top), vec2(1.0, log_height));
            if pending {
                let mut mesh = egui::epaint::Mesh::default();
                mesh.colored_vertex(rail.left_top(), p.green);
                mesh.colored_vertex(rail.right_top(), p.green);
                mesh.colored_vertex(rail.left_bottom(), p.blue);
                mesh.colored_vertex(rail.right_bottom(), p.blue);
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(1, 3, 2);
                ui.painter().add(egui::Shape::mesh(mesh));
            } else {
                ui.painter().rect_filled(rail, 0.0, p.line);
            }
            ui.allocate_exact_size(vec2(width, log_height), Sense::hover());
            ui.add_space(12.0 - 8.0);
        }
        ui.add_space(8.0);
        animating
    }

    /// `.activity__wire`: a spark and a flowing green-to-blue wire.
    fn wire(&self, painter: &egui::Painter, left_center: Pos2, length: f32, opacity: f32) {
        let p = self.palette;
        if length > 0.5 {
            let rect = Rect::from_min_size(left_center - vec2(0.0, 1.0), vec2(length, 2.0));
            // Dashes 7 long, 3 apart, travelling 10 every 1.2 s.
            let shift = ((self.time / f64::from(tokens::WIRE)).fract() as f32) * 10.0;
            let mut x = rect.left() - 10.0 + shift;
            while x < rect.right() {
                let a = (x + 3.0).max(rect.left());
                let b = (x + 10.0).min(rect.right());
                if b > a {
                    let ca = tokens::lerp_rgb(p.green, p.blue, (a - rect.left()) / 24.0)
                        .gamma_multiply(opacity);
                    let cb = tokens::lerp_rgb(p.green, p.blue, (b - rect.left()) / 24.0)
                        .gamma_multiply(opacity);
                    let mut mesh = egui::epaint::Mesh::default();
                    mesh.colored_vertex(pos2(a, rect.top()), ca);
                    mesh.colored_vertex(pos2(b, rect.top()), cb);
                    mesh.colored_vertex(pos2(a, rect.bottom()), ca);
                    mesh.colored_vertex(pos2(b, rect.bottom()), cb);
                    mesh.add_triangle(0, 1, 2);
                    mesh.add_triangle(1, 3, 2);
                    painter.add(egui::Shape::mesh(mesh));
                }
                x += 10.0;
            }
        }
        // The spark, 5 to the left of the wire.
        let spark = left_center - vec2(5.0 - 3.0, 0.0);
        let phase = (self.time / 1.6).fract() as f32;
        paint::pulse_halo(painter, spark, 3.0, p.green.gamma_multiply(opacity), phase);
        painter.circle_filled(spark, 3.0, p.green.gamma_multiply(opacity));
    }

    fn step(&self, ui: &mut Ui, step: &Step, live: bool) {
        let p = self.palette;
        let width = ui.available_width();
        let ty = scale::SMALLER;
        let (row, response) = ui.allocate_exact_size(vec2(width, 3.0 + 19.5 + 3.0), Sense::hover());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &step.summary)
        });
        let painter = ui.painter();
        let icon_center = pos2(row.left() + 8.0, row.center().y);
        let pending = step.pending && live;
        if pending {
            // `step-pending`: a live ring breathing around the icon.
            let phase = (self.time / 1.6).fract() as f32;
            let k = tokens::ease_glide(if phase < 0.5 {
                phase * 2.0
            } else {
                2.0 - phase * 2.0
            });
            painter.circle_filled(
                icon_center,
                8.0 + 4.0 * k,
                p.live().gamma_multiply(0.25 * k),
            );
        }
        self.images
            .icon_at(painter, icon_center, 16.0, step_icon(step), p.ink_faint);
        // "4 found", in mono after the summary; the names on hover.
        let found = (!step.files.is_empty()).then(|| format!("{} found", step.files.len()));
        let found_galley = found.as_deref().map(|f| {
            paint::layout(
                painter,
                paint::job(f, Type::mono(11.0, 15.4), p.ink_faint, f32::INFINITY),
            )
        });
        let found_w = found_galley.as_ref().map_or(0.0, |g| g.size().x + 8.0);
        let galley = paint::layout(
            painter,
            paint::line_job(&step.summary, ty, p.ink_muted, width - 24.0 - found_w),
        );
        let pos = pos2(row.left() + 24.0, row.center().y - galley.size().y / 2.0);
        let summary_w = galley.size().x;
        if pending {
            paint::shimmer(painter, pos, &galley, p.ink_faint, self.time);
        } else {
            painter.galley(pos, galley, p.ink_muted);
        }
        if let Some(found_galley) = found_galley {
            let at = pos2(
                pos.x + summary_w + 8.0,
                row.center().y - found_galley.size().y / 2.0,
            );
            let rect = Rect::from_min_size(at, found_galley.size());
            painter.galley(at, found_galley, p.ink_faint);
            let files = step.files.join(", ");
            let hover = ui.interact(rect, Id::new(("step-files", &step.summary)), Sense::hover());
            hover.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &files));
            widgets::tooltip(ui, &hover, &files, p, true);
        }
    }

    fn notice(&self, ui: &mut Ui, tone: &str, text: &str) {
        let p = self.palette;
        let width = ui.available_width();
        ui.add_space(12.0);
        let color = if tone == "negative" {
            p.negative()
        } else {
            p.attention
        };
        let galley = paint::layout(
            ui.painter(),
            paint::job(text, scale::SMALL, p.ink, width - 32.0 - 18.0 - 12.0),
        );
        let height = 14.0 + galley.size().y.max(18.0) + 14.0;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
        let painter = ui.painter();
        painter.rect_filled(rect, tokens::RADIUS_CARD, p.surface);
        painter.rect_stroke(
            rect.shrink(0.5),
            tokens::RADIUS_CARD,
            egui::Stroke::new(1.0, p.line),
            egui::StrokeKind::Middle,
        );
        // The signal edge on the left, inside the corner.
        painter
            .with_clip_rect(Rect::from_min_size(rect.min, vec2(2.0, rect.height())))
            .rect_filled(rect, tokens::RADIUS_CARD, color);
        self.images.icon_at(
            painter,
            pos2(rect.left() + 16.0 + 9.0, rect.top() + 14.0 + 1.0 + 9.0),
            18.0,
            Icon::WarningCircle,
            color,
        );
        painter.galley(
            pos2(rect.left() + 16.0 + 18.0 + 12.0, rect.top() + 14.0),
            galley,
            p.ink,
        );
        ui.add_space(4.0);
    }

    fn thinking(&self, ui: &mut Ui, label: &str, stopping: bool) {
        let p = self.palette;
        let width = ui.available_width();
        let (row, response) =
            ui.allocate_exact_size(vec2(width, 12.0 + 28.0 + 12.0), Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label));
        let painter = ui.painter();
        let center = pos2(row.left() + 14.0, row.center().y);
        let turn = (self.time / 2.4).fract() as f32;
        painter.circle_filled(center, 14.0, p.surface);
        paint::conic_circle(painter, center, 16.0, 2.0, turn, stopping);
        self.images
            .mark(painter, Rect::from_center_size(center, vec2(16.0, 16.0)));
        let galley = paint::layout(
            painter,
            paint::job(label, Type::sans(14.0, 20.0), p.ink_muted, f32::INFINITY),
        );
        let pos = pos2(
            row.left() + 28.0 + 12.0,
            row.center().y - galley.size().y / 2.0,
        );
        paint::shimmer(painter, pos, &galley, p.ink_faint, self.time);
    }
}

fn step_icon(step: &Step) -> Icon {
    let summary = step.summary.to_lowercase();
    if summary.starts_with("read") {
        Icon::FileText
    } else if summary.starts_with("search") || summary.starts_with("found") {
        Icon::MagnifyingGlass
    } else if summary.contains("computer") || summary.contains("folder") {
        Icon::Desktop
    } else {
        Icon::Plug
    }
}
