//! Drawing the app. [`render`] is a pure function of the state, the theme
//! and the time it is given, so snapshot tests can pin every screen.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use time::OffsetDateTime;

use super::app::{
    Access, App, ChatPane, Choice, Daemon, DaemonStatus, Focus, Following, FormRow, Hit, Hits,
    Modal, ModelList, Offline, Pairing, PickItem, PickerKind, RootState, View, field_has_choices,
    parse_ts,
};
use super::chat::{Attachment, ChatSummary, Entry, Question};
use super::commands::COMMANDS;
use super::markdown::{self, Footnotes};
use super::theme::{Signal, Theme};
use crate::audit::{AuditEntry, Decision};

const DOT: &str = "●";
const RING: &str = "○";
const PROMPT: &str = "❯";
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How long an audit entry counts as happening right now.
const LIVE_SECS: i64 = 5;

pub fn render(frame: &mut Frame, app: &App, theme: &Theme, now: OffsetDateTime) {
    render_hits(frame, app, theme, now);
}

/// [`render`], saying where it drew what the mouse can click.
pub fn render_hits(frame: &mut Frame, app: &App, theme: &Theme, now: OffsetDateTime) -> Hits {
    let mut hits = Hits::default();
    let area = frame.area();
    frame.render_widget(Block::new().style(theme.base()), area);
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let side_width = match body.width {
        90.. => 32,
        64.. => 26,
        _ => 0,
    };
    let [side, main] =
        Layout::horizontal([Constraint::Length(side_width), Constraint::Min(0)]).areas(body);
    if side.width > 0 {
        sidebar(frame, side, app, theme, now, &mut hits);
    }
    main_pane(frame, main, app, theme, now, &mut hits);
    footer_line(frame, footer, app, theme);
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

    let daemon = daemon_section(app, theme, inner.width);
    let roots = app.daemon.roots().len() as u16;
    // A blank line, the label, then two lines per folder, at most three.
    let roots_height = if roots == 0 { 4 } else { 2 + 2 * roots.min(3) };
    let [brand, chats_area, roots_area, daemon_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(roots_height),
        Constraint::Length(daemon.len() as u16),
    ])
    .areas(inner);

    let name = Line::from(vec![
        Span::styled("Chat with Work", theme.strong()),
        Span::styled("  LOCAL AGENT", theme.micro()),
    ]);
    frame.render_widget(Paragraph::new(name), brand);
    chats_section(frame, chats_area, app, theme, now, hits);
    roots_section(frame, below(roots_area, 1), app, theme, now, hits);
    frame.render_widget(Paragraph::new(daemon), daemon_area);
}

