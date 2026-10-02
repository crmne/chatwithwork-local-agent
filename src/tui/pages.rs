//! The settings, as the desktop app has them: shared folders, the activity
//! log, the pairing with Chat with Work, and the daemon. Each is a page with
//! a title, a sentence on what it's for, and what it shows, with the keys it
//! takes written under it rather than in a footer.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use time::OffsetDateTime;

use super::app::{Access, App, Daemon, Hit, Hits, Page, RootState, parse_ts};
use super::theme::{Signal, Theme};
use super::ui::{
    COLUMN, Card, CardLine, DOT, RING, below, card_lines, clock, command_line, credits_left,
    ellipsize, ellipsize_start, files, heading, host, key_hints, log_view, notice_lines, pad,
    pairing_card, render_card, row, spinner, spread, thousands, wrap,
};

/// The width of a row's label, as in `Device ID      42`.
const LABEL: usize = 16;

/// The settings' list of pages, where the chats are in the sidebar.
pub fn nav(frame: &mut Frame, area: Rect, page: Page, theme: &Theme, hits: &mut Hits) {
    let back = Line::from(vec![
        Span::styled(" ← ", theme.faint()),
        Span::styled("Chats", theme.muted()),
    ]);
    let back = spread(
        back.spans,
        vec![Span::styled("esc", theme.faint())],
        area.width,
    );
    frame.render_widget(Paragraph::new(back), row(area, 0));
    hits.add(row(area, 0), Hit::Back);
    frame.render_widget(
        Paragraph::new(Line::styled(" SETTINGS", theme.micro())),
        row(area, 2),
    );
    for (i, each) in Page::ALL.iter().enumerate() {
        let at = row(area, 3 + i as u16);
        let here = *each == page;
        let line = if here {
            Line::from(vec![
                Span::styled("▌", theme.ink()),
                Span::raw(" "),
                Span::styled(each.title(), theme.strong()),
            ])
            .patch_style(theme.selected())
        } else {
            Line::from(vec![
                Span::raw("  "),
                Span::styled(each.title(), theme.muted()),
            ])
        };
        frame.render_widget(Paragraph::new(line), at);
        hits.add(at, Hit::Page(*each));
    }
}

/// A settings page in `area`. `bare` when there's no sidebar to list the
/// pages, so the heading says where Tab goes.
#[allow(clippy::too_many_arguments)]
pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    page: Page,
    bare: bool,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    let area = pad(area, 2, 0);
    if area.height == 0 {
        return;
    }
    let mut title = vec![Span::styled(page.title(), theme.strong())];
    if bare {
        title.insert(0, Span::styled("SETTINGS  ", theme.micro()));
        title.push(Span::styled("  tab for more", theme.faint()));
    }
    heading(frame, area, title, app, theme, now, hits);
    let width = area.width.min(COLUMN);
    let mut y = area.y + 2;
    for line in wrap(page.about(), width as usize) {
        if y >= area.bottom() {
            return;
        }
        frame.render_widget(
            Paragraph::new(Line::styled(line, theme.muted())),
            Rect::new(area.x, y, width, 1),
        );
        y += 1;
    }
    let notice = notice_lines(app, width as usize, theme);
    let bottom = area.bottom().saturating_sub(notice.len() as u16);
    if !notice.is_empty() {
        frame.render_widget(
            Paragraph::new(notice.clone()),
            Rect::new(area.x, bottom, area.width, notice.len() as u16),
        );
    }
    let body = Rect::new(
        area.x,
        (y + 1).min(bottom),
        area.width,
        bottom.saturating_sub(y + 1),
    );
    match page {
        Page::Folders => folders(frame, body, app, theme, now, hits),
        Page::Activity => {
            hits.add(body, Hit::Log);
            log_view(frame, body, app, theme);
        }
        Page::Account => account(frame, body, app, theme, hits),
        Page::General => general(frame, body, app, theme),
    }
}

/// Draw `lines` at the top of `area`, as many as fit.
fn draw(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    frame.render_widget(Paragraph::new(lines), area);
}

/// `Label           value`.
fn field(label: &str, value: Vec<Span<'static>>, theme: &Theme) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label:<LABEL$}"), theme.muted())];
    spans.extend(value);
    Line::from(spans)
}

/// Cards, one under the other, from `y` down; the next free row.
fn cards(frame: &mut Frame, area: Rect, cards: Vec<Card>, theme: &Theme) -> u16 {
    let width = area.width.min(COLUMN);
    let mut y = area.y;
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
    y
}

// ---------------------------------------------------------- shared folders

