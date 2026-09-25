//! Snapshot tests for the screens, and tests for the reducer.
//!
//! Each snapshot renders the app at 100x30 with a fixed clock into
//! `src/tui/snapshots/<name>.txt`. Run with `UPDATE_SNAPSHOTS=1` to rewrite
//! them after a deliberate change, then read the diff.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use time::macros::datetime;
use time::{OffsetDateTime, UtcOffset};

use super::app::*;
use super::chat::{Availability, ChatEvent, NO_CHAT_API, Role, ToolState, ToolStep};
use super::theme::{Depth, Signal, Theme};
use super::ui::render;
use crate::audit::{AuditEntry, Decision};

const NOW: OffsetDateTime = datetime!(2026-09-25 08:20:00 UTC);

fn render_to_string(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| render(frame, app, &Theme::new(Depth::TrueColor), NOW))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Compare with `src/tui/snapshots/<name>.txt`, or write it when
/// `UPDATE_SNAPSHOTS=1`.
fn assert_snapshot(name: &str, app: &App) {
    let actual = render_to_string(app, 100, 30);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/tui/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|v| v == "1") {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| {
            panic!(
                "no snapshot at {}; run with UPDATE_SNAPSHOTS=1",
                path.display()
            )
        })
        // Git may check text files out with CRLF on Windows.
        .replace("\r\n", "\n");
    assert!(
        actual == expected,
        "snapshot {name} changed; run with UPDATE_SNAPSHOTS=1 to accept.\n\
         --- expected\n{expected}\n--- actual\n{actual}"
    );
}

// --------------------------------------------------------------- fixtures

fn app() -> App {
    App::new(
        Availability::Unavailable {
            reason: NO_CHAT_API.into(),
        },
        UtcOffset::UTC,
    )
}

fn root(id: &str, label: &str, index: &str, files: u64) -> RootState {
    RootState {
        id: id.into(),
        label: label.into(),
        available: true,
        index: index.into(),
        indexed_files: files,
        local_path: format!("/home/carmine/{label}"),
    }
}

fn status(connection: &str, roots: Vec<RootState>) -> DaemonStatus {
    DaemonStatus {
        protocol: 1,
        version: "0.1.0".into(),
        paired: connection != "not_paired",
        server: (connection != "not_paired").then(|| "https://chatwithwork.com".into()),
        device_id: (connection != "not_paired").then(|| "42".into()),
        paused: false,
        connection: Link {
            connection: connection.into(),
            since: Some("2026-09-25T08:14:03Z".into()),
            last_error: None,
        },
        roots,
    }
}

fn running(app: &mut App, status: DaemonStatus) {
    app.update(Msg::Daemon(DaemonMsg::Status(Box::new(status))));
}

fn documents() -> Suggestion {
    Suggestion {
        path: "/home/carmine/Documents".into(),
        label: "Documents".into(),
        exists: true,
        shared: false,
    }
}

fn tool(ts: &str, tool: &str, decision: Decision) -> AuditEntry {
    AuditEntry {
        ts: ts.into(),
        event: "tool".into(),
        tool: Some(tool.into()),
        decision: Some(decision),
        chat_id: Some("1234".into()),
        request_id: Some("7".into()),
        ..AuditEntry::default()
    }
}

fn event(ts: &str, event: &str, detail: Option<&str>) -> AuditEntry {
    AuditEntry {
        ts: ts.into(),
        event: event.into(),
        detail: detail.map(Into::into),
        ..AuditEntry::default()
    }
}

fn history() -> Vec<AuditEntry> {
    let mut search = tool("2026-09-25T08:19:58Z", "search", Decision::Allowed);
    search.query = Some("budget".into());
    search.results = Some(3);
    search.bytes = Some(1840);
    search.duration_ms = Some(1.3);
    let mut read = tool("2026-09-25T08:16:10Z", "read", Decision::Allowed);
    read.path = Some("work-docs:plans/q3.md".into());
    read.bytes = Some(5210);
    let mut denied = tool("2026-09-25T08:17:30Z", "read", Decision::Denied);
    denied.path = Some("work-docs:.env".into());
    denied.code = Some("denied".into());
    denied.reason = Some("this path is on the deny list (.env*)".into());
    let mut list = tool("2026-09-25T08:18:02Z", "list", Decision::Allowed);
    list.path = Some("projects:".into());
    list.results = Some(12);
    let mut error = tool("2026-09-25T08:18:40Z", "read", Decision::Error);
    error.path = Some("projects:big.pdf".into());
    error.code = Some("too_large".into());
    error.reason = Some("the file is larger than 64 MiB".into());
    vec![
        event("2026-09-25T08:14:00Z", "started", None),
        event(
            "2026-09-25T08:14:03Z",
            "connected",
            Some("https://chatwithwork.com"),
        ),
        read,
        denied,
        list,
        error,
        search,
    ]
}

