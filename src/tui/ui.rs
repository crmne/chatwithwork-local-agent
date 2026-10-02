//! Drawing the app. [`render`] is a pure function of the state, the theme
//! and the time it is given, so snapshot tests can pin every screen.
//!
//! The layout follows the desktop app's chat page: on the left the name,
//! New chat, the search and your chats by day, with your name at the foot,
//! which opens the settings; on the right the open chat and the composer.
//! The settings (shared folders, activity, pairing, the daemon) are pages of
//! their own, drawn by [`super::pages`]. One status word shows at the top
//! right only while something needs attention.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use time::OffsetDateTime;

use super::app::{
    Access, App, ChatPane, Choice, Daemon, Focus, Following, FormRow, Hit, Hits, Modal, ModelList,
    Pairing, PickItem, PickerKind, View, field_has_choices, parse_ts,
};
use super::chat::{Attachment, ChatSummary, Entry, Question};
use super::commands::COMMANDS;
use super::markdown::{self, Footnotes};
use super::pages;
use super::theme::{Signal, Theme};
use crate::audit::{AuditEntry, Decision};

pub(super) const DOT: &str = "●";
pub(super) const RING: &str = "○";
const PROMPT: &str = "❯";
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The brand mark beside the name, in the rainbow as the GUI's mark is.
const MARK: &str = "◆";

/// The widest a column of text gets, as the web's reading column.
pub(super) const COLUMN: u16 = 76;

pub fn render(frame: &mut Frame, app: &App, theme: &Theme, now: OffsetDateTime) {
    render_hits(frame, app, theme, now);
}

/// [`render`], saying where it drew what the mouse can click.
pub fn render_hits(frame: &mut Frame, app: &App, theme: &Theme, now: OffsetDateTime) -> Hits {
    let mut hits = Hits::default();
    let area = frame.area();
    frame.render_widget(Block::new().style(theme.base()), area);
    let side_width = match area.width {
        90.. => 32,
        64.. => 26,
        _ => 0,
    };
    let [side, main] =
        Layout::horizontal([Constraint::Length(side_width), Constraint::Min(0)]).areas(area);
    if side.width > 0 {
        sidebar(frame, side, app, theme, now, &mut hits);
    }
    match app.view {
        View::Chat => main_pane(frame, main, app, theme, now, &mut hits),
        View::Settings(page) => {
            let bare = side.width == 0;
            pages::render(frame, main, app, page, bare, theme, now, &mut hits);
        }
    }
    if let Some(modal) = &app.modal {
        modal_box(frame, area, modal, app, theme);
    }
    hits
}

// ---------------------------------------------------------------- sidebar

fn sidebar(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    let block = Block::new()
        .borders(Borders::RIGHT)
        .border_style(theme.line())
        .style(theme.sunken());
    let inner = pad(block.inner(area), 1, 0);
    frame.render_widget(block, area);
    let [brand, body, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(inner);
    let name = Line::from(vec![
        Span::styled(MARK, theme.rainbow(0.48)),
        Span::raw(" "),
        Span::styled("Chat with Work", theme.strong()),
    ]);
    frame.render_widget(Paragraph::new(name), brand);
    match app.view {
        View::Chat => chats_section(frame, body, app, theme, now, hits),
        View::Settings(page) => pages::nav(frame, body, page, theme, hits),
    }
    account_line(frame, row(foot, 1), app, theme, hits);
}

/// Your name at the foot, as the desktop app has it: it opens the settings.
fn account_line(frame: &mut Frame, area: Rect, app: &App, theme: &Theme, hits: &mut Hits) {
    let name = app.chat.list.user.name.trim();
    let line = if name.is_empty() {
        Line::from(vec![
            Span::raw(" "),
            Span::styled("Settings", theme.muted()),
        ])
    } else {
        let initial: String = name
            .chars()
            .next()
            .into_iter()
            .flat_map(char::to_uppercase)
            .collect();
        Line::from(vec![
            Span::styled(format!(" {initial} "), theme.bubble().patch(theme.muted())),
            Span::raw(" "),
            Span::styled(
                ellipsize(name, area.width.saturating_sub(5) as usize),
                theme.ink(),
            ),
        ])
    };
    let line = if matches!(app.view, View::Settings(_)) {
        line.patch_style(theme.selected())
    } else {
        line
    };
    hits.add(area, Hit::Settings);
    frame.render_widget(Paragraph::new(line), area);
}

/// "New chat", the search, then the chats by day, as the web's history
/// groups them.
fn chats_section(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    if area.height == 0 {
        return;
    }
    let pane = &app.chat;
    let focused = app.focus == Focus::Chats && app.modal.is_none();
    let width = area.width as usize;
    if !pane.ready() {
        let text = match &pane.access {
            Access::Loading => "Loading…".to_string(),
            Access::NeedsApproval {
                requested: false, ..
            } => "Chats need your OK · o asks".into(),
            Access::NeedsApproval { .. } => "Waiting for your OK".into(),
            Access::Unavailable(_) => "Chats aren't available here".into(),
            Access::Ready | Access::Unknown => match &app.daemon {
                Daemon::NotRunning(_) => "Start the daemon to chat".into(),
                Daemon::Running(s) if !s.paired || s.connection.connection == "revoked" => {
                    "Pair this computer to chat".into()
                }
                _ => "Looking for the daemon…".into(),
            },
        };
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!(" {}", ellipsize(&text, width - 1)),
                theme.faint(),
            )),
            area,
        );
        return;
    }

    let marker = |selected: bool| {
        if selected && focused {
            Span::styled("▌", theme.ink())
        } else {
            Span::raw(" ")
        }
    };
    // New chat and the search stay put; the chats scroll under them.
    let new_selected = pane.selected == 0;
    let new_style = if pane.open.is_none() {
        theme.strong()
    } else {
        theme.muted()
    };
    let mut new_chat = Line::from(vec![
        marker(new_selected),
        Span::styled("+ ", theme.rainbow(0.2)),
        Span::styled("New chat", new_style),
    ]);
    if new_selected && focused {
        new_chat = new_chat.patch_style(theme.selected());
    }
    frame.render_widget(Paragraph::new(new_chat), row(area, 0));
    hits.add(row(area, 0), Hit::NewChat);

    let search = match &pane.search {
        Some(query) => {
            let mut spans = vec![
                Span::raw(" "),
                Span::styled("/ ", theme.signal_ink(Signal::Live)),
                Span::styled(tail(query, width.saturating_sub(4)), theme.ink()),
            ];
            if pane.visible().is_empty() {
                spans.push(Span::styled("  no match", theme.faint()));
            }
            Line::from(spans)
        }
        None => Line::from(vec![
            Span::raw(" "),
            Span::styled("/ ", theme.faint()),
            Span::styled("Search chats", theme.faint()),
        ]),
    };
    frame.render_widget(Paragraph::new(search), row(area, 1));
    hits.add(row(area, 1), Hit::Search);

    let mut body = below(area, 3);
    // The projects, under the chats, as the web pins them under Recent.
    let projects = &pane.list.projects;
    if pane.search.is_none() && !projects.is_empty() && body.height >= 8 {
        let height = (projects.len() as u16).min(body.height / 3).max(1) + 2;
        let at = Rect {
            y: body.bottom() - height,
            height,
            ..body
        };
        body.height -= height;
        projects_section(frame, at, app, focused, theme, hits);
    }
    // Rows, what clicking each does, and which of them is the selection.
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut targets: Vec<Option<Hit>> = Vec::new();
    let mut selected_row = 0;
    if pane.search.is_none() && pane.visible().is_empty() {
        let text = match pane.filter_project() {
            Some(project) => format!(" No chats in {} yet", project.name),
            None => " No chats yet".into(),
        };
        rows.push(Line::styled(ellipsize(&text, width), theme.faint()));
        targets.push(None);
    }
    let mut group = "";
    for (i, chat) in pane.visible().iter().enumerate() {
        let this = day_group(&chat.updated_at, now, app);
        if this != group {
            if !group.is_empty() {
                rows.push(Line::raw(""));
                targets.push(None);
            }
            group = this;
            rows.push(Line::styled(format!(" {this}"), theme.micro()));
            targets.push(None);
        }
        let selected = pane.selected == i + 1;
        if selected {
            selected_row = rows.len();
        }
        rows.push(chat_row(
            chat,
            pane,
            selected && focused,
            marker(selected),
            width,
            theme,
            now,
        ));
        targets.push(Some(Hit::Chat(chat.number)));
    }

    let height = body.height as usize;
    let start = (selected_row + 1).saturating_sub(height);
    hits.add(body, Hit::Chats);
    for (i, target) in targets.into_iter().skip(start).take(height).enumerate() {
        if let Some(target) = target {
            hits.add(row(body, i as u16), target);
        }
    }
    let shown: Vec<Line> = rows.into_iter().skip(start).take(height).collect();
    frame.render_widget(Paragraph::new(shown), body);
}

