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
use super::chat::{
    AccessRequest, ChatList, ChatSummary, Entry, Failure, Live, Named, Project, Source, Step,
    Transcript,
};
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
    App::new(UtcOffset::UTC)
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
        proxy: None,
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
        event(
            "2026-09-25T08:19:00Z",
            "shutdown_requested",
            Some("over the control channel"),
        ),
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
fn snapshot_pairing() {
    let mut app = app();
    running(&mut app, status("not_paired", vec![]));
    assert_eq!(
        app.update(char('c')),
        vec![Effect::Daemon(DaemonCommand::Pair)]
    );
    app.update(Msg::Daemon(DaemonMsg::Pairing(PairingMsg::Code {
        code: "WDJB-MJHT".into(),
        url: "https://chatwithwork.com/device?user_code=WDJB-MJHT".into(),
        name: "carmine-mbp".into(),
        fingerprint: "3sQ2x9…".into(),
        opened: true,
    })));
    assert_snapshot("pairing", &app);
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
fn shows_the_proxy_without_credentials() {
    let mut app = app();
    let mut s = status(
        "connected",
        vec![root("work-docs", "Work docs", "ready", 1532)],
    );
    s.proxy = Some(crate::proxy::ProxyInfo {
        url: "http://alice:***@proxy.corp:3128".into(),
        source: "config".into(),
    });
    running(&mut app, s);
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("via proxy proxy.corp:3128"), "{screen}");
    assert!(!screen.contains("alice"), "{screen}");
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
    let first = history().len() - 1;
    assert_eq!(app.log_scroll, first, "at most to the first entry");
    app.update(Msg::Daemon(DaemonMsg::Audit(Box::new(event(
        "2026-09-25T08:20:00Z",
        "resumed",
        None,
    )))));
    assert_eq!(app.log_scroll, first + 1, "scrolled view stays put");
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

#[test]
fn pairing_starts_only_when_needed_and_can_be_cancelled() {
    let mut app = app();
    running(
        &mut app,
        status(
            "connected",
            vec![root("work-docs", "Work docs", "ready", 3)],
        ),
    );
    assert!(app.update(char('c')).is_empty(), "already paired");
    assert_eq!(app.pairing, None);

    running(&mut app, status("revoked", vec![]));
    assert_eq!(
        app.update(char('c')),
        vec![Effect::Daemon(DaemonCommand::Pair)]
    );
    assert_eq!(app.pairing, Some(Pairing::Starting));
    assert!(app.update(char('c')).is_empty(), "one pairing at a time");
    assert_eq!(
        app.update(key(KeyCode::Esc)),
        vec![Effect::Daemon(DaemonCommand::CancelPairing)]
    );
    assert_eq!(app.pairing, None);
    // The cancelled pairing's thread reports back quietly.
    app.update(Msg::Daemon(DaemonMsg::Pairing(PairingMsg::Finished(Err(
        "pairing was cancelled".into(),
    )))));
    assert_eq!(app.notice, None);
}

#[test]
fn a_finished_pairing_says_so_and_looks_again() {
    let mut app = app();
    running(&mut app, status("not_paired", vec![]));
    app.update(char('c'));
    let effects = app.update(Msg::Daemon(DaemonMsg::Pairing(PairingMsg::Finished(Ok(
        "https://chatwithwork.com".into(),
    )))));
    assert_eq!(effects, vec![Effect::Daemon(DaemonCommand::Retry)]);
    assert_eq!(app.pairing, None);
    assert!(app.notice.as_ref().unwrap().text.contains("Paired with"));
}

#[test]
fn s_starts_a_daemon_that_isnt_running() {
    let mut app = app();
    app.update(Msg::Daemon(DaemonMsg::NotRunning {
        offline: Box::new(Offline::default()),
        audit: vec![],
        suggestion: None,
    }));
    assert_eq!(
        app.update(char('s')),
        vec![Effect::Daemon(DaemonCommand::InstallService)]
    );
}

#[test]
fn the_stopped_notice_goes_when_the_daemon_is_back() {
    let mut app = app();
    running(&mut app, status("connected", vec![]));
    app.update(Msg::Daemon(DaemonMsg::Lost));
    assert!(app.notice.is_some());
    running(&mut app, status("connected", vec![]));
    assert_eq!(app.notice, None);
}

// ------------------------------------------------------------------ chats

fn summary(number: u64, title: &str, updated_at: &str) -> ChatSummary {
    ChatSummary {
        number,
        title: title.into(),
        state: "idle".into(),
        project: None,
        mine: true,
        updated_at: updated_at.into(),
        url: format!("https://chatwithwork.com/482139075/chats/{number}"),
    }
}

fn chat_list() -> ChatList {
    let mut launch = summary(40, "Launch plan for Falcon", "2026-09-25T07:02:00Z");
    launch.project = Some(Project {
        id: 3,
        name: "Falcon".into(),
    });
    launch.mine = false;
    ChatList {
        chats: vec![
            summary(42, "Q3 budget", "2026-09-25T08:14:03Z"),
            launch,
            summary(38, "Onboarding checklist", "2026-09-24T16:30:00Z"),
            summary(31, "Vendor contracts", "2026-09-19T10:00:00Z"),
            summary(27, "Hiring plan", "2026-09-02T10:00:00Z"),
        ],
        projects: vec![Project {
            id: 3,
            name: "Falcon".into(),
        }],
        account: Named {
            name: "Plenty".into(),
        },
        user: Named {
            name: "Carmine".into(),
        },
        locked_reason: None,
    }
}

fn budget_transcript(state: &str) -> Transcript {
    let mut chat = summary(42, "Q3 budget", "2026-09-25T08:14:03Z");
    chat.state = state.into();
    Transcript {
        chat,
        locked_reason: None,
        entries: vec![
            Entry::User {
                id: 1,
                content: "What did we budget for Q3, and who signed it off?".into(),
                author: None,
            },
            Entry::Activity {
                id: 2,
                title: "Searched Drive and Carmine's MacBook".into(),
                details: Some("2 searches, read 1 file".into()),
                progress: None,
                services: vec!["Drive".into(), "Carmine's MacBook".into()],
                pending: false,
                steps: vec![
                    Step {
                        summary: "Searched Drive for “q3 budget”".into(),
                        pending: false,
                        files: vec!["Q3 plan.pdf".into(), "Budget 2026.xlsx".into()],
                    },
                    Step {
                        summary: "Read plans/q3.md".into(),
                        pending: false,
                        files: vec![],
                    },
                ],
            },
            Entry::Assistant {
                id: 3,
                content: "The Q3 budget is **€40k**, signed off by *Ada* on 12 September.\n\n\
                          - Marketing: €18k\n- Engineering: `€22k`\n\n\
                          Details are in [the plan](https://drive.google.com/file/d/q3)."
                    .into(),
                sources: vec![Source {
                    title: "Q3 plan.pdf".into(),
                    url: Some("https://drive.google.com/file/d/q3".into()),
                }],
            },
        ],
    }
}

fn chats_ready(app: &mut App) {
    running(
        app,
        status(
            "connected",
            vec![root("work-docs", "Work docs", "ready", 1532)],
        ),
    );
    app.update(Msg::Chat(ChatMsg::Listed(Ok(chat_list()))));
}

fn open_budget(app: &mut App) -> Vec<Effect> {
    chats_ready(app);
    app.update(key(KeyCode::Down));
    app.update(key(KeyCode::Enter))
}

#[test]
fn snapshot_chat_conversation() {
    let mut app = app();
    assert_eq!(
        open_budget(&mut app),
        vec![
            Effect::Chat(ChatCommand::Open(42)),
            Effect::Chat(ChatCommand::Follow(Some(42)))
        ]
    );
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(budget_transcript("idle")),
    }));
    app.update(key(KeyCode::Esc));
    assert_eq!(app.focus, Focus::Chats);
    assert_snapshot("chat_conversation", &app);
}