/// A section's micro-label. The focused one gets the rainbow dash.
fn label(text: &str, focused: bool, theme: &Theme) -> Vec<Span<'static>> {
    if focused {
        let mut spans = theme.rainbow_text("━━");
        spans.push(Span::raw(" "));
        spans.push(Span::styled(text.to_string(), theme.strong()));
        spans
    } else {
        vec![Span::styled(text.to_string(), theme.micro())]
    }
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
    let focused = app.focus == Focus::Chats;
    let width = area.width as usize;
    let visible = pane.visible();
    let count = if pane.ready() && !pane.list.chats.is_empty() {
        visible.len().to_string()
    } else {
        String::new()
    };
    let header = spread(
        label("CHATS", focused, theme),
        vec![Span::styled(count, theme.faint())],
        area.width,
    );
    frame.render_widget(Paragraph::new(header), row(area, 0));
    let body = below(area, 1);
    if !pane.ready() {
        let text = match &pane.access {
            Access::Loading => "Loading…".to_string(),
            Access::NeedsApproval {
                requested: false, ..
            } => "Need your OK · o asks".into(),
            Access::NeedsApproval { .. } => "Waiting for your OK".into(),
            Access::Unavailable(_) => "Not available here".into(),
            Access::Ready | Access::Unknown => match &app.daemon {
                Daemon::NotRunning(_) => "Start the daemon to chat".into(),
                Daemon::Running(s) if !s.paired || s.connection.connection == "revoked" => {
                    "Pair this computer to chat".into()
                }
                _ => "Looking for the daemon…".into(),
            },
        };
        frame.render_widget(
            Paragraph::new(Line::styled(ellipsize(&text, width), theme.faint())),
            body,
        );
        return;
    }

    // Rows, what clicking each does, and which of them is the selection.
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut targets: Vec<Option<Hit>> = Vec::new();
    let mut selected_row = 0;
    let marker = |selected: bool| {
        if selected && focused {
            Span::styled("▌", theme.ink())
        } else {
            Span::raw(" ")
        }
    };
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
    rows.push(new_chat);
    targets.push(Some(Hit::NewChat));
    if let Some(query) = &pane.search {
        let mut spans = vec![
            Span::raw(" "),
            Span::styled("/ ", theme.signal_ink(Signal::Live)),
            Span::styled(tail(query, width.saturating_sub(4)), theme.ink()),
        ];
        if visible.is_empty() {
            spans.push(Span::styled("  no match", theme.faint()));
        }
        rows.push(Line::from(spans));
        targets.push(None);
    } else if pane.list.chats.is_empty() {
        rows.push(Line::styled(" No chats yet", theme.faint()));
        targets.push(None);
    }
    let mut group = "";
    for (i, chat) in visible.iter().enumerate() {
        let this = day_group(&chat.updated_at, now, app);
        if this != group {
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
    let live = chat.processing();
    let room = width.saturating_sub(if live { 5 } else { 3 });
    let title = ellipsize(&chat.title, room);
    let mut spans = vec![marker, Span::raw(" "), Span::styled(title.clone(), style)];
    if let Some(project) = &chat.project {
        let left = room.saturating_sub(title.chars().count() + 3);
        if left >= 4 {
            spans.push(Span::styled(
                format!(" · {}", ellipsize(&project.name, left)),
                theme.faint(),
            ));
        }
    }
    let mut line = if live {
        spread(
            spans,
            vec![Span::styled(spinner(now), theme.signal(Signal::Live))],
            width as u16,
        )
    } else {
        Line::from(spans)
    };
    if highlighted {
        line = line.patch_style(theme.selected());
    }
    line
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

fn roots_section(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    let roots = app.daemon.roots();
    let focused = app.focus == Focus::Roots;
    let count = if roots.is_empty() {
        String::new()
    } else {
        roots.len().to_string()
    };
    let header = spread(
        label("ROOTS", focused, theme),
        vec![Span::styled(count, theme.faint())],
        area.width,
    );
    frame.render_widget(Paragraph::new(header), row(area, 0));
    if area.height < 2 {
        return;
    }
    if roots.is_empty() {
        let text = match app.daemon {
            Daemon::Unknown => vec![],
            _ => vec![
                Line::styled("Nothing shared yet", theme.muted()),
                Line::from(vec![
                    Span::styled("a", theme.strong()),
                    Span::styled(" shares a folder", theme.faint()),
                ]),
            ],
        };
        frame.render_widget(Paragraph::new(text), below(area, 1));
        return;
    }
    let visible = ((area.height - 1) / 2).max(1) as usize;
    let start = app.selected_root.saturating_sub(visible - 1);
    for (slot, (i, root)) in roots
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .enumerate()
    {
        let rect = Rect {
            y: area.y + 1 + slot as u16 * 2,
            height: 2,
            ..area
        };
        let selected = focused && i == app.selected_root;
        let (glyph, signal, state) = root_state(root, &app.daemon, now);
        let marker = if selected {
            Span::styled("▌", theme.ink())
        } else {
            Span::raw(" ")
        };
        let width = area.width as usize;
        let lines = vec![
            Line::from(vec![
                marker,
                Span::styled(glyph, theme.signal(signal)),
                Span::raw(" "),
                Span::styled(ellipsize(&root.label, width - 3), theme.ink()),
            ]),
            Line::from(vec![
                Span::raw("   "),
                Span::styled(ellipsize(&root.id, width / 2), theme.faint()),
                Span::styled(" · ", theme.faint()),
                Span::styled(state, theme.signal_ink(signal)),
            ]),
        ];
        let mut paragraph = Paragraph::new(lines);
        if selected {
            paragraph = paragraph.style(theme.selected());
        }
        hits.add(rect.intersection(area), Hit::Root(i));
        frame.render_widget(paragraph, rect.intersection(area));
    }
}

/// The glyph, colour and words for a root's state.
fn root_state(
    root: &RootState,
    daemon: &Daemon,
    now: OffsetDateTime,
) -> (&'static str, Signal, String) {
    if !root.available {
        return (RING, Signal::Negative, "missing".into());
    }
    if !matches!(daemon, Daemon::Running(_)) {
        return (RING, Signal::Idle, "not running".into());
    }
    match root.index.as_str() {
        "ready" => (DOT, Signal::Positive, files(root.indexed_files)),
        "indexing" => (
            spinner(now),
            Signal::Attention,
            format!("indexing {}", thousands(root.indexed_files)),
        ),
        "pending" => (spinner(now), Signal::Attention, "waiting to index".into()),
        "error" => (DOT, Signal::Negative, "index error".into()),
        "disabled" => (DOT, Signal::Positive, "live search".into()),
        other => (DOT, Signal::Idle, other.replace('_', " ")),
    }
}

fn daemon_section(app: &App, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(label("DAEMON", false, theme))];
    match &app.daemon {
        Daemon::Unknown => lines.push(Line::styled("Looking for it…", theme.faint())),
        Daemon::NotRunning(offline) => daemon_offline(&mut lines, offline, theme),
        Daemon::Running(status) => daemon_running(&mut lines, status, app, theme, width),
    }
    lines
}

fn daemon_offline(lines: &mut Vec<Line<'static>>, offline: &Offline, theme: &Theme) {
    lines.push(Line::from(vec![
        Span::styled(RING, theme.signal(Signal::Negative)),
        Span::styled(" not running", theme.signal_ink(Signal::Negative)),
    ]));
    lines.push(command_line("cww daemon install", theme));
    let paired = match &offline.server {
        Some(server) => format!("  paired · {}", host(server)),
        None => "  not paired".into(),
    };
    lines.push(Line::styled(paired, theme.faint()));
    if offline.paused {
        lines.push(Line::styled(
            "  paused",
            theme.signal_ink(Signal::Attention),
        ));
    }
}

fn daemon_running(
    lines: &mut Vec<Line<'static>>,
    status: &DaemonStatus,
    app: &App,
    theme: &Theme,
    width: u16,
) {
    let (glyph, signal, word) = connection_state(&status.connection.connection);
    let since = status
        .connection
        .since
        .as_deref()
        .and_then(parse_ts)
        .map(|t| format!("since {}", clock(t, app)))
        .unwrap_or_default();
    lines.push(spread(
        vec![
            Span::styled(glyph, theme.signal(signal)),
            Span::styled(format!(" {word}"), theme.signal_ink(signal)),
        ],
        vec![Span::styled(since, theme.faint())],
        width,
    ));
    match status.connection.connection.as_str() {
        "not_paired" | "revoked" => lines.push(command_line("cww login", theme)),
        _ => {
            let server = status.server.as_deref().map(host).unwrap_or_default();
            let mut text = format!("  {server}");
            if let Some(device) = &status.device_id {
                let long = format!(" · device {device}");
                let fits = text.chars().count() + long.chars().count() <= width as usize;
                text.push_str(if fits { &long } else { " · #" });
                if !fits {
                    text.push_str(device);
                }
            }
            lines.push(Line::styled(text, theme.muted()));
            if let Some(proxy) = &status.proxy {
                lines.push(Line::styled(
                    format!("  via proxy {}", proxy_host(&proxy.url)),
                    theme.muted(),
                ));
            }
        }
    }
    let answering = if status.paused {
        Span::styled("  paused", theme.signal_ink(Signal::Attention))
    } else {
        Span::styled("  answering", theme.muted())
    };
    lines.push(Line::from(vec![
        answering,
        Span::styled(format!(" · cww {}", status.version), theme.faint()),
    ]));
}

/// `host:port` of a proxy URL, without the scheme or the user name.
fn proxy_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.rsplit_once('@').map_or(rest, |(_, host)| host)
}