fn key(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn char(c: char) -> Msg {
    key(KeyCode::Char(c))
}

// -------------------------------------------------------------- snapshots

#[test]
fn snapshot_daemon_not_running() {
    let mut app = app();
    let mut work = root("work-docs", "Work docs", "", 0);
    work.local_path = "/home/carmine/Documents/Work".into();
    app.update(Msg::Daemon(DaemonMsg::NotRunning {
        offline: Box::new(Offline {
            paired: false,
            server: None,
            device_id: None,
            paused: false,
            roots: vec![work],
            error: None,
        }),
        audit: history()[..2].to_vec(),
        suggestion: Some(documents()),
    }));
    assert_snapshot("daemon_not_running", &app);
}

#[test]
fn snapshot_not_paired() {
    let mut app = app();
    running(
        &mut app,
        status(
            "not_paired",
            vec![root("work-docs", "Work docs", "ready", 1532)],
        ),
    );
    assert_snapshot("not_paired", &app);
}

#[test]
fn snapshot_connected_with_roots() {
    let mut app = app();
    running(
        &mut app,
        status(
            "connected",
            vec![
                root("work-docs", "Work docs", "ready", 1532),
                root("projects", "Projects", "indexing", 214),
            ],
        ),
    );
    app.update(Msg::Daemon(DaemonMsg::AuditHistory(history())));
    assert_snapshot("connected_with_roots", &app);
}

#[test]
fn snapshot_paused() {
    let mut app = app();
    let mut s = status(
        "connected",
        vec![root("work-docs", "Work docs", "ready", 1532)],
    );
    s.paused = true;
    running(&mut app, s);
    app.update(Msg::Daemon(DaemonMsg::AuditHistory(vec![event(
        "2026-09-25T08:10:00Z",
        "paused",
        None,
    )])));
    assert_snapshot("paused", &app);
}

#[test]
fn snapshot_first_run_offer() {
    let mut app = app();
    running(&mut app, status("connected", vec![]));
    app.update(Msg::Daemon(DaemonMsg::Suggestions(vec![documents()])));
    assert_snapshot("first_run_offer", &app);
}

#[test]
fn snapshot_audit_log() {
    let mut app = app();
    running(
        &mut app,
        status(
            "connected",
            vec![root("work-docs", "Work docs", "ready", 1532)],
        ),
    );
    app.update(Msg::Daemon(DaemonMsg::AuditHistory(history())));
    app.update(char('l'));
    assert_snapshot("audit_log", &app);
}

#[test]
fn snapshot_chat_connect_state() {
    let mut app = app();
    running(
        &mut app,
        status(
            "connected",
            vec![root("work-docs", "Work docs", "ready", 1532)],
        ),
    );
    assert_snapshot("chat_connect_state", &app);
}

#[test]
fn snapshot_add_folder_dialog() {
    let mut app = app();
    running(
        &mut app,
        status(
            "connected",
            vec![root("work-docs", "Work docs", "ready", 1532)],
        ),
    );
    app.update(Msg::Daemon(DaemonMsg::Suggestions(vec![documents()])));
    app.update(char('a'));
    assert_snapshot("add_folder_dialog", &app);
}

#[test]
fn snapshot_remove_folder_dialog() {
    let mut app = app();
    running(
        &mut app,
        status(
            "connected",
            vec![root("work-docs", "Work docs", "ready", 1532)],
        ),
    );
    app.update(char('d'));
    assert_snapshot("remove_folder_dialog", &app);
}

#[test]
fn renders_at_any_size_without_panicking() {
    let mut apps = vec![app()];
    let mut a = app();
    running(
        &mut a,
        status(
            "connected",
            vec![
                root(
                    "work-docs",
                    "A label much longer than any sidebar",
                    "ready",
                    1532,
                ),
                root("projects", "Projects", "indexing", 214),
            ],
        ),
    );
    a.update(Msg::Daemon(DaemonMsg::AuditHistory(history())));
    apps.push(a.clone());
    a.update(char('d'));
    apps.push(a.clone());
    a.modal = Some(Modal::Help);
    apps.push(a.clone());
    a.modal = None;
    a.update(char('l'));
    apps.push(a);
    let mut offer = app();
    running(&mut offer, status("revoked", vec![]));
    offer.update(Msg::Daemon(DaemonMsg::Suggestions(vec![documents()])));
    offer.update(char('a'));
    apps.push(offer);
    for app in &apps {
        for (w, h) in [
            (1, 1),
            (20, 5),
            (40, 12),
            (63, 20),
            (64, 8),
            (90, 30),
            (240, 70),
        ] {
            render_to_string(app, w, h);
        }
    }
}

// ---------------------------------------------------------------- reducer

#[test]
fn quits_on_q_and_ctrl_c() {
    let mut app = app();
    assert_eq!(app.update(char('q')), vec![Effect::Quit]);
    let mut app = self::app();
    let ctrl_c = Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(app.update(ctrl_c), vec![Effect::Quit]);
    assert!(app.quit);
}

#[test]
fn pause_toggles_from_the_daemons_state() {
    let mut app = app();
    assert!(app.update(char('p')).is_empty(), "nothing known yet");
    running(&mut app, status("connected", vec![]));
    assert_eq!(
        app.update(char('p')),
        vec![Effect::Daemon(DaemonCommand::Pause)]
    );
    let mut s = status("connected", vec![]);
    s.paused = true;
    running(&mut app, s);
    assert_eq!(
        app.update(char('p')),
        vec![Effect::Daemon(DaemonCommand::Resume)]
    );
}

#[test]
fn documents_is_shared_only_on_an_explicit_yes() {
    let mut app = app();
    running(&mut app, status("connected", vec![]));
    app.update(Msg::Daemon(DaemonMsg::Suggestions(vec![documents()])));
    assert!(app.offer().is_some());

    // Other keys don't share it.
    for c in ['j', 'k', 'l', 'l', 'x'] {
        let effects = app.update(char(c));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Daemon(DaemonCommand::AddRoot { .. }))),
            "{c} shared a folder"
        );
    }
    assert_eq!(
        app.update(char('y')),
        vec![Effect::Daemon(DaemonCommand::AddRoot {
            path: "/home/carmine/Documents".into(),
            label: Some("Documents".into()),
            i_know: false,
        })]
    );
    assert!(app.offer().is_none(), "asked once");
}