/// The projects: picking one shows its chats and starts a new chat in it;
/// picking it again shows every chat.
fn projects_section(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    focused: bool,
    theme: &Theme,
    hits: &mut Hits,
) {
    let pane = &app.chat;
    frame.render_widget(
        Paragraph::new(Line::styled(" PROJECTS", theme.micro())),
        row(area, 1),
    );
    let rows = below(area, 2);
    let first = pane.side_items().len() - pane.list.projects.len();
    let height = rows.height as usize;
    let selected = pane.selected.checked_sub(first);
    let start = selected.map_or(0, |s| (s + 1).saturating_sub(height));
    let width = area.width as usize;
    for (slot, (i, project)) in pane
        .list
        .projects
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .enumerate()
    {
        let here = selected == Some(i) && focused;
        let active = pane.filter == Some(project.id);
        let marker = if here {
            Span::styled("▌", theme.ink())
        } else {
            Span::raw(" ")
        };
        let style = if active {
            theme.strong()
        } else {
            theme.muted()
        };
        let mut spans = vec![
            marker,
            Span::raw(" "),
            Span::styled(ellipsize(&project.name, width.saturating_sub(5)), style),
        ];
        if active {
            spans = spread(spans, vec![Span::styled("× ", theme.faint())], area.width).spans;
        }
        let mut line = Line::from(spans);
        if here {
            line = line.patch_style(theme.selected());
        }
        let at = row(rows, slot as u16);
        frame.render_widget(Paragraph::new(line), at);
        hits.add(at, Hit::Project(project.id));
    }
}

/// A chat in the list. One that's being answered gets the live dot, and
/// its title shimmers while the TUI can tell when that stops: it's the open
/// chat, or the server keeps the list current.
fn chat_row(
    chat: &ChatSummary,
    pane: &ChatPane,
    highlighted: bool,
    marker: Span<'static>,
    width: usize,
    theme: &Theme,
    now: OffsetDateTime,
) -> Line<'static> {
    let open = pane.open == Some(chat.number);
    let style = if open { theme.strong() } else { theme.muted() };
    let dot = if chat.processing() || (open && pane.working()) {
        Some(theme.signal(Signal::Live))
    } else if chat.state == "error" {
        Some(theme.signal(Signal::Attention))
    } else {
        None
    };
    let room = width.saturating_sub(if dot.is_some() { 4 } else { 2 });
    let title = ellipsize(&chat.title, room);
    let mut spans = vec![marker, Span::raw(" ")];
    if let Some(dot) = dot {
        spans.push(Span::styled(DOT, dot));
        spans.push(Span::raw(" "));
    }
    if (open && pane.working()) || (pane.list_live && chat.processing()) {
        spans.extend(theme.shimmer_text(&title, sweep(now)));
    } else {
        spans.push(Span::styled(title, style));
    }
    let line = Line::from(spans);
    if highlighted {
        line.patch_style(theme.selected())
    } else {
        line
    }
}

/// "TODAY", "YESTERDAY" or "EARLIER", in local time.
fn day_group(updated_at: &str, now: OffsetDateTime, app: &App) -> &'static str {
    let Some(at) = parse_ts(updated_at) else {
        return "EARLIER";
    };
    let (day, today) = (local(at, app).date(), local(now, app).date());
    if day >= today {
        "TODAY"
    } else if today.previous_day() == Some(day) {
        "YESTERDAY"
    } else {
        "EARLIER"
    }
}

// --------------------------------------------------------- status and main

/// The one status word, top right, while something needs attention:
/// nothing at all while Chat with Work can reach this computer and the open
/// chat is followed live.
pub(super) fn attention(
    app: &App,
    now: OffsetDateTime,
) -> Option<(&'static str, Signal, &'static str)> {
    match &app.daemon {
        Daemon::Unknown => None,
        Daemon::NotRunning(_) => Some((RING, Signal::Negative, "daemon stopped")),
        Daemon::Running(status) if status.paused => Some(("‖", Signal::Attention, "paused")),
        Daemon::Running(status) => match status.connection.connection.as_str() {
            "connected" => match app.chat.following {
                _ if app.view != View::Chat => None,
                Following::Offline => Some((DOT, Signal::Attention, "reconnecting")),
                Following::Refused | Following::Unsupported => {
                    Some((RING, Signal::Attention, "not live"))
                }
                _ => None,
            },
            "connecting" => Some((spinner(now), Signal::Attention, "connecting")),
            "not_paired" => Some((RING, Signal::Attention, "not paired")),
            "revoked" => Some((DOT, Signal::Negative, "revoked")),
            _ => Some((DOT, Signal::Negative, "offline")),
        },
    }
}

/// A heading on the left and the status word, if any, on the right of
/// `area`; the status word opens the settings.
pub(super) fn heading(
    frame: &mut Frame,
    area: Rect,
    left: Vec<Span<'static>>,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    let right = match attention(app, now) {
        Some((glyph, signal, text)) => vec![
            Span::styled(glyph, theme.signal(signal)),
            Span::styled(format!(" {text}"), theme.signal_ink(signal)),
        ],
        None => Vec::new(),
    };
    let right_width: u16 = right.iter().map(|s| s.width() as u16).sum();
    if right_width > 0 {
        hits.add(
            Rect {
                x: area.right().saturating_sub(right_width),
                width: right_width.min(area.width),
                ..row(area, 0)
            },
            Hit::Settings,
        );
    }
    frame.render_widget(
        Paragraph::new(spread(left, right, area.width)),
        row(area, 0),
    );
}

fn main_pane(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    // The composer grows with what's typed and the list under it, up to
    // half the pane.
    let view = composer_view(app, theme, area.width.saturating_sub(6) as usize, now);
    let most = (area.height / 2).max(3);
    let composer_height = (view.lines.len() as u16 + 2).clamp(3, most);
    let notice = notice_lines(app, area.width.saturating_sub(4) as usize, theme);
    let [header, body, notice_area, composer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(notice.len() as u16),
        Constraint::Length(composer_height),
    ])
    .areas(area);
    let header = pad(header, 2, 0);
    heading(
        frame,
        header,
        title_spans(app, header.width, theme),
        app,
        theme,
        now,
        hits,
    );
    chat_view(frame, pad(body, 2, 0), app, theme, now, hits);
    frame.render_widget(Paragraph::new(notice), pad(notice_area, 2, 0));
    composer_box(frame, pad(composer, 1, 0), app, theme, now, view, hits);
}

/// The open chat's title, and its project.
fn title_spans(app: &App, width: u16, theme: &Theme) -> Vec<Span<'static>> {
    let pane = &app.chat;
    if !pane.ready() {
        return Vec::new();
    }
    let (title, project) = match pane.open_summary() {
        Some(chat) => (
            chat.title.clone(),
            chat.project.as_ref().map(|p| p.name.clone()),
        ),
        None if pane.open.is_some() => ("Loading…".into(), None),
        None => (
            "New chat".into(),
            pane.project.as_ref().map(|p| p.name.clone()),
        ),
    };
    // Room for the status word on the right.
    let room = (width as usize).saturating_sub(18);
    let mut spans = vec![Span::styled(ellipsize(&title, room), theme.strong())];
    if let Some(project) = project {
        let left = room.saturating_sub(title.chars().count() + 3);
        if left >= 4 {
            spans.push(Span::styled(
                format!(" · {}", ellipsize(&project, left)),
                theme.faint(),
            ));
        }
    }
    spans
}

/// What just happened, or went wrong, above the composer until the next
/// key.
pub(super) fn notice_lines(app: &App, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    match &app.notice {
        Some(notice) => {
            let mut lines = notice_line(&notice.text, notice.signal, width, theme);
            lines.truncate(2);
            if notice.signal == Signal::Idle
                && let Some(first) = lines.first_mut()
                && let Some(glyph) = first.spans.first_mut()
            {
                *glyph = Span::styled(RING, theme.faint());
            }
            lines
        }
        None => Vec::new(),
    }
}
/// Something that needs saying, with a signal edge.
pub(super) struct Card {
    pub signal: Signal,
    pub title: String,
    pub lines: Vec<CardLine>,
}

pub(super) enum CardLine {
    Text(String),
    Command(String),
    Path(String),
    /// Something to read off the screen: a code, a link.
    Value(String),
    Keys(Vec<(&'static str, &'static str)>),
}

/// A pairing under way: the code to check, and where to approve it.
pub(super) fn pairing_card(app: &App) -> Option<Card> {
    match &app.pairing {
        Some(Pairing::Starting) => Some(Card {
            signal: Signal::Attention,
            title: "Pairing this computer".into(),
            lines: vec![CardLine::Text("Asking Chat with Work for a code…".into())],
        }),
        Some(Pairing::Waiting {
            code,
            url,
            name,
            fingerprint,
            opened,
        }) => Some(Card {
            signal: Signal::Attention,
            title: format!("Approve \"{name}\" on Chat with Work"),
            lines: vec![
                CardLine::Text(if *opened {
                    "The page is open in your browser. If it isn't, open:".into()
                } else {
                    "Open this page, sign in, and approve:".into()
                }),
                CardLine::Value(url.clone()),
                CardLine::Text("Check that it shows the same code:".into()),
                CardLine::Value(code.clone()),
                CardLine::Text(format!("Key fingerprint {fingerprint}")),
                CardLine::Keys(vec![("esc", "cancel")]),
            ],
        }),
        None => None,
    }
}