fn connection_state(state: &str) -> (&'static str, Signal, String) {
    match state {
        "connected" => (DOT, Signal::Positive, "connected".into()),
        "connecting" => (DOT, Signal::Attention, "connecting".into()),
        "offline" => (DOT, Signal::Negative, "offline".into()),
        "revoked" => (DOT, Signal::Negative, "revoked".into()),
        "not_paired" => (RING, Signal::Attention, "not paired".into()),
        other => (RING, Signal::Idle, other.replace('_', " ")),
    }
}

fn command_line(command: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("  $ ", theme.faint()),
        Span::styled(command.to_string(), theme.strong()),
    ])
}

// ------------------------------------------------------------------- main

fn main_pane(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    if app.view == View::Log {
        let [header, body] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);
        header_bar(frame, header, app, theme, now, hits);
        let body = pad(body, 2, 1);
        hits.add(body, Hit::Log);
        log_view(frame, body, app, theme);
        return;
    }
    // The composer grows with what's typed and the list under it, up to
    // half the pane.
    let view = composer_view(app, theme, area.width.saturating_sub(6) as usize, now);
    let most = (area.height / 2).max(3);
    let composer_height = (view.lines.len() as u16 + 2).clamp(3, most);
    let [header, body, activity, composer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(2),
        Constraint::Length(composer_height),
    ])
    .areas(area);
    header_bar(frame, header, app, theme, now, hits);
    chat_view(frame, pad(body, 2, 1), app, theme, now, hits);
    activity_line(frame, activity, app, theme, now);
    composer_box(frame, pad(composer, 1, 0), app, theme, now, view, hits);
}