#[test]
fn declining_the_offer_hides_it() {
    let mut app = app();
    running(&mut app, status("connected", vec![]));
    app.update(Msg::Daemon(DaemonMsg::Suggestions(vec![documents()])));
    assert!(app.update(char('n')).is_empty());
    assert!(app.offer().is_none());
    assert!(app.update(char('y')).is_empty());
}

#[test]
fn no_offer_when_something_is_shared_or_documents_is_missing() {
    let mut app = app();
    running(
        &mut app,
        status("connected", vec![root("a", "A", "ready", 1)]),
    );
    app.update(Msg::Daemon(DaemonMsg::Suggestions(vec![documents()])));
    assert!(app.offer().is_none());

    let mut app = self::app();
    running(&mut app, status("connected", vec![]));
    let mut missing = documents();
    missing.exists = false;
    app.update(Msg::Daemon(DaemonMsg::Suggestions(vec![missing])));
    assert!(app.offer().is_none());
}

#[test]
fn adding_a_folder_prefills_documents_and_takes_typing() {
    let mut app = app();
    running(&mut app, status("connected", vec![]));
    app.update(Msg::Daemon(DaemonMsg::Suggestions(vec![documents()])));
    app.update(char('a'));
    assert_eq!(
        app.modal,
        Some(Modal::AddRoot {
            input: "/home/carmine/Documents".into()
        })
    );
    // Ctrl-W drops the last path component; typing and paste append.
    app.update(Msg::Key(KeyEvent::new(
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
    )));
    for c in "Work".chars() {
        app.update(char(c));
    }
    app.update(Msg::Paste("/Plans\n".into()));
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![Effect::Daemon(DaemonCommand::AddRoot {
            path: "/home/carmine/Work/Plans".into(),
            label: None,
            i_know: false,
        })]
    );
    assert_eq!(app.modal, None);

    app.update(char('a'));
    assert!(app.update(key(KeyCode::Esc)).is_empty());
    assert_eq!(app.modal, None);
}