#[test]
fn snapshot_chat_streaming() {
    let mut app = app();
    open_budget(&mut app);
    let mut transcript = budget_transcript("processing");
    transcript.entries.push(Entry::User {
        id: 4,
        content: "And Q4?".into(),
        author: None,
    });
    transcript.entries.push(Entry::Activity {
        id: 5,
        title: "Searching Carmine's MacBook".into(),
        details: Some("1 search".into()),
        progress: None,
        services: vec!["Carmine's MacBook".into()],
        pending: true,
        steps: vec![Step {
            summary: "Searching for “q4 budget”…".into(),
            pending: true,
            files: vec![],
        }],
    });
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(transcript),
    }));
    app.update(Msg::Chat(ChatMsg::Live {
        chat: 42,
        live: Live::Progress("Reading plans/q4.md · 3 of 5".into()),
    }));
    assert_snapshot("chat_streaming", &app);
}

#[test]
fn snapshot_chat_needs_approval() {
    let mut app = app();
    running(
        &mut app,
        status(
            "connected",
            vec![root("work-docs", "Work docs", "ready", 1532)],
        ),
    );
    app.update(Msg::Chat(ChatMsg::Listed(Err(Failure {
        code: "chat_access_required".into(),
        message: "Allow this computer to use your chats in Settings".into(),
        approve_url: Some(
            "https://chatwithwork.com/482139075/settings?tab=connectors#computers".into(),
        ),
        requested: false,
    }))));
    assert_snapshot("chat_needs_approval", &app);

    assert_eq!(
        app.update(char('o')),
        vec![Effect::Chat(ChatCommand::RequestAccess)]
    );
    let url = "https://chatwithwork.com/482139075/settings?tab=connectors#computers";
    assert_eq!(
        app.update(Msg::Chat(ChatMsg::Access(Ok(AccessRequest {
            granted: false,
            requested: true,
            approve_url: Some(url.into()),
        })))),
        vec![Effect::Chat(ChatCommand::OpenUrl(url.into()))]
    );
    app.notice = None;
    assert_snapshot("chat_waiting_for_approval", &app);

    // Once allowed, the poll's list arrives and the chats show.
    app.update(Msg::Chat(ChatMsg::Listed(Ok(chat_list()))));
    assert!(app.chat.ready());
    assert_eq!(app.focus, Focus::Chats);
}