/// The cards the chat view leads with: the daemon's state, then the
/// first-run offer.
fn cards(app: &App) -> Vec<Card> {
    let mut cards: Vec<Card> = pairing_card(app).into_iter().collect();
    match &app.daemon {
        Daemon::Unknown => {}
        Daemon::NotRunning(offline) => {
            let mut lines = match &offline.error {
                Some(error) => vec![
                    CardLine::Text(error.clone()),
                    CardLine::Text("If the daemon isn't running, start it with:".into()),
                ],
                None => vec![CardLine::Text(
                    "Chat with Work can't search your folders until it runs. Start it in the \
                     background with:"
                        .into(),
                )],
            };
            lines.push(CardLine::Command("cww daemon install".into()));
            if !offline.paired && app.pairing.is_none() {
                lines.push(CardLine::Text(
                    "This computer isn't paired yet either.".into(),
                ));
            }
            lines.push(CardLine::Text(
                "Until then, this shows config.toml, and changes here are saved there.".into(),
            ));
            let mut keys = vec![("s", "start it")];
            if !offline.paired && app.pairing.is_none() {
                keys.push(("c", "pair"));
            }
            keys.push(("r", "look again"));
            lines.push(CardLine::Keys(keys));
            let title = if offline.error.is_some() {
                "Can't reach the daemon"
            } else {
                "The daemon isn't running"
            };
            cards.push(Card {
                signal: Signal::Negative,
                title: title.into(),
                lines,
            });
        }
        Daemon::Running(status) => match status.connection.connection.as_str() {
            "not_paired" if app.pairing.is_none() => cards.push(Card {
                signal: Signal::Attention,
                title: "This computer isn't paired".into(),
                lines: vec![
                    CardLine::Text(
                        "Chat with Work can't search your folders until you pair it. Press \
                             c to pair it here, or run cww login in a terminal."
                            .into(),
                    ),
                    CardLine::Keys(vec![("c", "pair this computer")]),
                ],
            }),
            "revoked" if app.pairing.is_none() => cards.push(Card {
                signal: Signal::Negative,
                title: "Chat with Work revoked this computer".into(),
                lines: vec![
                    CardLine::Text("It won't reconnect until you pair it again.".into()),
                    CardLine::Keys(vec![("c", "pair again")]),
                ],
            }),
            "offline" => cards.push(Card {
                signal: Signal::Attention,
                title: "Can't reach Chat with Work".into(),
                lines: vec![CardLine::Text(format!(
                    "{} The daemon keeps trying on its own.",
                    status
                        .connection
                        .last_error
                        .as_deref()
                        .map(|e| format!("Last error: {e}."))
                        .unwrap_or_else(|| "The connection dropped.".into())
                ))],
            }),
            _ => {}
        },
    }
    if let Some(card) = chat_card(app) {
        cards.push(card);
    }
    if let Some(suggestion) = app.offer() {
        cards.push(Card {
            signal: Signal::Live,
            title: format!("Share your {} folder?", suggestion.label),
            lines: vec![
                CardLine::Text(format!(
                    "Nothing is shared yet. Chat with Work can search your {} folder:",
                    suggestion.label
                )),
                CardLine::Path(suggestion.path.clone()),
                CardLine::Text(
                    "Keys, .env files and password databases inside it stay private either way."
                        .into(),
                ),
                CardLine::Keys(vec![
                    ("y", "share it"),
                    ("n", "not now"),
                    ("a", "pick another folder"),
                ]),
            ],
        });
    } else if !matches!(app.daemon, Daemon::Unknown) && app.daemon.roots().is_empty() {
        cards.push(Card {
            signal: Signal::Idle,
            title: "Nothing is shared yet".into(),
            lines: vec![
                CardLine::Text("Chat with Work can only search folders you share.".into()),
                CardLine::Keys(vec![("a", "share a folder")]),
            ],
        });
    }
    cards
}

/// What stands between this computer and the chats, when something does.
fn chat_card(app: &App) -> Option<Card> {
    match &app.chat.access {
        Access::NeedsApproval {
            requested: false, ..
        } => Some(Card {
            signal: Signal::Attention,
            title: "Chats need your OK".into(),
            lines: vec![
                CardLine::Text(
                    "This computer shares folders with Chat with Work. To read and send your \
                     chats here too, allow it in Chat with Work. It's a separate permission, \
                     and you can turn it off without unsharing anything."
                        .into(),
                ),
                CardLine::Keys(vec![("o", "ask, and open the page to allow it")]),
            ],
        }),
        Access::NeedsApproval { url, .. } => {
            let mut lines = vec![CardLine::Text(
                "Allow chats for this computer in Chat with Work, under Settings, Computers:"
                    .into(),
            )];
            if let Some(url) = url {
                lines.push(CardLine::Value(url.clone()));
            }
            lines.push(CardLine::Text("This updates by itself once you do.".into()));
            lines.push(CardLine::Keys(vec![
                ("o", "open the page"),
                ("r", "check now"),
            ]));
            Some(Card {
                signal: Signal::Attention,
                title: "Waiting for your OK".into(),
                lines,
            })
        }
        Access::Unavailable(failure) => {
            let (signal, title) = match failure.code.as_str() {
                "unsupported" => (Signal::Idle, "This server doesn't offer chats here"),
                "daemon_outdated" => (Signal::Attention, "The daemon needs restarting"),
                "revoked" => (Signal::Negative, "Chat with Work revoked this computer"),
                "unreachable" => (Signal::Attention, "Can't reach Chat with Work"),
                "forbidden" => (Signal::Attention, "Chats aren't available to you here"),
                _ => (Signal::Negative, "Chats aren't available"),
            };
            Some(Card {
                signal,
                title: title.into(),
                lines: vec![
                    CardLine::Text(failure.message.clone()),
                    CardLine::Keys(vec![("r", "try again")]),
                ],
            })
        }
        _ => None,
    }
}

pub(super) fn card_lines(card: &Card, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let (glyph, style) = match card.signal {
        Signal::Idle => (RING, theme.faint()),
        signal => (DOT, theme.signal(signal)),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(glyph, style),
        Span::raw(" "),
        Span::styled(card.title.clone(), theme.strong()),
    ])];
    for line in &card.lines {
        match line {
            CardLine::Text(text) => lines.extend(
                wrap(text, width)
                    .into_iter()
                    .map(|l| Line::styled(l, theme.muted())),
            ),
            CardLine::Command(command) => lines.push(command_line(command, theme)),
            CardLine::Path(path) => lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(ellipsize_start(path, width.saturating_sub(2)), theme.ink()),
            ])),
            // Links and codes wrap rather than lose their end.
            CardLine::Value(value) => lines.extend(
                wrap(value, width.saturating_sub(2))
                    .into_iter()
                    .map(|part| {
                        Line::from(vec![Span::raw("  "), Span::styled(part, theme.strong())])
                    }),
            ),
            CardLine::Keys(keys) => lines.push(key_hints(keys, theme)),
        }
    }
    lines
}

pub(super) fn render_card(
    frame: &mut Frame,
    area: Rect,
    card: &Card,
    lines: Vec<Line<'static>>,
    theme: &Theme,
) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.line())
        .style(theme.surface());
    let inner = pad(block.inner(area), 1, 0);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
    if card.signal != Signal::Idle {
        // The alert edge.
        let style = theme.signal(card.signal);
        for y in area.y + 1..area.bottom().saturating_sub(1) {
            if let Some(cell) = frame.buffer_mut().cell_mut(Position::new(area.x, y)) {
                cell.set_style(style);
            }
        }
    }
}

fn chat_view(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    let mut y = area.y;
    let width = area.width.min(COLUMN);
    // Inside a conversation, only a pairing under way stays on top of it;
    // the status word says the rest.
    let in_chat =
        app.chat.ready() && (app.chat.open.is_some() || app.chat.pending_question.is_some());
    let cards = if in_chat {
        pairing_card(app).into_iter().collect()
    } else {
        cards(app)
    };
    for card in cards {
        let lines = card_lines(&card, width.saturating_sub(4) as usize, theme);
        let height = lines.len() as u16 + 2;
        if y + height > area.bottom() {
            break;
        }
        render_card(
            frame,
            Rect::new(area.x, y, width, height),
            &card,
            lines,
            theme,
        );
        y += height + 1;
    }
    let rest = Rect::new(area.x, y, area.width, area.bottom().saturating_sub(y));
    if app.chat.ready() {
        conversation(frame, rest, app, theme, now, hits);
    }
}

/// The open chat's transcript, newest at the bottom.
fn conversation(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    if area.height == 0 {
        return;
    }
    let pane = &app.chat;
    let body = area;
    let (mut lines, marks) = transcript(app, body.width as usize, theme, now);
    let height = body.height as usize;
    let most = lines.len().saturating_sub(height);
    let scroll = pane.scroll.min(most);
    let end = lines.len() - scroll;
    let start = end.saturating_sub(height);
    lines.truncate(end);
    let shown = lines.split_off(start);
    hits.add(body, Hit::Transcript);
    for (i, line) in shown.iter().enumerate() {
        if let Some(url) = link_in(line) {
            hits.add(row(body, i as u16), Hit::Link(url));
        }
    }
    for (at, hit) in marks {
        if (start..end).contains(&at) {
            hits.add(row(body, (at - start) as u16), hit);
        }
    }
    frame.render_widget(Paragraph::new(shown), body);
    if scroll > 0 && body.height > 0 {
        let note = Line::styled(format!("↓ {scroll} more · end"), theme.faint())
            .alignment(Alignment::Right);
        frame.render_widget(Paragraph::new(note), row(body, body.height - 1));
    }
}

pub(super) fn command_line(command: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("  $ ", theme.faint()),
        Span::styled(command.to_string(), theme.strong()),
    ])
}