#[test]
fn a_too_broad_folder_asks_before_i_know() {
    let mut app = app();
    running(&mut app, status("connected", vec![]));
    let command = DaemonCommand::AddRoot {
        path: "/home/carmine".into(),
        label: None,
        i_know: false,
    };
    app.update(Msg::Daemon(DaemonMsg::Done {
        command,
        result: Err(
            "refusing to share /home/carmine: it is your home folder. Share a narrower \
                     folder, or pass --i-know if you really mean it"
                .into(),
        ),
    }));
    assert_eq!(
        app.modal,
        Some(Modal::ConfirmBroad {
            path: "/home/carmine".into(),
            reason: "It is your home folder".into(),
        })
    );
    assert_eq!(
        app.update(char('y')),
        vec![Effect::Daemon(DaemonCommand::AddRoot {
            path: "/home/carmine".into(),
            label: None,
            i_know: true,
        })]
    );
}

#[test]
fn removing_a_folder_needs_confirmation() {
    let mut app = app();
    running(
        &mut app,
        status(
            "connected",
            vec![root("a", "A", "ready", 1), root("b", "B", "ready", 1)],
        ),
    );
    app.update(key(KeyCode::Down));
    app.update(char('d'));
    assert!(matches!(app.modal, Some(Modal::ConfirmRemove { ref id, .. }) if id == "b"));
    assert!(app.update(char('z')).is_empty(), "other keys keep asking");
    assert!(app.update(char('n')).is_empty());
    assert_eq!(app.modal, None);

    app.update(char('x'));
    assert_eq!(
        app.update(char('y')),
        vec![Effect::Daemon(DaemonCommand::RemoveRoot { id: "b".into() })]
    );
}

#[test]
fn selection_stays_in_range_when_roots_go_away() {
    let mut app = app();
    running(
        &mut app,
        status(
            "connected",
            vec![root("a", "A", "ready", 1), root("b", "B", "ready", 1)],
        ),
    );
    app.update(key(KeyCode::Down));
    app.update(key(KeyCode::Down));
    assert_eq!(app.selected_root, 1);
    running(
        &mut app,
        status("connected", vec![root("a", "A", "ready", 1)]),
    );
    assert_eq!(app.selected_root, 0);
}

#[test]
fn command_results_become_notices() {
    let mut app = app();
    app.update(Msg::Daemon(DaemonMsg::Done {
        command: DaemonCommand::Pause,
        result: Ok("Paused.".into()),
    }));
    assert_eq!(app.notice.as_ref().unwrap().signal, Signal::Positive);
    app.update(Msg::Daemon(DaemonMsg::Done {
        command: DaemonCommand::RemoveRoot { id: "x".into() },
        result: Err("no root matches x".into()),
    }));
    let notice = app.notice.clone().unwrap();
    assert_eq!(notice.signal, Signal::Negative);
    assert_eq!(notice.text, "no root matches x");
    app.update(char('j'));
    assert_eq!(app.notice, None, "a key clears it");
}

#[test]
fn audit_history_and_live_entries_merge_without_duplicates() {
    let mut app = app();
    let history = history();
    let live = history.last().unwrap().clone();
    let newer = event("2026-09-25T08:19:59Z", "paused", None);
    app.update(Msg::Daemon(DaemonMsg::Audit(Box::new(live))));
    app.update(Msg::Daemon(DaemonMsg::Audit(Box::new(newer.clone()))));
    app.update(Msg::Daemon(DaemonMsg::AuditHistory(history.clone())));
    assert_eq!(app.audit.len(), history.len() + 1);
    assert_eq!(app.audit.last(), Some(&newer));
}

#[test]
fn the_log_view_scrolls_and_follows() {
    let mut app = app();
    app.update(Msg::Daemon(DaemonMsg::AuditHistory(history())));
    app.update(char('l'));
    assert_eq!(app.view, View::Log);
    app.update(key(KeyCode::PageUp));
    assert_eq!(app.log_scroll, 6, "at most to the first entry");
    app.update(Msg::Daemon(DaemonMsg::Audit(Box::new(event(
        "2026-09-25T08:20:00Z",
        "resumed",
        None,
    )))));
    assert_eq!(app.log_scroll, 7, "scrolled view stays put");
    app.update(key(KeyCode::End));
    assert_eq!(app.log_scroll, 0);
    app.update(key(KeyCode::Esc));
    assert_eq!(app.view, View::Chat);
}

#[test]
fn losing_the_daemon_forgets_its_state() {
    let mut app = app();
    running(
        &mut app,
        status("connected", vec![root("a", "A", "ready", 1)]),
    );
    app.update(Msg::Daemon(DaemonMsg::Lost));
    assert_eq!(app.daemon, Daemon::Unknown);
    assert_eq!(
        app.update(char('r')),
        vec![Effect::Daemon(DaemonCommand::Retry)]
    );
}