fn folders(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    now: OffsetDateTime,
    hits: &mut Hits,
) {
    let roots = app.daemon.roots();
    let width = area.width.min(COLUMN);
    if roots.is_empty() {
        let mut list = Vec::new();
        if let Some(suggestion) = app.offer() {
            list.push(Card {
                signal: Signal::Live,
                title: format!("Share your {} folder?", suggestion.label),
                lines: vec![
                    CardLine::Path(suggestion.path.clone()),
                    CardLine::Text(
                        "Keys, .env files and password databases inside it stay private \
                         either way."
                            .into(),
                    ),
                    CardLine::Keys(vec![
                        ("y", "share it"),
                        ("n", "not now"),
                        ("a", "pick another folder"),
                    ]),
                ],
            });
        } else if !matches!(app.daemon, Daemon::Unknown) {
            list.push(Card {
                signal: Signal::Idle,
                title: "Nothing is shared yet".into(),
                lines: vec![
                    CardLine::Text("Chat with Work can only search folders you share.".into()),
                    CardLine::Keys(vec![("a", "share a folder")]),
                ],
            });
        }
        cards(frame, area, list, theme);
        return;
    }
    // Two lines a folder, then the keys.
    let room = (area.height.saturating_sub(2) / 2).max(1) as usize;
    let selected = app.selected_root.min(roots.len() - 1);
    let start = (selected + 1).saturating_sub(room);
    let mut y = area.y;
    for (i, root) in roots.iter().enumerate().skip(start).take(room) {
        let rect = Rect::new(area.x, y, width, 2).intersection(area);
        if rect.height == 0 {
            break;
        }
        let here = i == selected;
        let (glyph, signal, state) = root_state(root, &app.daemon, now);
        let marker = if here {
            Span::styled("▌", theme.ink())
        } else {
            Span::raw(" ")
        };
        let state_width = state.chars().count() + 1;
        let label_room = (width as usize).saturating_sub(state_width + 5);
        let first = spread(
            vec![
                marker,
                Span::styled(glyph, theme.signal(signal)),
                Span::raw(" "),
                Span::styled(ellipsize(&root.label, label_room), theme.strong()),
            ],
            vec![Span::styled(state, theme.signal_ink(signal))],
            width,
        );
        let id = format!(" · {}:", root.id);
        let path_room = (width as usize).saturating_sub(id.chars().count() + 3);
        let second = Line::from(vec![
            Span::raw("   "),
            Span::styled(ellipsize_start(&root.local_path, path_room), theme.faint()),
            Span::styled(id, theme.faint()),
        ]);
        let mut lines = Paragraph::new(vec![first, second]);
        if here {
            lines = lines.style(theme.selected());
        }
        frame.render_widget(lines, rect);
        hits.add(rect, Hit::Root(i));
        y += 2;
    }
    let mut after = vec![Line::raw("")];
    if matches!(app.daemon, Daemon::NotRunning(_)) {
        after.push(Line::styled(
            "The daemon isn't running: changes are saved in config.toml for when it starts.",
            theme.faint(),
        ));
    }
    after.push(key_hints(
        &[
            ("↑↓", "select"),
            ("a", "share a folder"),
            ("r", "rename"),
            ("d", "stop sharing"),
        ],
        theme,
    ));
    draw(frame, below(area, y - area.y), after);
}

/// The glyph, colour and words for a folder's state.
fn root_state(
    root: &RootState,
    daemon: &Daemon,
    now: OffsetDateTime,
) -> (&'static str, Signal, String) {
    if !root.available {
        return (RING, Signal::Negative, "Folder not found".into());
    }
    if !matches!(daemon, Daemon::Running(_)) {
        return (RING, Signal::Idle, "Not running".into());
    }
    match root.index.as_str() {
        "ready" => (
            DOT,
            Signal::Positive,
            format!("Indexed · {}", files(root.indexed_files)),
        ),
        "indexing" => (
            spinner(now),
            Signal::Attention,
            format!("Indexing · {}", thousands(root.indexed_files)),
        ),
        "pending" => (spinner(now), Signal::Attention, "Waiting to index".into()),
        "error" => (DOT, Signal::Negative, "Index error".into()),
        "disabled" => (DOT, Signal::Positive, "Searched live".into()),
        other => (DOT, Signal::Idle, other.replace('_', " ")),
    }
}

// ----------------------------------------------------------------- account