/// A whole web link a line shows, to open with a click.
fn link_in(line: &Line) -> Option<String> {
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let start = text.find("https://").or_else(|| text.find("http://"))?;
    let url: String = text[start..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    // Cut short to fit: not the real address.
    (!url.ends_with('…')).then_some(url)
}

/// The conversation as lines, oldest first, with the lines a click acts on.
fn transcript(
    app: &App,
    width: usize,
    theme: &Theme,
    now: OffsetDateTime,
) -> (Vec<Line<'static>>, Vec<(usize, Hit)>) {
    let mut marks: Vec<(usize, Hit)> = Vec::new();
    let pane = &app.chat;
    let width = width.max(16);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if pane.open.is_none() && pane.pending_question.is_none() {
        lines.push(Line::styled("Ask anything.", theme.strong()));
        lines.extend(
            wrap(
                "Chat with Work searches your shared folders and the services you connected, \
                 and the chat stays in Chat with Work on the web too.",
                width.min(72),
            )
            .into_iter()
            .map(|l| Line::styled(l, theme.muted())),
        );
        if let Some(reason) = pane.locked_reason() {
            lines.push(Line::raw(""));
            lines.push(notice_line(reason, Signal::Attention, width, theme).remove(0));
        }
        return (lines, marks);
    }
    if pane.transcript.is_none() && pane.loading {
        lines.push(Line::from(vec![
            Span::styled(spinner(now), theme.signal(Signal::Live)),
            Span::styled(" Reading the chat…", theme.faint()),
        ]));
    }
    let mut answered: Vec<u64> = Vec::new();
    let mut live_activity = false;
    let entries = pane.transcript.as_ref().map_or(&[][..], |t| &t.entries[..]);
    for entry in entries {
        match entry {
            Entry::User {
                content,
                author,
                attachments,
                ..
            } => {
                bubble(&mut lines, content, author.as_deref(), width, theme, false);
                attachment_lines(&mut lines, attachments, width, theme);
            }
            Entry::Activity {
                title,
                details,
                progress,
                pending,
                steps,
                ..
            } => {
                live_activity |= *pending;
                let progress = if *pending {
                    pane.progress.as_ref().or(progress.as_ref())
                } else {
                    None
                };
                activity_lines(
                    &mut lines,
                    ActivityView {
                        title,
                        details: details.as_deref(),
                        progress: progress.map(String::as_str),
                        pending: *pending,
                        steps,
                        expanded: pane.show_steps || *pending,
                    },
                    width,
                    theme,
                    now,
                );
            }
            Entry::Assistant {
                id,
                content,
                sources,
            } => {
                answered.push(*id);
                let text = if content.trim().is_empty() {
                    pane.streamed.get(id).map_or("", String::as_str)
                } else {
                    content.as_str()
                };
                if !text.trim().is_empty() {
                    answer_lines(&mut lines, text, sources, width, theme);
                }
            }
            Entry::Notice { tone, text } => {
                let signal = if tone == "negative" {
                    Signal::Negative
                } else {
                    Signal::Attention
                };
                lines.extend(notice_line(text, signal, width, theme));
                lines.push(Line::raw(""));
            }
            Entry::Unknown => {}
        }
    }
    // Answers still being written that the last read didn't have yet.
    let mut writing = false;
    for (id, text) in &pane.streamed {
        if !answered.contains(id) && !text.trim().is_empty() {
            answer_lines(&mut lines, text, &[], width, theme);
            writing = true;
        }
    }
    if let Some(question) = &pane.pending_question {
        bubble(&mut lines, question, None, width, theme, true);
    }
    if pane.working() && !live_activity {
        // As the web says it: "Writing" once the answer streams in.
        let word = if pane.stopping {
            "Stopping…"
        } else if writing || pane.streamed.values().any(|t| !t.trim().is_empty()) {
            "Writing"
        } else {
            "Thinking"
        };
        let mut spans = vec![working_mark(theme, now), Span::raw(" ")];
        spans.extend(theme.shimmer_text(word, sweep(now)));
        if let Some(progress) = &pane.progress {
            spans.push(Span::styled(format!(" · {progress}"), theme.faint()));
        }
        lines.push(Line::from(spans));
    }
    waiting_lines(&mut lines, &mut marks, app, width, theme);
    if let Some(error) = &pane.error {
        lines.extend(notice_line(error, Signal::Negative, width, theme));
    }
    while lines.last().is_some_and(|l| l.width() == 0) {
        lines.pop();
    }
    (lines, marks)
}

/// The files sent with a question, under its bubble.
fn attachment_lines(
    lines: &mut Vec<Line<'static>>,
    attachments: &[Attachment],
    width: usize,
    theme: &Theme,
) {
    if attachments.is_empty() {
        return;
    }
    // Under the bubble's blank line.
    let blank = lines.pop();
    for file in attachments {
        let text = format!("+ {} · {}", file.filename, size(file.byte_size));
        lines.push(
            Line::styled(ellipsize(&text, width.saturating_sub(2)), theme.faint())
                .alignment(Alignment::Right),
        );
    }
    lines.extend(blank);
}

/// What the answer waits on: a change to approve, a question from a
/// tool's server, or someone else in a project chat.
fn waiting_lines(
    lines: &mut Vec<Line<'static>>,
    marks: &mut Vec<(usize, Hit)>,
    app: &App,
    width: usize,
    theme: &Theme,
) {
    let pane = &app.chat;
    let Some(transcript) = pane
        .transcript
        .as_ref()
        .filter(|t| !t.chat.processing() && Some(t.chat.number) == pane.open)
    else {
        return;
    };
    let focused = app.focus == Focus::Composer;
    for approval in transcript.approvals.iter().filter(|a| !a.decidable) {
        let who = approval.waiting_for.as_deref().unwrap_or("someone");
        lines.push(Line::from(vec![
            Span::styled(RING, theme.faint()),
            Span::styled(
                format!(
                    " Waiting for {who} to approve a change in {}",
                    approval.service
                ),
                theme.muted(),
            ),
        ]));
        lines.push(Line::raw(""));
    }
    for question in transcript.questions.iter().filter(|q| !q.decidable) {
        let who = question.waiting_for.as_deref().unwrap_or("someone");
        lines.push(Line::from(vec![
            Span::styled(RING, theme.faint()),
            Span::styled(
                format!(" Waiting for {who} to answer {}", question.service),
                theme.muted(),
            ),
        ]));
        lines.push(Line::raw(""));
    }
    if let Some(approval) = pane.pending_decision() {
        let title = if pane.deciding {
            "Sending your answer…".to_string()
        } else {
            format!("Waiting for your approval · {}", approval.service)
        };
        lines.push(Line::from(vec![
            Span::styled(DOT, theme.signal(Signal::Attention)),
            Span::raw(" "),
            Span::styled(title, theme.strong()),
        ]));
        for part in wrap(&approval.summary, width.saturating_sub(2)) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(part, theme.ink()),
            ]));
        }
        detail_lines(
            lines,
            approval
                .details
                .iter()
                .map(|d| (d.label.as_str(), d.value.as_str())),
            width,
            theme,
        );
        let choices = ChatPane::choices(approval);
        let selected = pane.decision.min(choices.len() - 1);
        for (i, choice) in choices.iter().enumerate() {
            let text = match choice {
                Choice::Approve => "Approve",
                Choice::ApproveAll => "Approve for the rest of this chat",
                Choice::Deny => "Deny",
                Choice::DenyWithReason => "Deny, and say what to do instead",
            };
            marks.push((lines.len(), Hit::Choice(i)));
            lines.push(choice_line(
                &format!("{}. {text}", i + 1),
                focused && i == selected,
                theme,
            ));
        }
        lines.push(Line::raw(""));
    } else if let Some(question) = pane.pending_question() {
        question_lines(lines, marks, app, question, width, theme);
    }
}

/// `label  value` pairs, labels lined up.
fn detail_lines<'a>(
    lines: &mut Vec<Line<'static>>,
    details: impl Iterator<Item = (&'a str, &'a str)>,
    width: usize,
    theme: &Theme,
) {
    let details: Vec<(&str, &str)> = details.collect();
    let label_width = details
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0)
        .min(18);
    for (label, value) in details {
        let room = width.saturating_sub(label_width + 4).max(8);
        for (i, part) in wrap(value, room).into_iter().enumerate() {
            let label = if i == 0 {
                format!("  {:<label_width$}  ", ellipsize(label, label_width))
            } else {
                " ".repeat(label_width + 4)
            };
            lines.push(Line::from(vec![
                Span::styled(label, theme.faint()),
                Span::styled(part, theme.muted()),
            ]));
        }
    }
}

/// One answer to pick, marked when selected.
fn choice_line(text: &str, selected: bool, theme: &Theme) -> Line<'static> {
    if selected {
        Line::from(vec![
            Span::styled(PROMPT, theme.rainbow(0.48)),
            Span::raw(" "),
            Span::styled(text.to_string(), theme.strong()),
        ])
        .patch_style(theme.selected())
    } else {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(text.to_string(), theme.muted()),
        ])
    }
}