#[test]
fn a_request_made_elsewhere_is_waited_for_too() {
    let mut app = app();
    running(&mut app, status("connected", vec![]));
    let effects = app.update(Msg::Chat(ChatMsg::Listed(Err(Failure {
        code: "chat_access_required".into(),
        message: "Allow it".into(),
        approve_url: None,
        requested: true,
    }))));
    assert_eq!(effects, vec![Effect::Chat(ChatCommand::WaitForAccess)]);
}

#[test]
fn chats_taken_back_show_at_once_and_come_back_live() {
    let mut app = app();
    open_budget(&mut app);
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(budget_transcript("idle")),
    }));
    // The server refuses to follow the chat after a reconnect: ask why.
    assert_eq!(
        app.update(Msg::Chat(ChatMsg::Live {
            chat: 42,
            live: Live::Refused
        })),
        vec![Effect::Chat(ChatCommand::List)]
    );
    app.update(Msg::Chat(ChatMsg::Listed(Err(Failure {
        code: "chat_access_required".into(),
        message: "Allow it".into(),
        approve_url: None,
        requested: false,
    }))));
    assert!(matches!(app.chat.access, Access::NeedsApproval { .. }));
    assert_eq!(app.focus, Focus::Roots, "nothing to type into");

    // Allowed again: the open chat is read and followed again.
    assert_eq!(
        app.update(Msg::Chat(ChatMsg::Listed(Ok(chat_list())))),
        vec![
            Effect::Chat(ChatCommand::Follow(Some(42))),
            Effect::Chat(ChatCommand::Open(42)),
        ]
    );
    assert_eq!(app.chat.following, Following::Starting);
}

#[test]
fn snapshot_chat_search() {
    let mut app = app();
    chats_ready(&mut app);
    app.update(char('/'));
    for c in "pla".chars() {
        app.update(char(c));
    }
    assert_eq!(app.chat.visible().len(), 2, "Launch plan and Hiring plan");
    assert_snapshot("chat_search", &app);
    app.update(key(KeyCode::Down));
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![
            Effect::Chat(ChatCommand::Open(27)),
            Effect::Chat(ChatCommand::Follow(Some(27)))
        ]
    );
    assert_eq!(app.chat.search, None);
    assert_eq!(app.chat.selected_chat().map(|c| c.number), Some(27));
}