fn account(frame: &mut Frame, area: Rect, app: &App, theme: &Theme, hits: &mut Hits) {
    if let Some(card) = pairing_card(app) {
        cards(frame, area, vec![card], theme);
        return;
    }
    let mut lines = Vec::new();
    let (server, device, connection) = match &app.daemon {
        Daemon::Unknown => {
            draw(
                frame,
                area,
                vec![Line::styled("Looking for the daemon…", theme.faint())],
            );
            return;
        }
        Daemon::NotRunning(offline) => (offline.server.clone(), offline.device_id.clone(), None),
        Daemon::Running(status) => (
            status.server.clone(),
            status.device_id.clone(),
            Some(status),
        ),
    };
    let revoked = connection.is_some_and(|s| s.connection.connection == "revoked");
    match server.filter(|_| connection.is_none_or(|s| s.connection.connection != "not_paired")) {
        None => {
            lines.push(Line::styled("This computer isn't paired", theme.strong()));
            lines.extend(
                wrap(
                    "Pair it with your Chat with Work account so Chat with Work can search the \
                     folders you share. The page to approve it opens in your browser.",
                    area.width.min(COLUMN) as usize,
                )
                .into_iter()
                .map(|l| Line::styled(l, theme.muted())),
            );
            lines.push(Line::raw(""));
            lines.push(key_hints(&[("c", "pair this computer")], theme));
            draw(frame, area, lines);
            return;
        }
        Some(server) => {
            lines.push(field(
                "Chat with Work",
                vec![Span::styled(host(&server), theme.ink())],
                theme,
            ));
        }
    }
    let state = match connection {
        None => vec![
            Span::styled(RING, theme.faint()),
            Span::styled(" The daemon isn't running", theme.muted()),
        ],
        Some(status) => {
            let (glyph, signal, text) = match status.connection.connection.as_str() {
                "connected" => {
                    let since = status
                        .connection
                        .since
                        .as_deref()
                        .and_then(parse_ts)
                        .map(|t| format!("Online since {}", clock(t, app)))
                        .unwrap_or_else(|| "Online".into());
                    (DOT, Signal::Positive, since)
                }
                "connecting" => (DOT, Signal::Attention, "Connecting…".into()),
                "revoked" => (DOT, Signal::Negative, "Revoked".into()),
                _ => (DOT, Signal::Negative, "Offline".into()),
            };
            vec![
                Span::styled(glyph, theme.signal(signal)),
                Span::styled(format!(" {text}"), theme.signal_ink(signal)),
            ]
        }
    };
    lines.push(field("Connection", state, theme));
    if let Some(error) = connection
        .filter(|s| s.connection.connection != "connected")
        .and_then(|s| s.connection.last_error.as_deref())
    {
        let room = (area.width.min(COLUMN) as usize).saturating_sub(LABEL);
        for part in wrap(error, room) {
            lines.push(field("", vec![Span::styled(part, theme.faint())], theme));
        }
    }
    if let Some(device) = device {
        lines.push(field(
            "Device ID",
            vec![Span::styled(device, theme.ink())],
            theme,
        ));
    }
    if let Some(proxy) = connection.and_then(|s| s.proxy.as_ref()) {
        lines.push(field(
            "Proxy",
            vec![Span::styled(
                proxy_host(&proxy.url).to_string(),
                theme.ink(),
            )],
            theme,
        ));
    }
    let chats = match &app.chat.access {
        Access::Ready => (Signal::Positive, "Allowed".to_string()),
        Access::Loading => (Signal::Idle, "Checking…".into()),
        Access::NeedsApproval {
            requested: false, ..
        } => (Signal::Attention, "Not allowed yet".into()),
        Access::NeedsApproval { .. } => (Signal::Attention, "Waiting for your OK".into()),
        Access::Unavailable(failure) => (Signal::Negative, failure.message.clone()),
        Access::Unknown => (Signal::Idle, "Unknown until the daemon connects".into()),
    };
    lines.push(field(
        "Chats here",
        vec![Span::styled(
            ellipsize(&chats.1, (area.width as usize).saturating_sub(LABEL)),
            theme.signal_ink(chats.0),
        )],
        theme,
    ));
    let list = &app.chat.list;
    if app.chat.ready() {
        if !list.account.name.is_empty() {
            lines.push(field(
                "Organization",
                vec![Span::styled(list.account.name.clone(), theme.ink())],
                theme,
            ));
        }
        if let Some(credits) = &list.credits {
            let signal = if credits.running_low {
                Signal::Attention
            } else {
                Signal::Idle
            };
            lines.push(field(
                "Credits",
                vec![Span::styled(
                    credits_left(credits),
                    theme.signal_ink(signal),
                )],
                theme,
            ));
        }
    }
    lines.push(Line::raw(""));
    if revoked {
        lines.extend(
            wrap(
                "Chat with Work no longer accepts this computer. Pair it again to keep sharing.",
                area.width.min(COLUMN) as usize,
            )
            .into_iter()
            .map(|l| Line::styled(l, theme.signal_ink(Signal::Attention))),
        );
        lines.push(Line::raw(""));
    }
    let mut keys = Vec::new();
    if revoked {
        keys.push(("c", "pair again"));
    }
    if matches!(app.chat.access, Access::NeedsApproval { .. }) {
        keys.push(("o", "allow chats here"));
    }
    keys.push(("d", "disconnect this computer"));
    lines.push(key_hints(&keys, theme));
    // The web's own settings, to open in the browser.
    let links = &list.links.settings;
    if app.chat.ready() && !links.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("ON THE WEB", theme.micro()));
        let first = lines.len() as u16;
        let selected = app.selected_link.min(links.len() - 1);
        for (i, link) in links.iter().enumerate() {
            let here = i == selected;
            let mut line = Line::from(vec![
                if here {
                    Span::styled("▌", theme.ink())
                } else {
                    Span::raw(" ")
                },
                Span::styled(format!("{:<LABEL$}", link.label), theme.ink()),
                Span::styled(host_path(&link.url), theme.faint()),
            ]);
            if here {
                line = line.patch_style(theme.selected());
            }
            lines.push(line);
            let at = first + i as u16;
            if at < area.height {
                hits.add(
                    Rect::new(area.x, area.y + at, area.width.min(COLUMN), 1),
                    Hit::Web(i),
                );
            }
        }
        lines.push(key_hints(
            &[("↑↓", "select"), ("enter", "open in the browser")],
            theme,
        ));
    }
    draw(frame, area, lines);
}