/// A question from a tool's server, as a form to fill in or a page to open.
fn question_lines(
    lines: &mut Vec<Line<'static>>,
    marks: &mut Vec<(usize, Hit)>,
    app: &App,
    question: &Question,
    width: usize,
    theme: &Theme,
) {
    let pane = &app.chat;
    let focused = app.focus == Focus::Composer && pane.form.editing.is_none();
    let title = if pane.deciding {
        "Sending your answer…".to_string()
    } else if question.is_url() {
        format!("{} asks you to open a page", question.service)
    } else {
        format!("{} asks", question.service)
    };
    lines.push(Line::from(vec![
        Span::styled(DOT, theme.signal(Signal::Attention)),
        Span::raw(" "),
        Span::styled(title, theme.strong()),
    ]));
    for part in wrap(&question.message, width.saturating_sub(2)) {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(part, theme.ink()),
        ]));
    }
    let rows = ChatPane::question_rows(question);
    let selected = pane.form.row.min(rows.len() - 1);
    let label_width = question
        .fields
        .iter()
        .map(|f| f.label().chars().count())
        .max()
        .unwrap_or(0)
        .min(20);
    for (i, row) in rows.iter().enumerate() {
        let here = focused && i == selected;
        let line = match row {
            FormRow::Field(f) => {
                let field = &question.fields[*f];
                let value = pane.form.values.get(*f).map_or("", String::as_str);
                let (text, style) = if pane.form.editing == Some(*f) {
                    ("typing below…".to_string(), theme.faint())
                } else if !value.is_empty() {
                    (value.to_string(), theme.ink())
                } else if field_has_choices(field) {
                    let hint = if field.required {
                        "pick with ← →"
                    } else {
                        "–"
                    };
                    (hint.to_string(), theme.faint())
                } else if field.required {
                    (
                        "needed: enter to type it".to_string(),
                        theme.signal_ink(Signal::Attention),
                    )
                } else {
                    ("–".to_string(), theme.faint())
                };
                let label = format!("{:<label_width$}  ", ellipsize(field.label(), label_width));
                let room = width.saturating_sub(label_width + 4);
                let mut spans = vec![
                    if here {
                        Span::styled(PROMPT, theme.rainbow(0.48))
                    } else {
                        Span::raw(" ")
                    },
                    Span::raw(" "),
                    Span::styled(label, theme.muted()),
                    Span::styled(ellipsize(&text, room), style),
                ];
                if here && field_has_choices(field) {
                    spans.push(Span::styled("  ← →", theme.faint()));
                }
                let line = Line::from(spans);
                if here {
                    line.patch_style(theme.selected())
                } else {
                    line
                }
            }
            FormRow::OpenPage => {
                let host = question.host.as_deref().unwrap_or("the page");
                choice_line(&format!("Open {host} in the browser"), here, theme)
            }
            FormRow::Send if question.is_url() => choice_line("Done, carry on", here, theme),
            FormRow::Send => choice_line("Send", here, theme),
            FormRow::Decline => choice_line("Decline", here, theme),
        };
        marks.push((lines.len(), Hit::Ask(i)));
        lines.push(line);
    }
    if let Some(field) = rows
        .get(selected)
        .and_then(|r| match r {
            FormRow::Field(f) => question.fields.get(*f),
            _ => None,
        })
        .filter(|_| focused)
        && let Some(description) = &field.description
    {
        for part in wrap(description, width.saturating_sub(2)) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(part, theme.faint()),
            ]));
        }
    }
    if let Some(note) = &question.note {
        for part in wrap(note, width.saturating_sub(2)) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(part, theme.faint()),
            ]));
        }
    }
    lines.push(Line::raw(""));
}

/// A question, as a bubble on the right.
fn bubble(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    author: Option<&str>,
    width: usize,
    theme: &Theme,
    pending: bool,
) {
    let most = (width * 3 / 4).clamp(16, width);
    let wrapped = wrap(text.trim(), most.saturating_sub(4));
    let inner = wrapped.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let indent = " ".repeat(width.saturating_sub(inner + 4));
    if let Some(author) = author {
        lines.push(Line::styled(author.to_string(), theme.micro()).alignment(Alignment::Right));
    }
    let style = if pending {
        theme.bubble().patch(theme.faint())
    } else {
        theme.bubble()
    };
    for line in wrapped {
        let pad = inner - line.chars().count();
        lines.push(Line::from(vec![
            Span::raw(indent.clone()),
            Span::styled(format!("  {line}{}  ", " ".repeat(pad)), style),
        ]));
    }
    lines.push(Line::raw(""));
}

struct ActivityView<'a> {
    title: &'a str,
    details: Option<&'a str>,
    progress: Option<&'a str>,
    pending: bool,
    steps: &'a [super::chat::Step],
    expanded: bool,
}

/// The tool work between two things said: one line, like the web's
/// collapsed activity, and its steps under it when expanded.
fn activity_lines(
    lines: &mut Vec<Line<'static>>,
    activity: ActivityView,
    width: usize,
    theme: &Theme,
    now: OffsetDateTime,
) {
    let mut spans = if activity.pending {
        let mut spans = vec![working_mark(theme, now), Span::raw(" ")];
        spans.extend(theme.shimmer_text(activity.title, sweep(now)));
        spans
    } else {
        let caret = if activity.expanded { "▾" } else { "▸" };
        vec![
            Span::styled(caret, theme.faint()),
            Span::raw(" "),
            Span::styled(activity.title.to_string(), theme.muted()),
        ]
    };
    if let Some(details) = activity.progress.or(activity.details) {
        let used: usize = spans.iter().map(Span::width).sum();
        let room = width.saturating_sub(used + 3);
        if room > 4 {
            spans.push(Span::styled(
                format!(" · {}", ellipsize(details, room)),
                theme.faint(),
            ));
        }
    }
    lines.push(Line::from(spans));
    if activity.expanded {
        for step in activity.steps {
            let (glyph, style) = if step.pending {
                (RING, theme.signal(Signal::Live))
            } else {
                ("·", theme.faint())
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(glyph, style),
                Span::raw(" "),
                Span::styled(
                    ellipsize(&step.summary, width.saturating_sub(4)),
                    theme.muted(),
                ),
            ]));
            for file in &step.files {
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(ellipsize(file, width.saturating_sub(6)), theme.faint()),
                ]));
            }
        }
    }
    lines.push(Line::raw(""));
}

/// An answer: Markdown, then its sources and links as footnotes.
fn answer_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    sources: &[super::chat::Source],
    width: usize,
    theme: &Theme,
) {
    let mut notes = Footnotes::new(sources.iter().map(|s| (s.title.clone(), s.url.clone())));
    lines.extend(markdown::render(text, width, theme, &mut notes));
    if !notes.notes.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("SOURCES", theme.micro()));
        lines.extend(markdown::footnote_lines(&notes, width, theme));
    }
    lines.push(Line::raw(""));
}

pub(super) fn notice_line(
    text: &str,
    signal: Signal,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    wrap(text, width.saturating_sub(2))
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let glyph = if i == 0 { DOT } else { " " };
            Line::from(vec![
                Span::styled(glyph, theme.signal(signal)),
                Span::raw(" "),
                Span::styled(l, theme.signal_ink(signal)),
            ])
        })
        .collect()
}

/// What the composer box holds: the prompt line or what's typed, the
/// files to send, and the slash list or a picker under them.
pub struct ComposerView {
    lines: Vec<Line<'static>>,
    /// Where the cursor goes, as (column, line), when it shows.
    cursor: Option<(u16, u16)>,
    /// The lines a click picks.
    rows: Vec<(u16, Hit)>,
}

/// Rows of a list shown at once under the composer.
const LIST_ROWS: usize = 8;

/// Lines of what's typed shown at once.
const INPUT_ROWS: usize = 8;

/// The composer's content for a box `width` columns wide inside.
fn composer_view(app: &App, theme: &Theme, width: usize, now: OffsetDateTime) -> ComposerView {
    let _ = now;
    let pane = &app.chat;
    let locked = pane.locked_reason();
    let usable = pane.ready() && locked.is_none();
    let focused = usable && app.focus == Focus::Composer && app.modal.is_none();
    let room = width.saturating_sub(3).max(4);
    let prompt = || Span::styled(PROMPT, theme.rainbow(0.48));
    let mut view = ComposerView {
        lines: Vec::new(),
        cursor: None,
        rows: Vec::new(),
    };

    if let Some(picker) = pane.picker.as_ref().filter(|_| usable) {
        let label = match picker.kind {
            PickerKind::Chats => "Resume a chat: ",
            PickerKind::Models => "Model: ",
            PickerKind::Projects => "Project: ",
        };
        let query = tail(&picker.query, room.saturating_sub(label.chars().count()));
        view.cursor = focused.then_some((
            (2 + label.chars().count() + query.chars().count()) as u16,
            0,
        ));
        view.lines.push(Line::from(vec![
            prompt(),
            Span::raw(" "),
            Span::styled(label, theme.faint()),
            Span::styled(query, theme.ink()),
        ]));
        picker_rows(&mut view, app, theme, width);
        return view;
    }

    let typed = !pane.input.is_empty();
    match (&pane.access, locked) {
        (Access::Ready, Some(reason)) => view.lines.push(Line::from(vec![
            prompt(),
            Span::raw(" "),
            Span::styled(ellipsize(reason, room), theme.signal_ink(Signal::Attention)),
        ])),
        (Access::Ready, None) if typed => {
            let (rows, (row, col)) = wrap_input(pane.input.text(), pane.input.cursor(), room);
            // Keep the cursor's line in view.
            let first = (row + 1).saturating_sub(INPUT_ROWS);
            for (i, text) in rows.iter().enumerate().skip(first).take(INPUT_ROWS) {
                let lead = if i == 0 { prompt() } else { Span::raw(" ") };
                view.lines.push(Line::from(vec![
                    lead,
                    Span::raw(" "),
                    Span::styled(text.clone(), theme.ink()),
                ]));
            }
            view.cursor = focused.then_some(((col + 2) as u16, (row - first) as u16));
        }
        (Access::Ready, None) => {
            let editing = pane.form.editing.and_then(|i| {
                pane.pending_question()
                    .and_then(|q| q.fields.get(i))
                    .map(|f| f.label().to_string())
            });
            let text = if let Some(label) = editing {
                format!("{label}: type it, then enter")
            } else if pane.reason {
                "Say what to do instead, then enter".into()
            } else if pane.pending_decision().is_some() {
                "Approve or deny above: ↑ ↓ and enter, or y and n".into()
            } else if pane.pending_question().is_some() {
                "Answer above: ↑ ↓ and enter".into()
            } else if pane.working() && pane.stopping {
                "Stopping…".into()
            } else if pane.working() {
                "Writing…  esc stops".into()
            } else if pane.open.is_some() {
                "Reply to Chat with Work".into()
            } else {
                "Ask anything…".into()
            };
            let mut spans = vec![
                prompt(),
                Span::raw(" "),
                Span::styled(ellipsize(&text, room), theme.faint()),
            ];
            // The one hint, as the desktop app's composer has its own.
            let hint = "/ for commands";
            let used = text.chars().count() + 2;
            if !pane.working() && focused && used + hint.len() + 4 <= width {
                spans.push(Span::raw(" ".repeat(width - used - hint.len())));
                spans.push(Span::styled(hint, theme.faint()));
            }
            view.lines.push(Line::from(spans));
            view.cursor = focused.then_some((2, 0));
        }
        (access, _) => {
            let text = match access {
                Access::Loading => "Loading your chats…",
                Access::NeedsApproval {
                    requested: false, ..
                } => "Chats need your OK first: press o",
                Access::NeedsApproval { .. } => "Waiting for your OK in Chat with Work",
                Access::Unavailable(_) => "Chats aren't available here",
                _ => match &app.daemon {
                    Daemon::NotRunning(_) => "Start the daemon to chat here: press s",
                    Daemon::Running(_) if app.can_pair() => {
                        "Pair this computer to chat here: press c"
                    }
                    _ => "Looking for the daemon…",
                },
            };
            view.lines.push(Line::from(vec![
                prompt(),
                Span::raw(" "),
                Span::styled(ellipsize(text, room), theme.faint()),
            ]));
        }
    }
    if usable {
        for file in &pane.attachments {
            let state = match file.uploaded {
                Some((_, bytes)) => size(bytes),
                None => "uploading…".into(),
            };
            view.lines.push(Line::styled(
                ellipsize(&format!("  + {} · {state}", file.name), width),
                theme.faint(),
            ));
        }
    }
    menu_rows(&mut view, app, theme, width);
    view
}