#[test]
fn snapshot_chat_new() {
    let mut app = app();
    chats_ready(&mut app);
    assert!(app.update(char('n')).is_empty(), "nothing followed yet");
    assert_eq!(app.focus, Focus::Composer);
    for c in "What changed in Falcon this week?".chars() {
        app.update(char(c));
    }
    assert_snapshot("chat_new", &app);
}

#[test]
fn chats_load_once_a_running_daemon_is_paired() {
    let mut app = app();
    let effects = app.update(Msg::Daemon(DaemonMsg::Status(Box::new(status(
        "not_paired",
        vec![],
    )))));
    assert!(effects.is_empty());
    assert_eq!(app.chat.access, Access::Unknown);

    let effects = app.update(Msg::Daemon(DaemonMsg::Status(Box::new(status(
        "connected",
        vec![],
    )))));
    assert_eq!(effects, vec![Effect::Chat(ChatCommand::List)]);
    assert_eq!(app.chat.access, Access::Loading);
    // More status events don't ask again.
    assert!(
        app.update(Msg::Daemon(DaemonMsg::Status(Box::new(status(
            "connected",
            vec![]
        )))))
        .is_empty()
    );
    app.update(Msg::Chat(ChatMsg::Listed(Ok(chat_list()))));
    assert!(app.chat.ready());
    assert_eq!(app.focus, Focus::Chats);
    assert_eq!(
        app.focus_order(),
        vec![Focus::Chats, Focus::Composer, Focus::Roots]
    );

    // The daemon going away forgets that chats worked.
    app.update(Msg::Daemon(DaemonMsg::Lost));
    assert_eq!(app.chat.access, Access::Unknown);
}

#[test]
fn a_streamed_answer_builds_up_then_the_chat_is_read_back() {
    let mut app = app();
    chats_ready(&mut app);
    app.update(char('n'));
    for c in "q3 budget?".chars() {
        app.update(char(c));
    }
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![Effect::Chat(ChatCommand::Send {
            chat: None,
            text: "q3 budget?".into()
        })]
    );
    assert!(app.chat.working());
    assert_eq!(app.chat.pending_question.as_deref(), Some("q3 budget?"));

    let mut started = summary(43, "Chat #43", "2026-09-25T08:20:00Z");
    started.state = "processing".into();
    let effects = app.update(Msg::Chat(ChatMsg::Sent {
        chat: None,
        result: Ok(started),
    }));
    assert_eq!(
        effects,
        vec![
            Effect::Chat(ChatCommand::List),
            Effect::Chat(ChatCommand::Follow(Some(43))),
            Effect::Chat(ChatCommand::Open(43)),
        ]
    );
    assert_eq!(app.chat.open, Some(43));

    // Changes while a read is under way ask for one more read, not many.
    for _ in 0..3 {
        assert!(
            app.update(Msg::Chat(ChatMsg::Live {
                chat: 43,
                live: Live::Changed
            }))
            .is_empty()
        );
    }
    for text in ["It's ", "€40k."] {
        app.update(Msg::Chat(ChatMsg::Live {
            chat: 43,
            live: Live::Chunk {
                message_id: 9,
                text: text.into(),
            },
        }));
    }
    assert_eq!(
        app.chat.streamed.get(&9).map(String::as_str),
        Some("It's €40k.")
    );

    let mut transcript = budget_transcript("processing");
    transcript.chat.number = 43;
    transcript.entries = vec![
        Entry::User {
            id: 8,
            content: "q3 budget?".into(),
            author: None,
        },
        Entry::Assistant {
            id: 9,
            content: String::new(),
            sources: vec![],
        },
    ];
    let effects = app.update(Msg::Chat(ChatMsg::Shown {
        chat: 43,
        result: Ok(transcript.clone()),
    }));
    assert_eq!(
        effects,
        vec![Effect::Chat(ChatCommand::Open(43))],
        "the stale read"
    );
    assert_eq!(
        app.chat.pending_question, None,
        "the chat shows the question"
    );
    assert!(
        app.chat.streamed.contains_key(&9),
        "an empty answer keeps what streamed"
    );
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("It's €40k."), "{screen}");

    transcript.chat.state = "idle".into();
    transcript.entries[1] = Entry::Assistant {
        id: 9,
        content: "It's €40k.".into(),
        sources: vec![],
    };
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 43,
        result: Ok(transcript),
    }));
    assert!(app.chat.streamed.is_empty());
    assert!(!app.chat.working());
    assert_eq!(app.next_wakeup(NOW), None, "nothing moves once it's done");
}