fn header_bar(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    let block = Block::new()
        .borders(Borders::BOTTOM)
        .border_style(theme.line());
    let inner = pad(block.inner(area), 2, 0);
    frame.render_widget(block, area);
    let tab = |name: &str, view: View| {
        if app.view == view {
            Span::styled(name.to_string(), theme.strong())
        } else {
            Span::styled(name.to_string(), theme.faint())
        }
    };
    let tabs = vec![
        tab("CHAT", View::Chat),
        Span::raw("   "),
        tab("AUDIT LOG", View::Log),
    ];
    hits.add(
        Rect {
            width: 4,
            ..row(inner, 0)
        },
        Hit::Tab(View::Chat),
    );
    hits.add(
        Rect {
            x: inner.x + 7,
            width: 9,
            ..row(inner, 0)
        }
        .intersection(inner),
        Hit::Tab(View::Log),
    );
    frame.render_widget(
        Paragraph::new(spread(tabs, live_pill(app, theme, now), inner.width)),
        inner,
    );
}

/// Top right: whether Chat with Work can reach this computer right now.
fn live_pill(app: &App, theme: &Theme, now: OffsetDateTime) -> Vec<Span<'static>> {
    let pill = |glyph: &'static str, signal: Signal, text: &str| {
        vec![
            Span::styled(glyph, theme.signal(signal)),
            Span::styled(format!(" {text}"), theme.signal_ink(signal)),
        ]
    };
    match &app.daemon {
        Daemon::Unknown => pill(RING, Signal::Idle, "CHECKING"),
        Daemon::NotRunning(_) => pill(RING, Signal::Negative, "DAEMON STOPPED"),
        Daemon::Running(status) if status.paused => pill("‖", Signal::Attention, "PAUSED"),
        Daemon::Running(status) => match status.connection.connection.as_str() {
            "connected" => pill(DOT, Signal::Positive, "LIVE"),
            "connecting" => pill(spinner(now), Signal::Attention, "CONNECTING"),
            "not_paired" => pill(RING, Signal::Attention, "NOT PAIRED"),
            "revoked" => pill(DOT, Signal::Negative, "REVOKED"),
            _ => pill(DOT, Signal::Negative, "OFFLINE"),
        },
    }
}