/// What's typed, cut into rows of `room` characters at line breaks and
/// where a line is too long, and the cursor's (row, column) in them.
fn wrap_input(text: &str, cursor: usize, room: usize) -> (Vec<String>, (usize, usize)) {
    let room = room.max(1);
    let mut rows = Vec::new();
    let mut at = (0, 0);
    let mut offset = 0;
    for line in text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        let first = rows.len();
        if chars.is_empty() {
            rows.push(String::new());
        }
        for chunk in chars.chunks(room) {
            rows.push(chunk.iter().collect());
        }
        if (offset..=offset + line.len()).contains(&cursor) {
            let column = line[..cursor - offset].chars().count();
            let row = (column / room).min(rows.len() - first - 1);
            at = (first + row, column - row * room);
        }
        offset += line.len() + 1;
    }
    (rows, at)
}

/// The slash commands that match, under what's typed.
fn menu_rows(view: &mut ComposerView, app: &App, theme: &Theme, width: usize) {
    let menu = app.menu();
    if menu.is_empty() {
        return;
    }
    let selected = app.chat.menu.min(menu.len() - 1);
    let first = (selected + 1).saturating_sub(LIST_ROWS);
    let name_width = 22;
    for (i, command) in menu.iter().enumerate().skip(first).take(LIST_ROWS) {
        let here = i == selected;
        let mut name = format!("/{}", command.name);
        if !command.args.is_empty() {
            name.push(' ');
            name.push_str(command.args);
        }
        let about = ellipsize(command.about, width.saturating_sub(name_width + 2));
        let mut line = Line::from(vec![
            if here {
                Span::styled("▌", theme.ink())
            } else {
                Span::raw(" ")
            },
            Span::styled(
                format!("{:<name_width$}", ellipsize(&name, name_width - 1)),
                if here { theme.strong() } else { theme.ink() },
            ),
            Span::styled(about, theme.faint()),
        ]);
        if here {
            line = line.patch_style(theme.selected());
        }
        view.rows.push((view.lines.len() as u16, Hit::Menu(i)));
        view.lines.push(line);
    }
}

/// The open picker's rows, filtered by what's typed.
fn picker_rows(view: &mut ComposerView, app: &App, theme: &Theme, width: usize) {
    let pane = &app.chat;
    let Some(picker) = &pane.picker else { return };
    let items = pane.pick_items();
    if items.is_empty() {
        let text = match (&pane.models, picker.kind) {
            (ModelList::Loading, PickerKind::Models) => " Loading models…",
            _ => " No match",
        };
        view.lines.push(Line::styled(text, theme.faint()));
        return;
    }
    let selected = picker.selected.min(items.len() - 1);
    let first = (selected + 1).saturating_sub(LIST_ROWS);
    let current = pane.current_model_id();
    for (i, item) in items.iter().enumerate().skip(first).take(LIST_ROWS) {
        let here = i == selected;
        let marker = if here {
            Span::styled("▌", theme.ink())
        } else {
            Span::raw(" ")
        };
        let name_style = if here { theme.strong() } else { theme.ink() };
        let mut spans = vec![marker];
        match item {
            PickItem::Chat(chat) => {
                let mut rest = format!(" · #{}", chat.number);
                if let Some(project) = &chat.project {
                    rest.push_str(&format!(" · {}", project.name));
                }
                let title = ellipsize(
                    &chat.title,
                    width.saturating_sub(rest.chars().count() + 2).max(8),
                );
                spans.push(Span::styled(title, name_style));
                spans.push(Span::styled(rest, theme.faint()));
            }
            PickItem::Model(model) => {
                let style = if model.selectable {
                    name_style
                } else {
                    theme.faint()
                };
                spans.push(Span::styled(model.name.clone(), style));
                if current == Some(model.id.as_str()) {
                    spans.push(Span::styled(
                        "  current",
                        theme.signal_ink(Signal::Positive),
                    ));
                }
                let detail = if model.selectable {
                    model.rate.clone().unwrap_or_else(|| model.provider.clone())
                } else {
                    model.reason.clone().unwrap_or_default()
                };
                let used: usize = spans.iter().map(Span::width).sum();
                let room = width.saturating_sub(used + 3);
                if room > 4 && !detail.is_empty() {
                    let style = if model.selectable {
                        theme.faint()
                    } else {
                        theme.signal_ink(Signal::Attention)
                    };
                    spans.push(Span::styled(
                        format!(" · {}", ellipsize(&detail, room)),
                        style,
                    ));
                }
            }
            PickItem::Project(project) => {
                let name = project.map_or("No project", |p| p.name.as_str());
                spans.push(Span::styled(
                    ellipsize(name, width.saturating_sub(2)),
                    name_style,
                ));
                if project.map(|p| p.id) == pane.project.as_ref().map(|p| p.id) {
                    spans.push(Span::styled(
                        "  current",
                        theme.signal_ink(Signal::Positive),
                    ));
                }
            }
        }
        let mut line = Line::from(spans);
        if here {
            line = line.patch_style(theme.selected());
        }
        view.rows.push((view.lines.len() as u16, Hit::Pick(i)));
        view.lines.push(line);
    }
}

/// The composer, with the Live Wire rainbow for its edge: dimmed while it
/// can't send, flowing while an answer is written. Without true colour it's
/// the nearest 256 colours, and with `NO_COLOR` a bold or dim frame.
fn composer_box(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    view: ComposerView,
    hits: &mut Hits,
) {
    let pane = &app.chat;
    let usable = pane.ready() && pane.locked_reason().is_none();
    let block = Block::bordered().border_type(BorderType::Rounded);
    let inner = pad(block.inner(area), 1, 0);
    frame.render_widget(block, area);
    let phase = if pane.working() {
        // One trip around every two seconds.
        (now.unix_timestamp_nanos() % 2_000_000_000) as f32 / 2_000_000_000.0
    } else {
        0.0
    };
    rainbow_border(frame.buffer_mut(), area, theme, usable, phase);
    // The model, on the bottom edge, where the desktop app shows it.
    if let Some(model) = pane.model_name().filter(|_| usable && area.height >= 3) {
        let label = format!(
            " {} ",
            ellipsize(model, (area.width as usize).saturating_sub(8))
        );
        let width = label.chars().count() as u16;
        if width + 4 <= area.width {
            let at = Rect::new(area.right() - 2 - width, area.bottom() - 1, width, 1);
            frame.render_widget(Paragraph::new(Line::styled(label, theme.faint())), at);
        }
    }
    hits.add(area, Hit::Composer);
    for (at, hit) in view.rows {
        if at < inner.height {
            hits.add(row(inner, at), hit);
        }
    }
    frame.render_widget(Paragraph::new(view.lines), inner);
    if let Some((x, y)) = view.cursor
        && y < inner.height
    {
        frame.set_cursor_position(Position::new(
            (inner.x + x).min(inner.right().saturating_sub(1)),
            inner.y + y,
        ));
    }
}

/// The composer's edge: the rainbow, dimmed while it can't send, shifted
/// by `phase` so it flows while an answer is written.
fn rainbow_border(buf: &mut Buffer, area: Rect, theme: &Theme, bright: bool, phase: f32) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    let span = (area.width - 1) as f32;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let edge = y == area.top()
                || y == area.bottom() - 1
                || x == area.left()
                || x == area.right() - 1;
            if !edge {
                continue;
            }
            let along = (x - area.left()) as f32 / span;
            // Flowing: the gradient runs there and back so it never jumps.
            let t = if phase > 0.0 {
                let shifted = (along + phase) % 1.0;
                1.0 - (2.0 * shifted - 1.0).abs()
            } else {
                along
            };
            let style = if bright {
                theme.rainbow(t)
            } else {
                theme.rainbow_dim(t)
            };
            if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                cell.set_style(style);
            }
        }
    }
}