#[test]
fn esc_stops_an_answer_then_goes_back_to_the_list() {
    let mut app = app();
    open_budget(&mut app);
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(budget_transcript("processing")),
    }));
    assert_eq!(app.focus, Focus::Composer);
    assert_eq!(
        app.update(key(KeyCode::Esc)),
        vec![Effect::Chat(ChatCommand::Cancel(42))]
    );
    assert!(app.chat.stopping);
    assert!(app.update(key(KeyCode::Esc)).is_empty());
    assert_eq!(app.focus, Focus::Chats);
}

#[test]
fn a_failed_question_comes_back_to_the_composer() {
    let mut app = app();
    open_budget(&mut app);
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(budget_transcript("idle")),
    }));
    for c in "And Q4?".chars() {
        app.update(char(c));
    }
    app.update(key(KeyCode::Enter));
    assert!(app.chat.input.is_empty());
    app.update(Msg::Chat(ChatMsg::Sent {
        chat: Some(42),
        result: Err(Failure::new(
            "locked",
            "You're out of credits. Add credits in Settings to keep going.",
        )),
    }));
    assert_eq!(app.chat.input, "And Q4?");
    assert!(!app.chat.working());
    assert_eq!(app.notice.as_ref().unwrap().signal, Signal::Negative);
}

#[test]
fn a_locked_composer_says_why_and_sends_nothing() {
    let mut app = app();
    running(&mut app, status("connected", vec![]));
    let mut list = chat_list();
    list.locked_reason =
        Some("You're out of credits. Add credits in Settings to keep going.".into());
    app.update(Msg::Chat(ChatMsg::Listed(Ok(list))));
    app.update(char('n'));
    for c in "hi".chars() {
        app.update(char(c));
    }
    assert!(app.update(key(KeyCode::Enter)).is_empty());
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("You're out of credits"), "{screen}");
}

#[test]
fn only_pages_on_the_paired_server_open_in_the_browser() {
    let mut app = app();
    running(&mut app, status("connected", vec![]));
    let mut list = chat_list();
    list.chats[0].url = "https://evil.example/chats/42".into();
    app.update(Msg::Chat(ChatMsg::Listed(Ok(list))));
    app.update(key(KeyCode::Down));
    assert!(app.update(char('o')).is_empty(), "not the paired server");
    app.update(key(KeyCode::Down));
    assert_eq!(
        app.update(char('o')),
        vec![Effect::Chat(ChatCommand::OpenUrl(
            "https://chatwithwork.com/482139075/chats/40".into()
        ))]
    );
}

#[test]
fn live_updates_for_another_chat_are_ignored_and_offline_shows() {
    let mut app = app();
    open_budget(&mut app);
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(budget_transcript("processing")),
    }));
    assert!(
        app.update(Msg::Chat(ChatMsg::Live {
            chat: 7,
            live: Live::Chunk {
                message_id: 1,
                text: "x".into()
            }
        }))
        .is_empty()
    );
    assert!(app.chat.streamed.is_empty());
    app.update(Msg::Chat(ChatMsg::Live {
        chat: 42,
        live: Live::Offline,
    }));
    assert_eq!(app.chat.following, Following::Offline);
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("reconnecting"), "{screen}");
    assert_eq!(
        app.update(Msg::Chat(ChatMsg::Live {
            chat: 42,
            live: Live::Watching
        })),
        vec![Effect::Chat(ChatCommand::Open(42))],
        "catch up on what happened while offline"
    );
}

#[test]
fn chat_cards_say_what_to_do() {
    for (code, title) in [
        ("unsupported", "This server doesn't offer chats here"),
        ("revoked", "Chat with Work revoked this computer"),
        ("unreachable", "Can't reach Chat with Work"),
    ] {
        let mut app = app();
        running(&mut app, status("connected", vec![]));
        app.update(Msg::Chat(ChatMsg::Listed(Err(Failure::new(
            code, "Because.",
        )))));
        let screen = render_to_string(&app, 100, 30);
        assert!(screen.contains(title), "{code}: {screen}");
    }
}