/// `chatwithwork.com/…/settings?tab=usage`, shorter: the path after the
/// organization.
fn host_path(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    match rest.split_once('/') {
        Some((host, path)) => {
            let path = path.split_once('/').map_or(path, |(_, p)| p);
            format!("{host}/…/{path}")
        }
        None => rest.to_string(),
    }
}

/// `host:port` of a proxy URL, without the scheme or the user name.
fn proxy_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.rsplit_once('@').map_or(rest, |(_, host)| host)
}

// ----------------------------------------------------------------- general

fn general(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let mut lines = Vec::new();
    let agent = match &app.daemon {
        Daemon::Unknown => vec![Span::styled("Looking for it…", theme.faint())],
        Daemon::NotRunning(_) => vec![
            Span::styled(RING, theme.signal(Signal::Negative)),
            Span::styled(" Not running", theme.signal_ink(Signal::Negative)),
        ],
        Daemon::Running(status) => vec![
            Span::styled(DOT, theme.signal(Signal::Positive)),
            Span::styled(" Running", theme.ink()),
            Span::styled(format!(" · cww {}", status.version), theme.faint()),
        ],
    };
    lines.push(field("Local Agent", agent, theme));
    let sharing = match app.daemon.paused() {
        None => vec![Span::styled("–", theme.faint())],
        Some(true) => vec![
            Span::styled("‖", theme.signal(Signal::Attention)),
            Span::styled(
                " Paused: requests from Chat with Work are refused",
                theme.signal_ink(Signal::Attention),
            ),
        ],
        Some(false) => vec![Span::styled("Answering Chat with Work", theme.ink())],
    };
    lines.push(field("Sharing", sharing, theme));
    let (config, audit) = app.daemon.files();
    let room = (area.width.min(COLUMN) as usize).saturating_sub(LABEL);
    for (label, path) in [("Configuration", config), ("Activity log", audit)] {
        if let Some(path) = path {
            lines.push(field(
                label,
                vec![Span::styled(
                    ellipsize_start(&home(path), room),
                    theme.muted(),
                )],
                theme,
            ));
        }
    }
    lines.push(Line::raw(""));
    if matches!(app.daemon, Daemon::NotRunning(_)) {
        let width = area.width.min(COLUMN) as usize;
        lines.extend(
            wrap(
                "Chat with Work can't search your folders until it runs. Start it in the \
                 background, where it starts again whenever you log in:",
                width,
            )
            .into_iter()
            .map(|l| Line::styled(l, theme.muted())),
        );
        lines.push(command_line("cww daemon install", theme));
        lines.push(Line::raw(""));
    }
    let mut keys = Vec::new();
    match app.daemon.paused() {
        Some(true) => keys.push(("p", "resume sharing")),
        Some(false) => keys.push(("p", "pause sharing")),
        None => {}
    }
    if matches!(app.daemon, Daemon::NotRunning(_)) {
        keys.push(("s", "start the daemon"));
    }
    keys.push(("r", "look again"));
    lines.push(key_hints(&keys, theme));
    draw(frame, area, lines);
}

/// `~/.config/cww/config.toml` rather than the whole home path.
fn home(path: &str) -> String {
    match crate::paths::home_dir() {
        Ok(home) => {
            let home = home.display().to_string();
            match path.strip_prefix(&home) {
                Some(rest) if rest.starts_with(['/', '\\']) => format!("~{rest}"),
                _ => path.to_string(),
            }
        }
        Err(_) => path.to_string(),
    }
}