// -------------------------------------------------------------- audit log

pub(super) fn log_view(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if area.height == 0 {
        return;
    }
    let mut status = format!("{} entries", app.audit.len());
    if app.log_scroll > 0 {
        status.push_str(" · end follows");
    }
    let header = spread(
        vec![Span::styled(
            format!("{:<10}{:<9}{:<12}{}", "TIME", "RESULT", "TOOL", "DETAIL"),
            theme.micro(),
        )],
        vec![Span::styled(status, theme.faint())],
        area.width,
    );
    frame.render_widget(Paragraph::new(header), row(area, 0));
    let body = below(area, 1);
    if app.audit.is_empty() {
        let text = wrap(
            "No entries yet. Every request Chat with Work makes shows up here, answered or \
             refused, as it happens.",
            body.width.min(72) as usize,
        );
        frame.render_widget(
            Paragraph::new(
                text.into_iter()
                    .map(|l| Line::styled(l, theme.faint()))
                    .collect::<Vec<_>>(),
            ),
            below(body, 1),
        );
        return;
    }
    // Newest at the bottom; an entry may take more than one line.
    let end = app.audit.len().saturating_sub(app.log_scroll);
    let mut lines: Vec<Line> = Vec::new();
    for entry in app.audit[..end].iter().rev() {
        let mut entry_lines = log_lines(entry, app, theme, body.width as usize);
        if lines.len() + entry_lines.len() > body.height as usize {
            break;
        }
        entry_lines.append(&mut lines);
        lines = entry_lines;
    }
    frame.render_widget(Paragraph::new(lines), body);
}

/// `text` padded to `width`, and always followed by at least one space, so
/// a long event name can't run into the next column.
fn column(text: &str, width: usize) -> String {
    if text.chars().count() < width {
        format!("{text:<width$}")
    } else {
        format!("{text} ")
    }
}

/// Where the DETAIL column starts.
const LOG_DETAIL: usize = 31;

/// One entry: a line, plus the reason under the detail for refusals.
fn log_lines(entry: &AuditEntry, app: &App, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let time = parse_ts(&entry.ts)
        .map(|t| clock_secs(t, app))
        .unwrap_or_else(|| "?".into());
    let mut spans = vec![Span::styled(format!("{time:<10}"), theme.faint())];
    let mut reason = None;
    if entry.event == "tool" {
        let signal = entry_signal(entry);
        let word = match entry.decision {
            Some(Decision::Allowed) => "allowed",
            Some(Decision::Denied) => "denied",
            Some(Decision::Error) => "error",
            None => "?",
        };
        spans.push(Span::styled(format!("{word:<9}"), theme.signal_ink(signal)));
        spans.push(Span::styled(
            column(entry.tool.as_deref().unwrap_or("?"), 12),
            theme.ink(),
        ));
        let target = target(entry);
        if !target.is_empty() {
            spans.push(Span::styled(target, theme.muted()));
        }
        match entry.decision {
            Some(Decision::Denied | Decision::Error) => {
                reason = entry.reason.clone().or_else(|| entry.code.clone());
            }
            _ => {
                spans.push(Span::styled(" → ", theme.faint()));
                spans.push(Span::styled(outcome(entry).0, theme.muted()));
                if let Some(ms) = entry.duration_ms {
                    spans.push(Span::styled(format!(" · {}", duration(ms)), theme.faint()));
                }
            }
        }
        if let Some(chat) = &entry.chat_id {
            spans.push(Span::styled(format!(" · chat {chat}"), theme.faint()));
        }
    } else {
        spans.push(Span::styled(format!("{:<9}", "·"), theme.faint()));
        spans.push(Span::styled(
            column(&entry.event.replace('_', " "), 12),
            theme.signal_ink(entry_signal(entry)),
        ));
        if let Some(detail) = &entry.detail {
            spans.push(Span::styled(detail.clone(), theme.muted()));
        }
    }
    let mut lines = vec![Line::from(spans)];
    if let Some(reason) = reason {
        let indent = " ".repeat(LOG_DETAIL);
        for text in wrap(&reason, width.saturating_sub(LOG_DETAIL)) {
            lines.push(Line::from(vec![
                Span::raw(indent.clone()),
                Span::styled(text, theme.signal_ink(Signal::Negative)),
            ]));
        }
    }
    lines
}

fn entry_signal(entry: &AuditEntry) -> Signal {
    if entry.event == "tool" {
        return match entry.decision {
            Some(Decision::Allowed) => Signal::Positive,
            Some(Decision::Denied | Decision::Error) => Signal::Negative,
            None => Signal::Idle,
        };
    }
    match entry.event.as_str() {
        "connected" | "resumed" => Signal::Positive,
        "revoked" => Signal::Negative,
        "disconnected" | "paused" | "stopped" | "shutdown_requested" => Signal::Attention,
        _ => Signal::Live,
    }
}

/// `0.4 ms`, `12 ms`, `1.2 s`.
fn duration(ms: f64) -> String {
    if ms < 10.0 {
        format!("{ms:.1} ms")
    } else if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{:.1} s", ms / 1000.0)
    }
}

fn target(entry: &AuditEntry) -> String {
    match (&entry.query, &entry.path) {
        (Some(query), _) => format!("\"{query}\""),
        (None, Some(path)) => path.clone(),
        (None, None) => String::new(),
    }
}

fn outcome(entry: &AuditEntry) -> (String, Signal) {
    let why = || {
        entry
            .reason
            .clone()
            .or_else(|| entry.code.clone())
            .unwrap_or_default()
    };
    match entry.decision {
        Some(Decision::Denied) => (format!("denied: {}", why()), Signal::Negative),
        Some(Decision::Error) => (format!("failed: {}", why()), Signal::Negative),
        _ => {
            let n = entry.results;
            let text = match (entry.tool.as_deref(), n, entry.bytes) {
                (Some("search"), Some(n), _) => plural(n, "hit", "hits"),
                (Some("list"), Some(n), _) => plural(n, "entry", "entries"),
                (Some("roots"), Some(n), _) => plural(n, "folder", "folders"),
                (_, _, Some(b)) => bytes(b),
                (_, Some(n), None) => plural(n, "result", "results"),
                _ => "done".into(),
            };
            (text, Signal::Idle)
        }
    }
}

// ----------------------------------------------------------------- modals

/// The keys an open dialog takes.
fn modal_hints(modal: &Modal) -> Vec<(&'static str, &'static str)> {
    match modal {
        Modal::AddRoot { .. } => vec![("enter", "share"), ("esc", "cancel")],
        Modal::RenameRoot { .. } => vec![("enter", "rename"), ("esc", "cancel")],
        Modal::ConfirmRemove { .. } => vec![("y", "stop sharing"), ("n", "keep it")],
        Modal::ConfirmBroad { .. } => vec![("y", "share anyway"), ("n", "cancel")],
        Modal::ConfirmDelete { .. } => vec![("y", "delete it"), ("n", "keep it")],
        Modal::ConfirmLogout => vec![("y", "disconnect"), ("n", "keep it")],
        Modal::Help { .. } => vec![("↑↓", "scroll"), ("any other key", "close")],
    }
}

pub(super) fn key_hints(keys: &[(&'static str, &'static str)], theme: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, what)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(*key, theme.strong()));
        spans.push(Span::styled(format!(" {what}"), theme.faint()));
    }
    Line::from(spans)
}