/// Something that needs saying, with a signal edge.
struct Card {
    signal: Signal,
    title: String,
    lines: Vec<CardLine>,
}

enum CardLine {
    Text(String),
    Command(String),
    Path(String),
    /// Something to read off the screen: a code, a link.
    Value(String),
    Keys(Vec<(&'static str, &'static str)>),
}

/// The cards the chat view leads with: the daemon's state, then the
/// first-run offer.
fn cards(app: &App) -> Vec<Card> {
    let mut cards = Vec::new();
    match &app.pairing {
        Some(Pairing::Starting) => cards.push(Card {
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
        }) => cards.push(Card {
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
        None => {}
    }
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
        Daemon::Running(status) => {
            match status.connection.connection.as_str() {
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
            }
            if status.paused {
                cards.push(Card {
                    signal: Signal::Attention,
                    title: "Paused".into(),
                    lines: vec![
                        CardLine::Text(
                            "Chat with Work's requests for your files are refused until you \
                             resume."
                                .into(),
                        ),
                        CardLine::Keys(vec![("p", "resume")]),
                    ],
                });
            }
        }
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

fn card_lines(card: &Card, width: usize, theme: &Theme) -> Vec<Line<'static>> {
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

fn render_card(
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
    let width = area.width.min(76);
    // Inside a conversation, only what needs doing stays on top of it.
    let in_chat =
        app.chat.ready() && (app.chat.open.is_some() || app.chat.pending_question.is_some());
    for card in cards(app) {
        if in_chat && matches!(card.signal, Signal::Live | Signal::Idle) {
            continue;
        }
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

/// The open chat: its title, then the transcript, newest at the bottom.
fn conversation(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    if area.height < 2 {
        return;
    }
    let pane = &app.chat;
    let width = area.width as usize;
    let title = match pane.open_summary() {
        Some(chat) => chat.title.clone(),
        None if pane.open.is_some() => "Loading…".into(),
        None => "New chat".into(),
    };
    let mut meta: Vec<Span<'static>> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    match pane.open_summary() {
        Some(chat) => {
            parts.push(format!("#{}", chat.number));
            if let Some(project) = &chat.project {
                parts.push(project.name.clone());
            }
        }
        None if pane.open.is_none() => {
            if let Some(project) = &pane.project {
                parts.push(project.name.clone());
            }
        }
        None => {}
    }
    if let Some(model) = pane.model_name() {
        parts.push(model.to_string());
    }
    if !parts.is_empty() {
        meta.push(Span::styled(parts.join(" · "), theme.micro()));
    }
    match pane.following {
        Following::Live if pane.working() => {
            meta.push(Span::raw("  "));
            meta.push(Span::styled(DOT, theme.signal(Signal::Positive)));
            meta.push(Span::raw(" "));
            meta.push(Span::styled("LIVE", theme.signal_ink(Signal::Positive)));
        }
        Following::Offline => {
            meta.push(Span::raw("  "));
            meta.push(Span::styled(
                "reconnecting",
                theme.signal_ink(Signal::Attention),
            ));
        }
        Following::Refused | Following::Unsupported => {
            meta.push(Span::raw("  "));
            meta.push(Span::styled(
                "not live",
                theme.signal_ink(Signal::Attention),
            ));
        }
        _ => {}
    }
    let meta_width: usize = meta.iter().map(Span::width).sum();
    let heading = spread(
        vec![Span::styled(
            ellipsize(&title, width.saturating_sub(meta_width + 2)),
            theme.strong(),
        )],
        meta,
        area.width,
    );
    frame.render_widget(Paragraph::new(heading), row(area, 0));

    let body = below(area, 2);
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

fn notice_line(text: &str, signal: Signal, width: usize, theme: &Theme) -> Vec<Line<'static>> {
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

fn activity_line(frame: &mut Frame, area: Rect, app: &App, theme: &Theme, now: OffsetDateTime) {
    let block = Block::new()
        .borders(Borders::TOP)
        .border_style(theme.line());
    let inner = pad(block.inner(area), 2, 0);
    frame.render_widget(block, area);
    let Some(entry) = app.audit.last() else {
        let line = Line::from(vec![
            Span::styled("ACTIVITY  ", theme.micro()),
            Span::styled(
                "Nothing yet. Requests from Chat with Work show up here.",
                theme.faint(),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), inner);
        return;
    };
    let at = parse_ts(&entry.ts);
    let live = at.is_some_and(|t| (now - t).whole_seconds() < LIVE_SECS);
    let mut spans = if live {
        vec![
            Span::styled("LIVE", theme.signal_ink(Signal::Positive)),
            Span::raw("      "),
        ]
    } else {
        vec![Span::styled("ACTIVITY  ", theme.micro())]
    };
    let signal = entry_signal(entry);
    spans.push(Span::styled(DOT, theme.signal(signal)));
    spans.push(Span::raw(" "));
    spans.extend(summary(entry, theme));
    if let Some(at) = at {
        spans.push(Span::styled(
            format!(" · {}", relative(at, now, app)),
            theme.faint(),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
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
                "Reply…".into()
            } else {
                "Ask anything…".into()
            };
            view.lines.push(Line::from(vec![
                prompt(),
                Span::raw(" "),
                Span::styled(ellipsize(&text, room), theme.faint()),
            ]));
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

fn log_view(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
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

/// `search "budget" → 3 hits`, or `connected · https://...`.
fn summary(entry: &AuditEntry, theme: &Theme) -> Vec<Span<'static>> {
    if entry.event != "tool" {
        let mut spans = vec![Span::styled(entry.event.replace('_', " "), theme.ink())];
        if let Some(detail) = &entry.detail {
            spans.push(Span::styled(format!(" · {detail}"), theme.muted()));
        }
        return spans;
    }
    let mut spans = vec![Span::styled(
        entry.tool.clone().unwrap_or_else(|| "?".into()),
        theme.ink(),
    )];
    let target = target(entry);
    if !target.is_empty() {
        spans.push(Span::styled(format!(" {target}"), theme.muted()));
    }
    let (outcome, signal) = outcome(entry);
    spans.push(Span::styled(" → ", theme.faint()));
    spans.push(Span::styled(outcome, theme.signal_ink(signal)));
    if let Some(ms) = entry.duration_ms {
        spans.push(Span::styled(format!(" · {}", duration(ms)), theme.faint()));
    }
    spans
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

// ----------------------------------------------------------------- footer

fn footer_line(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let area = pad(area, 1, 0);
    if let Some(notice) = &app.notice {
        let glyph = match notice.signal {
            Signal::Idle => RING,
            _ => DOT,
        };
        let line = Line::from(vec![
            Span::styled(glyph, theme.signal(notice.signal)),
            Span::raw(" "),
            Span::styled(notice.text.clone(), theme.signal_ink(notice.signal)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    frame.render_widget(Paragraph::new(key_hints(&hints(app), theme)), area);
}

/// The keys that do something right now.
fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    match &app.modal {
        Some(Modal::AddRoot { .. }) => return vec![("enter", "share"), ("esc", "cancel")],
        Some(Modal::ConfirmRemove { .. }) => return vec![("y", "stop sharing"), ("n", "keep it")],
        Some(Modal::ConfirmBroad { .. }) => return vec![("y", "share anyway"), ("n", "cancel")],
        Some(Modal::ConfirmDelete { .. }) => return vec![("y", "delete it"), ("n", "keep it")],
        Some(Modal::ConfirmLogout) => return vec![("y", "forget it"), ("n", "keep it")],
        Some(Modal::Help { .. }) => return vec![("↑↓", "scroll"), ("any other key", "close")],
        None => {}
    }
    if app.focus == Focus::Composer {
        let pane = &app.chat;
        if pane.picker.is_some() {
            return vec![
                ("type", "to filter"),
                ("↑↓", "select"),
                ("enter", "pick"),
                ("esc", "close"),
            ];
        }
        if !app.menu().is_empty() {
            return vec![
                ("↑↓", "select"),
                ("tab", "complete"),
                ("enter", "run"),
                ("esc", "close"),
            ];
        }
        if pane.form.editing.is_some() || pane.reason {
            return vec![("enter", "keep it"), ("esc", "cancel")];
        }
        if pane.pending_decision().is_some() && pane.input.is_empty() {
            let mut keys = vec![("↑↓", "choose"), ("enter", "confirm"), ("y", "approve")];
            if pane
                .pending_decision()
                .is_some_and(|a| a.allow_for_rest_of_chat)
            {
                keys.push(("a", "always in this chat"));
            }
            keys.extend([("n", "deny"), ("esc", "back")]);
            return keys;
        }
        if pane.pending_question().is_some() && pane.input.is_empty() {
            return vec![
                ("↑↓", "choose"),
                ("← →", "change"),
                ("enter", "confirm"),
                ("esc", "back"),
            ];
        }
        let esc = if pane.working() {
            ("esc", "stop")
        } else {
            ("esc", "back")
        };
        return vec![
            ("enter", "send"),
            ("/", "commands"),
            ("shift-enter", "new line"),
            ("↑", "history"),
            esc,
            ("tab", "focus"),
            ("ctrl-c", "clear"),
        ];
    }
    if app.focus == Focus::Chats && app.chat.search.is_some() {
        return vec![
            ("type", "to search"),
            ("↑↓", "select"),
            ("enter", "open"),
            ("esc", "stop searching"),
        ];
    }
    if app.focus == Focus::Chats && app.view == View::Chat {
        let mut keys = vec![
            ("↑↓", "select"),
            ("enter", "open"),
            ("n", "new chat"),
            ("/", "search"),
            ("tab", "focus"),
            ("e", "steps"),
        ];
        if app.chat.open.is_some() || app.chat.selected_chat().is_some() {
            keys.push(("o", "open in browser"));
        }
        keys.extend([("l", "audit log"), ("?", "help"), ("q", "quit")]);
        return keys;
    }
    let mut keys = Vec::new();
    if app.pairing.is_some() {
        keys.push(("esc", "cancel pairing"));
    } else if app.can_pair() {
        keys.push(("c", "pair"));
    }
    if matches!(app.daemon, Daemon::NotRunning(_)) {
        keys.push(("s", "start daemon"));
    }
    if app.focus_order().len() > 1 {
        keys.push(("tab", "focus"));
    }
    if app.view == View::Log {
        keys.extend([("↑↓", "scroll"), ("l", "chat")]);
    } else {
        if !app.daemon.roots().is_empty() {
            keys.push(("↑↓", "select"));
        }
        keys.push(("a", "add folder"));
        if !app.daemon.roots().is_empty() {
            keys.push(("d", "remove"));
        }
        keys.push(("l", "audit log"));
    }
    match app.daemon.paused() {
        Some(true) => keys.push(("p", "resume")),
        Some(false) => keys.push(("p", "pause")),
        None => {}
    }
    if matches!(app.daemon, Daemon::NotRunning(_)) {
        keys.push(("r", "retry"));
    }
    if matches!(app.chat.access, Access::NeedsApproval { .. }) {
        keys.push(("o", "allow chats"));
    }
    keys.extend([("?", "help"), ("q", "quit")]);
    keys
}

fn key_hints(keys: &[(&'static str, &'static str)], theme: &Theme) -> Line<'static> {
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

// ----------------------------------------------------------------- modals

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
            let mut lines = vec![Line::styled(
                "Forget this computer's pairing?",
                theme.strong(),
            )];
            lines.push(Line::raw(""));
            lines.extend(
                wrap(
                    "Chat with Work can no longer reach your folders, and chats stop here, \
                     until you pair again. Revoke the computer in Chat with Work as well, under \
                     Settings, Computers.",
                    text_width,
                )
                .into_iter()
                .map(|l| Line::styled(l, theme.muted())),
            );
            ("LOG OUT", lines)
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
    lines.push(key_hints(&hints(app), theme));
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
    if let Modal::AddRoot { input } = modal {
        let x = inner.x + 2 + tail(input, text_width.saturating_sub(3)).chars().count() as u16;
        frame.set_cursor_position(Position::new(
            x.min(inner.right().saturating_sub(1)),
            inner.y + 2,
        ));
    }
}

fn help_lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let mut keys: Vec<(&str, &str)> = vec![
        ("↑ ↓  j k", "select a folder; scroll the audit log"),
        ("a", "share a folder"),
        ("d  x  del", "stop sharing the selected folder"),
        ("p", "pause or resume answering Chat with Work"),
        ("l", "switch between chat and the audit log"),
        ("pgup pgdn", "page through the audit log"),
        ("r", "look for the daemon again"),
    ];
    if app.chat.ready() {
        keys.extend([
            ("tab", "move between chats, the composer and folders"),
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
            ("e", "show every tool step"),
            ("o", "open the chat in the browser"),
        ]);
    } else if matches!(app.chat.access, Access::NeedsApproval { .. }) {
        keys.push(("o", "ask to use your chats here"));
    }
    keys.extend([
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

fn pad(area: Rect, x: u16, y: u16) -> Rect {
    let x = x.min(area.width / 2);
    let y = y.min(area.height / 2);
    Rect::new(area.x + x, area.y + y, area.width - 2 * x, area.height - y)
}

fn row(area: Rect, offset: u16) -> Rect {
    Rect::new(
        area.x,
        area.y + offset.min(area.height),
        area.width,
        1.min(area.height - offset.min(area.height)),
    )
}

fn below(area: Rect, offset: u16) -> Rect {
    let offset = offset.min(area.height);
    Rect::new(area.x, area.y + offset, area.width, area.height - offset)
}

/// `left` and `right` on one line, pushed apart.
fn spread(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let used: usize = left.iter().chain(&right).map(Span::width).sum();
    let gap = (width as usize).saturating_sub(used).max(1);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(spans)
}

fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Keep the end of a path, which says the most.
fn ellipsize_start(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let mut out = String::from("…");
    out.extend(text.chars().skip(count + 1 - max));
    out
}

/// The end of what's being typed, so the cursor stays in view.
fn tail(text: &str, max: usize) -> String {
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
fn sweep(now: OffsetDateTime) -> f32 {
    (now.unix_timestamp_nanos().rem_euclid(2_000_000_000)) as f32 / 2_000_000_000.0
}

/// The mark beside "Thinking": the spinner, its colour going round the
/// rainbow as the web's ring does around the brand mark.
fn working_mark(theme: &Theme, now: OffsetDateTime) -> Span<'static> {
    let t = (now.unix_timestamp_nanos().rem_euclid(2_400_000_000)) as f32 / 2_400_000_000.0;
    Span::styled(spinner(now), theme.rainbow(1.0 - (2.0 * t - 1.0).abs()))
}

fn spinner(now: OffsetDateTime) -> &'static str {
    let frame = (now.unix_timestamp_nanos() / 120_000_000).rem_euclid(SPINNER.len() as i128);
    SPINNER[frame as usize]
}

fn local(at: OffsetDateTime, app: &App) -> OffsetDateTime {
    at.to_offset(app.utc_offset)
}

fn clock(at: OffsetDateTime, app: &App) -> String {
    let t = local(at, app);
    format!("{:02}:{:02}", t.hour(), t.minute())
}

fn clock_secs(at: OffsetDateTime, app: &App) -> String {
    let t = local(at, app);
    format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
}

/// "2s ago", "5m ago", then the time of day. `App::next_wakeup` redraws
/// exactly when this text changes.
fn relative(at: OffsetDateTime, now: OffsetDateTime, app: &App) -> String {
    let secs = (now - at).whole_seconds().max(0);
    match secs {
        0 => "just now".into(),
        1..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        _ => format!("at {}", clock(at, app)),
    }
}

fn host(url: &str) -> String {
    url.split_once("://")
        .map_or(url, |(_, rest)| rest)
        .trim_end_matches('/')
        .to_string()
}

fn thousands(n: u64) -> String {
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

fn files(n: u64) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{} files", thousands(n))
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
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