#[test]
fn a_newer_protocol_is_flagged() {
    let mut app = app();
    let mut s = status("connected", vec![]);
    s.protocol = 2;
    running(&mut app, s);
    assert_eq!(app.notice.as_ref().unwrap().signal, Signal::Attention);
}

#[test]
fn parses_the_daemons_status_json() {
    let json = serde_json::json!({
        "ok": true, "protocol": 1, "version": "0.1.0", "platform": "linux", "pid": 1,
        "paired": true, "server": "https://chatwithwork.com", "device_id": "42",
        "paused": false, "future_field": [1, 2],
        "connection": { "connection": "connected", "since": "2026-09-25T08:14:03Z" },
        "roots": [{ "id": "w", "label": "W", "available": false, "index": "ready",
                    "indexed_files": 3, "local_path": "/w" }]
    });
    let status: DaemonStatus = serde_json::from_value(json).unwrap();
    assert_eq!(status.connection.connection, "connected");
    assert!(!status.roots[0].available);
    assert_eq!(status.roots[0].indexed_files, 3);
}

#[test]
fn locked_composer_takes_no_input() {
    let mut app = app();
    running(&mut app, status("connected", vec![]));
    app.update(key(KeyCode::Tab));
    assert_eq!(app.focus, Focus::Roots, "nothing else to focus");
    app.update(char('i'));
    assert_eq!(app.focus, Focus::Roots);
    assert!(app.start().is_empty(), "no chat calls");
}

#[test]
fn redraws_only_when_something_changes() {
    let mut app = app();
    assert_eq!(app.next_wakeup(NOW), None, "idle");
    running(
        &mut app,
        status("connected", vec![root("a", "A", "indexing", 1)]),
    );
    assert_eq!(app.next_wakeup(NOW), Some(FRAME), "spinner");
    running(
        &mut app,
        status("connected", vec![root("a", "A", "ready", 1)]),
    );
    assert_eq!(app.next_wakeup(NOW), None);

    // "2s ago" ticks every second, "5m ago" every minute, then it stops.
    let recent = event("2026-09-25T08:19:58.250Z", "connected", None);
    app.update(Msg::Daemon(DaemonMsg::Audit(Box::new(recent))));
    assert_eq!(
        app.next_wakeup(NOW),
        Some(std::time::Duration::from_millis(250))
    );
    let later = NOW + time::Duration::minutes(5);
    assert_eq!(
        app.next_wakeup(later),
        Some(std::time::Duration::from_millis(58_250))
    );
    assert_eq!(app.next_wakeup(NOW + time::Duration::hours(2)), None);
    app.update(char('l'));
    assert_eq!(app.next_wakeup(NOW), None, "the log shows clock times");
}

/// The pane works against a backend that has an API. Only the reducer is
/// exercised here; no conversation is ever shown in the shipped app.
#[test]
fn a_streamed_reply_builds_up_the_answer() {
    let mut app = App::new(Availability::Available, UtcOffset::UTC);
    assert_eq!(
        app.start(),
        vec![Effect::Chat(super::app::ChatCommand::List)]
    );
    app.update(key(KeyCode::Tab));
    assert_eq!(app.focus, Focus::Composer);
    for c in "q3 budget?".chars() {
        app.update(char(c));
    }
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![Effect::Chat(super::app::ChatCommand::Send {
            chat: None,
            text: "q3 budget?".into()
        })]
    );
    assert!(app.chat.streaming);
    let step = |state| ToolStep {
        id: "t1".into(),
        service: "This computer".into(),
        summary: "search \"q3 budget\"".into(),
        state,
    };
    for event in [
        ChatEvent::Started {
            chat_id: "9".into(),
            message_id: "m2".into(),
        },
        ChatEvent::Tool(step(ToolState::Running)),
        ChatEvent::Tool(step(ToolState::Done)),
        ChatEvent::TextDelta("It's ".into()),
        ChatEvent::TextDelta("€40k.".into()),
        ChatEvent::Done,
    ] {
        app.update(Msg::Chat(ChatMsg::Event(event)));
    }
    assert!(!app.chat.streaming);
    assert_eq!(app.chat.open.as_deref(), Some("9"));
    let answer = app.chat.messages.last().unwrap();
    assert_eq!(answer.role, Role::Assistant);
    assert_eq!(answer.text, "It's €40k.");
    assert_eq!(answer.tools, vec![step(ToolState::Done)]);
}