fn modal_box(frame: &mut Frame, area: Rect, modal: &Modal, app: &App, theme: &Theme) {
    let width = area.width.saturating_sub(4).min(68);
    let text_width = width.saturating_sub(4) as usize;
    let (title, lines) = match modal {
        Modal::AddRoot { input } => {
            let mut spans = vec![Span::styled(PROMPT, theme.rainbow(0.48)), Span::raw(" ")];
            spans.push(Span::styled(
                tail(input, text_width.saturating_sub(3)),
                theme.ink(),
            ));
            let mut lines = vec![
                Line::styled("The folder to share with Chat with Work:", theme.muted()),
                Line::raw(""),
                Line::from(spans),
                Line::raw(""),
            ];
            lines.extend(
                wrap(
                    "Only this folder becomes searchable. Keys, .env files and password \
                     databases inside it stay private.",
                    text_width,
                )
                .into_iter()
                .map(|l| Line::styled(l, theme.faint())),
            );
            ("SHARE A FOLDER", lines)
        }
        Modal::RenameRoot { id, input } => {
            let spans = vec![
                Span::styled(PROMPT, theme.rainbow(0.48)),
                Span::raw(" "),
                Span::styled(tail(input, text_width.saturating_sub(3)), theme.ink()),
            ];
            let mut lines = vec![
                Line::styled(
                    "The name Chat with Work sees this folder by:",
                    theme.muted(),
                ),
                Line::raw(""),
                Line::from(spans),
                Line::raw(""),
            ];
            lines.extend(
                wrap(
                    &format!(
                        "Its files keep their paths under {id}:, so links to them keep working."
                    ),
                    text_width,
                )
                .into_iter()
                .map(|l| Line::styled(l, theme.faint())),
            );
            ("RENAME A FOLDER", lines)
        }
        Modal::ConfirmRemove { label, path, .. } => {
            let mut lines = vec![Line::styled(
                format!("Stop sharing {label}?"),
                theme.strong(),
            )];
            lines.push(Line::raw(""));
            lines.extend(
                wrap(
                    &format!(
                        "Chat with Work can no longer search or read {path}, and its entries \
                         leave the index. The folder itself isn't touched."
                    ),
                    text_width,
                )
                .into_iter()
                .map(|l| Line::styled(l, theme.muted())),
            );
            ("STOP SHARING", lines)
        }
        Modal::ConfirmBroad { path, reason } => {
            let mut lines = vec![Line::styled(
                format!("Share {path} anyway?"),
                theme.strong(),
            )];
            lines.push(Line::raw(""));
            lines.extend(
                wrap(
                    &format!(
                        "{reason}. Secrets inside it stay private, but everything else in it \
                         becomes searchable by Chat with Work."
                    ),
                    text_width,
                )
                .into_iter()
                .map(|l| Line::styled(l, theme.muted())),
            );
            ("A BROAD FOLDER", lines)
        }
        Modal::ConfirmDelete { title, .. } => {
            let mut lines = vec![Line::styled(format!("Delete {title}?"), theme.strong())];
            lines.push(Line::raw(""));
            lines.extend(
                wrap(
                    "It's gone from every list at once, its public link stops working, and \
                     Chat with Work purges it after 30 days.",
                    text_width,
                )
                .into_iter()
                .map(|l| Line::styled(l, theme.muted())),
            );
            ("DELETE CHAT", lines)
        }
        Modal::ConfirmLogout => {
            let mut lines = vec![Line::styled("Disconnect this computer?", theme.strong())];
            lines.push(Line::raw(""));
            lines.extend(
                wrap(
                    "This forgets its pairing: Chat with Work can no longer reach your folders, \
                     and chats stop here, until you pair again. Remove it in Chat with Work as \
                     well, under Settings, Computers.",
                    text_width,
                )
                .into_iter()
                .map(|l| Line::styled(l, theme.muted())),
            );
            ("DISCONNECT", lines)
        }
        Modal::Help { scroll } => {
            let all = help_lines(app, theme);
            // Room for the borders, the blank line and the hints.
            let room = (area.height as usize).saturating_sub(4).max(1);
            let scroll = (*scroll).min(all.len().saturating_sub(room));
            (
                "KEYS AND COMMANDS",
                all.into_iter().skip(scroll).take(room).collect(),
            )
        }
    };
    let mut lines = lines;
    lines.push(Line::raw(""));
    lines.push(key_hints(&modal_hints(modal), theme));
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.line())
        .style(theme.surface())
        .title(Span::styled(format!(" {title} "), theme.micro()));
    let inner = pad(block.inner(rect), 1, 0);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(lines), inner);
    if let Modal::AddRoot { input } | Modal::RenameRoot { input, .. } = modal {
        let x = inner.x + 2 + tail(input, text_width.saturating_sub(3)).chars().count() as u16;
        frame.set_cursor_position(Position::new(
            x.min(inner.right().saturating_sub(1)),
            inner.y + 2,
        ));
    }
}

fn help_lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let mut keys: Vec<(&str, &str)> = Vec::new();
    if app.chat.ready() {
        keys.extend([
            ("tab", "move between the chats and the composer"),
            ("↑ ↓  j k", "select a chat"),
            ("enter", "open a chat; send a message"),
            ("n", "new chat"),
            ("/", "search chats; in the composer, commands"),
            ("i", "write a message"),
            (
                "shift-enter",
                "a new line (also alt-enter, ctrl-j, or \\ then enter)",
            ),
            ("↑ ↓", "in the composer: earlier questions"),
            ("ctrl-a ctrl-e", "start and end of the line"),
            ("ctrl-u ctrl-k", "delete to the start or end of the line"),
            ("esc", "stop an answer; back to the chats"),
            ("pgup pgdn", "scroll the chat"),
            ("e", "show every tool step"),
            ("o", "open the chat in the browser"),
        ]);
    } else if matches!(app.chat.access, Access::NeedsApproval { .. }) {
        keys.push(("o", "ask to use your chats here"));
    }
    keys.extend([
        (
            ",",
            "settings: shared folders, activity, account, the daemon",
        ),
        ("tab ← →", "in settings: the next or previous page"),
        (
            "↑ ↓  j k",
            "in settings: select a folder, scroll the activity",
        ),
        ("a", "share a folder"),
        (
            "r",
            "rename the folder (Shared folders); look for the daemon again",
        ),
        (
            "d",
            "stop sharing the folder (Shared folders); disconnect (Account)",
        ),
        ("l", "the activity log"),
        ("p", "pause or resume answering Chat with Work"),
        ("c", "pair this computer, when it isn't"),
        ("s", "start the daemon, when it isn't running"),
        ("?", "this help"),
        ("ctrl-l", "draw the screen again"),
        ("ctrl-c", "clear what's typed; twice quits"),
        ("q", "quit"),
        ("mouse", "click to select and open; the wheel scrolls"),
        ("shift-drag", "select text while the mouse is on"),
    ]);
    let mut lines: Vec<Line<'static>> = keys
        .into_iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!("{key:<14}"), theme.strong()),
                Span::styled(what.to_string(), theme.muted()),
            ])
        })
        .collect();
    if app.chat.ready() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("COMMANDS", theme.micro()));
        for command in COMMANDS {
            let mut name = format!("/{}", command.name);
            if !command.args.is_empty() {
                name.push(' ');
                name.push_str(command.args);
            }
            let mut about = command.about.to_string();
            if !command.aliases.is_empty() {
                let aliases: Vec<String> =
                    command.aliases.iter().map(|a| format!("/{a}")).collect();
                about.push_str(&format!(" (also {})", aliases.join(", ")));
            }
            lines.push(Line::from(vec![
                Span::styled(format!("{name:<22}"), theme.strong()),
                Span::styled(about, theme.muted()),
            ]));
        }
        lines.push(Line::styled(
            "Start with // to ask something that begins with a slash.",
            theme.faint(),
        ));
    }
    lines
}

// ---------------------------------------------------------------- helpers

pub(super) fn pad(area: Rect, x: u16, y: u16) -> Rect {
    let x = x.min(area.width / 2);
    let y = y.min(area.height / 2);
    Rect::new(area.x + x, area.y + y, area.width - 2 * x, area.height - y)
}

pub(super) fn row(area: Rect, offset: u16) -> Rect {
    Rect::new(
        area.x,
        area.y + offset.min(area.height),
        area.width,
        1.min(area.height - offset.min(area.height)),
    )
}

pub(super) fn below(area: Rect, offset: u16) -> Rect {
    let offset = offset.min(area.height);
    Rect::new(area.x, area.y + offset, area.width, area.height - offset)
}

/// `left` and `right` on one line, pushed apart.
pub(super) fn spread(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: u16,
) -> Line<'static> {
    let used: usize = left.iter().chain(&right).map(Span::width).sum();
    let gap = (width as usize).saturating_sub(used).max(1);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(spans)
}

pub(super) fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Keep the end of a path, which says the most.
pub(super) fn ellipsize_start(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let mut out = String::from("…");
    out.extend(text.chars().skip(count + 1 - max));
    out
}

/// The end of what's being typed, so the cursor stays in view.
pub(super) fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(max)).collect()
}

/// Greedy word wrap by characters.
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let mut word = word.to_string();
            while word.chars().count() > width {
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                let rest = word.split_off(
                    word.char_indices()
                        .nth(width)
                        .map_or(word.len(), |(i, _)| i),
                );
                lines.push(word);
                word = rest;
            }
            let needed =
                line.chars().count() + usize::from(!line.is_empty()) + word.chars().count();
            if needed > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&word);
        }
        lines.push(line);
    }
    lines
}

/// How far along its two-second sweep the shimmer is.
pub(super) fn sweep(now: OffsetDateTime) -> f32 {
    (now.unix_timestamp_nanos().rem_euclid(2_000_000_000)) as f32 / 2_000_000_000.0
}

/// The mark beside "Thinking": the spinner, its colour going round the
/// rainbow as the web's ring does around the brand mark.
fn working_mark(theme: &Theme, now: OffsetDateTime) -> Span<'static> {
    let t = (now.unix_timestamp_nanos().rem_euclid(2_400_000_000)) as f32 / 2_400_000_000.0;
    Span::styled(spinner(now), theme.rainbow(1.0 - (2.0 * t - 1.0).abs()))
}

pub(super) fn spinner(now: OffsetDateTime) -> &'static str {
    let frame = (now.unix_timestamp_nanos() / 120_000_000).rem_euclid(SPINNER.len() as i128);
    SPINNER[frame as usize]
}

pub(super) fn local(at: OffsetDateTime, app: &App) -> OffsetDateTime {
    at.to_offset(app.utc_offset)
}

pub(super) fn clock(at: OffsetDateTime, app: &App) -> String {
    let t = local(at, app);
    format!("{:02}:{:02}", t.hour(), t.minute())
}

pub(super) fn clock_secs(at: OffsetDateTime, app: &App) -> String {
    let t = local(at, app);
    format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
}

pub(super) fn host(url: &str) -> String {
    url.split_once("://")
        .map_or(url, |(_, rest)| rest)
        .trim_end_matches('/')
        .to_string()
}

pub(super) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub(super) fn files(n: u64) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{} files", thousands(n))
    }
}

pub(super) fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", thousands(n as u64))
    }
}

/// A file's size, as the composer shows it.
fn size(n: u64) -> String {
    bytes(n as usize)
}

fn bytes(n: usize) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}
