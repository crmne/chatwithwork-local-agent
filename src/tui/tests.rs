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
        config_file: Some("/srv/cww/config.toml".into()),
        audit_file: Some("/srv/cww/audit.jsonl".into()),
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
            config_file: None,
            audit_file: None,
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
    app.update(Msg::Chat(ChatMsg::Listed(Ok(chat_list()))));
    app.update(char(','));
    assert_eq!(app.view, View::Settings(Page::Folders));
    assert_snapshot("connected_with_roots", &app);
}

#[test]
fn snapshot_settings_account_and_general() {
    let mut app = app();
    chats_ready(&mut app);
    app.update(char(','));
    app.update(char('3'));
    assert_snapshot("settings_account", &app);
    app.update(key(KeyCode::Tab));
    assert_eq!(app.view, View::Settings(Page::General));
    assert_snapshot("settings_general", &app);
}

#[test]
fn settings_go_page_to_page_and_back() {
    let mut app = app();
    chats_ready(&mut app);
    for (key_code, page) in [
        (KeyCode::Char(','), Page::Folders),
        (KeyCode::Tab, Page::Activity),
        (KeyCode::Right, Page::Account),
        (KeyCode::BackTab, Page::Activity),
        (KeyCode::Left, Page::Folders),
        (KeyCode::BackTab, Page::General),
        (KeyCode::Char('2'), Page::Activity),
    ] {
        app.update(key(key_code));
        assert_eq!(app.view, View::Settings(page), "{key_code:?}");
    }
    app.update(key(KeyCode::Esc));
    assert_eq!(app.view, View::Chat);
    app.update(char(','));
    app.update(char(','));
    assert_eq!(app.view, View::Chat, "a comma goes back too");

    // Where something needs you, the settings open there.
    running(&mut app, status("revoked", vec![]));
    app.update(char(','));
    assert_eq!(app.view, View::Settings(Page::Account));
    app.update(key(KeyCode::Esc));
    app.update(Msg::Daemon(DaemonMsg::NotRunning {
        offline: Box::new(Offline::default()),
        audit: vec![],
        suggestion: None,
    }));
    app.update(char(','));
    assert_eq!(app.view, View::Settings(Page::General));
    app.update(key(KeyCode::Esc));
    app.focus = Focus::Composer;
    type_text(&mut app, "/settings");
    app.update(key(KeyCode::Enter));
    assert_eq!(app.view, View::Settings(Page::General));
}

#[test]
fn a_folder_is_renamed_from_its_page() {
    let mut app = app();
    running(
        &mut app,
        status(
            "connected",
            vec![
                root("a", "A", "ready", 1),
                root("work-docs", "Work docs", "ready", 3),
            ],
        ),
    );
    app.update(char('r'));
    assert_eq!(app.modal, None, "r looks for the daemon in the chats");
    app.update(char(','));
    app.update(key(KeyCode::Down));
    app.update(char('r'));
    assert_eq!(
        app.modal,
        Some(Modal::RenameRoot {
            id: "work-docs".into(),
            input: "Work docs".into()
        })
    );
    app.update(ctrl('u'));
    type_text(&mut app, "Team docs");
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![Effect::Daemon(DaemonCommand::LabelRoot {
            id: "work-docs".into(),
            label: "Team docs".into()
        })]
    );
    // An unchanged or empty name sends nothing.
    app.update(char('r'));
    assert!(app.update(key(KeyCode::Enter)).is_empty());
    app.update(char('r'));
    app.update(ctrl('u'));
    assert!(app.update(key(KeyCode::Enter)).is_empty());
}

#[test]
fn one_status_word_and_only_when_something_needs_you() {
    let mut app = app();
    budget_open(&mut app, every_action());
    let screen = render_to_string(&app, 100, 30);
    for gone in [
        "LIVE",
        "connected",
        "ACTIVITY",
        "enter send",
        "tab focus",
        "AUDIT LOG",
    ] {
        assert!(!screen.contains(gone), "{gone}: {screen}");
    }
    let mut s = status(
        "connected",
        vec![root("work-docs", "Work docs", "ready", 1532)],
    );
    s.paused = true;
    running(&mut app, s);
    let screen = render_to_string(&app, 100, 30);
    assert_eq!(screen.matches("paused").count(), 1, "{screen}");
    // It opens the settings where pausing is.
    let (x, y) = find(&app, "paused");
    click(&mut app, x, y);
    assert_eq!(app.view, View::Settings(Page::General));
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("p resume sharing"), "{screen}");
    for (connection, word) in [
        ("offline", "offline"),
        ("revoked", "revoked"),
        ("not_paired", "not paired"),
        ("connecting", "connecting"),
    ] {
        running(&mut app, status(connection, vec![]));
        let screen = render_to_string(&app, 100, 30);
        assert!(screen.lines().next().unwrap().contains(word), "{screen}");
    }
}

#[test]
fn the_account_page_disconnects_after_asking() {
    let mut app = app();
    chats_ready(&mut app);
    app.update(char(','));
    app.update(char('3'));
    app.update(char('d'));
    assert_eq!(app.modal, Some(Modal::ConfirmLogout));
    assert_eq!(
        app.update(char('y')),
        vec![Effect::Daemon(DaemonCommand::Logout)]
    );
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
    app.update(char(','));
    app.update(char('3'));
    assert_eq!(app.view, View::Settings(Page::Account));
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("proxy.corp:3128"), "{screen}");
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
    app.update(char(','));
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
    for page in Page::ALL {
        a.view = View::Settings(page);
        apps.push(a.clone());
    }
    a.view = View::Settings(Page::Folders);
    a.update(char('d'));
    apps.push(a.clone());
    a.modal = Some(Modal::Help { scroll: 0 });
    apps.push(a.clone());
    a.modal = None;
    a.view = View::Chat;
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
    let ctrl_c = || Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.update(ctrl_c()).is_empty(), "once only warns");
    assert!(!app.quit);
    assert!(app.notice.as_ref().unwrap().text.contains("again to quit"));
    // Another key in between starts over.
    app.update(key(KeyCode::Down));
    assert!(app.update(ctrl_c()).is_empty());
    assert_eq!(app.update(ctrl_c()), vec![Effect::Quit]);
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
    app.update(char(','));
    assert_eq!(app.view, View::Settings(Page::Folders));
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
    app.update(char(','));
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
    assert_eq!(app.view, View::Settings(Page::Activity));
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
    assert_eq!(app.focus, Focus::Chats, "nothing else to focus");
    app.update(char('i'));
    assert_eq!(app.focus, Focus::Chats);
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
    assert_eq!(app.next_wakeup(NOW), None, "the folders aren't on screen");
    app.update(char(','));
    assert_eq!(app.next_wakeup(NOW), Some(FRAME), "their spinner is");
    running(
        &mut app,
        status("connected", vec![root("a", "A", "ready", 1)]),
    );
    assert_eq!(app.next_wakeup(NOW), None);
    running(&mut app, status("connecting", vec![]));
    assert_eq!(app.next_wakeup(NOW), Some(FRAME), "connecting spins");
    running(&mut app, status("connected", vec![]));

    // Activity coming in changes nothing that counts time.
    let recent = event("2026-09-25T08:19:58.250Z", "connected", None);
    app.update(Msg::Daemon(DaemonMsg::Audit(Box::new(recent))));
    assert_eq!(app.next_wakeup(NOW), None);
    app.update(key(KeyCode::Esc));
    assert_eq!(app.next_wakeup(NOW), None);
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
        model: None,
        share: None,
        can: None,
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
        approvals: vec![],
        questions: vec![],
        entries: vec![
            Entry::User {
                id: 1,
                content: "What did we budget for Q3, and who signed it off?".into(),
                author: None,
                attachments: vec![],
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
                        app: None,
                        waiting: false,
                    },
                    Step {
                        summary: "Read plans/q3.md".into(),
                        pending: false,
                        files: vec![],
                        app: None,
                        waiting: false,
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
        attachments: vec![],
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
            app: None,
            waiting: false,
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
    assert_eq!(app.focus, Focus::Chats, "nothing to type into");

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
fn a_chat_refused_while_chats_still_work_is_not_followed_in_a_loop() {
    let mut app = app();
    open_budget(&mut app);
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(budget_transcript("idle")),
    }));
    assert_eq!(
        app.update(Msg::Chat(ChatMsg::Live {
            chat: 42,
            live: Live::Refused
        })),
        vec![Effect::Chat(ChatCommand::List)]
    );
    assert!(app.chat.ready(), "still usable while the list says why");
    // The list works: the refusal was about this chat. Following it again
    // would only be refused again.
    assert!(
        app.update(Msg::Chat(ChatMsg::Listed(Ok(chat_list()))))
            .is_empty()
    );
    assert_eq!(app.chat.following, Following::Refused);
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("not live"), "{screen}");

    // r tries again, once.
    app.update(key(KeyCode::Esc));
    let effects = app.update(char('r'));
    assert!(effects.contains(&Effect::Chat(ChatCommand::Follow(Some(42)))));
    assert_eq!(app.chat.following, Following::Starting);
}

#[test]
fn a_connection_that_cant_follow_chats_asks_nothing_more() {
    let mut app = app();
    open_budget(&mut app);
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(budget_transcript("idle")),
    }));
    assert!(
        app.update(Msg::Chat(ChatMsg::Live {
            chat: 42,
            live: Live::Unsupported
        }))
        .is_empty()
    );
    assert_eq!(app.chat.following, Following::Unsupported);
    assert!(app.chat.ready());
    assert!(
        app.update(Msg::Chat(ChatMsg::Listed(Ok(chat_list()))))
            .is_empty()
    );
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("not live"), "{screen}");
}

#[test]
fn a_reconnect_follows_the_open_chat_once() {
    let mut app = app();
    open_budget(&mut app);
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(budget_transcript("idle")),
    }));
    app.update(Msg::Chat(ChatMsg::Listed(Err(Failure::new(
        "daemon_stopped",
        "Gone.",
    )))));
    let effects = app.update(Msg::Daemon(DaemonMsg::Status(Box::new(status(
        "connected",
        vec![],
    )))));
    assert_eq!(
        effects
            .iter()
            .filter(|e| **e == Effect::Chat(ChatCommand::Follow(Some(42))))
            .count(),
        1
    );
    assert!(
        !app.update(Msg::Chat(ChatMsg::Listed(Ok(chat_list()))))
            .contains(&Effect::Chat(ChatCommand::Follow(Some(42)))),
        "already following"
    );
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
    assert_eq!(app.focus_order(), vec![Focus::Chats, Focus::Composer]);

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
        vec![
            Effect::Remember("q3 budget?".into()),
            Effect::Chat(ChatCommand::Send {
                chat: None,
                text: "q3 budget?".into(),
                model: None,
                project: None,
                attachments: vec![],
            })
        ]
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
            attachments: vec![],
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

// ------------------------------------------- composer, commands, prompts

fn ctrl(c: char) -> Msg {
    Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.update(char(c));
    }
}

/// Draw the app, so the mouse knows what's where.
fn draw(app: &mut App) {
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let mut hits = Hits::default();
    terminal
        .draw(|frame| {
            hits = super::ui::render_hits(frame, app, &Theme::new(Depth::TrueColor), NOW);
        })
        .unwrap();
    app.hits = hits;
}

fn mouse(app: &mut App, kind: crossterm::event::MouseEventKind, x: u16, y: u16) -> Vec<Effect> {
    draw(app);
    app.update(Msg::Mouse(crossterm::event::MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }))
}

fn click(app: &mut App, x: u16, y: u16) -> Vec<Effect> {
    use crossterm::event::{MouseButton, MouseEventKind};
    mouse(app, MouseEventKind::Down(MouseButton::Left), x, y)
}

/// Where `text` is on screen, as (column, row).
fn find(app: &App, text: &str) -> (u16, u16) {
    let screen = render_to_string(app, 100, 30);
    for (y, line) in screen.lines().enumerate() {
        if let Some(byte) = line.find(text) {
            return (line[..byte].chars().count() as u16, y as u16);
        }
    }
    panic!("{text:?} isn't on screen:\n{screen}");
}

fn every_action() -> Option<super::chat::Can> {
    Some(super::chat::Can {
        retry: true,
        branch: true,
        rename: true,
        delete: true,
        share: true,
    })
}

/// The budget chat, open, read, with what the server allows.
fn budget_open(app: &mut App, can: Option<super::chat::Can>) {
    open_budget(app);
    let mut transcript = budget_transcript("idle");
    transcript.chat.can = can;
    transcript.chat.model = Some(super::chat::ModelRef {
        id: "12".into(),
        name: "Gemini 3.8 Flash".into(),
    });
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(transcript),
    }));
}

fn models() -> super::chat::Models {
    use super::chat::{Model, Models};
    Models {
        default_model_id: Some("12".into()),
        models: vec![
            Model {
                id: "12".into(),
                name: "Gemini 3.8 Flash".into(),
                provider: "vertexai".into(),
                rate: Some("About 3 credits per answer".into()),
                ..Model::default()
            },
            Model {
                id: "15".into(),
                name: "GPT-6 Sol".into(),
                provider: "azure".into(),
                rate: Some("About 12 credits per answer".into()),
                ..Model::default()
            },
            Model {
                id: "16".into(),
                name: "Claude Opus".into(),
                provider: "anthropic".into(),
                selectable: false,
                reason: Some(
                    "You're out of credits. Add credits in Settings to keep going.".into(),
                ),
                ..Model::default()
            },
        ],
    }
}

#[test]
fn snapshot_chat_slash_menu() {
    let mut app = app();
    budget_open(&mut app, every_action());
    type_text(&mut app, "/re");
    let names: Vec<&str> = app.menu().iter().map(|c| c.name).collect();
    assert_eq!(names, ["resume", "retry", "rename"], "what fits here");
    assert_snapshot("chat_slash_menu", &app);

    // Down, then Tab completes; Enter on one that needs a title leaves room.
    app.update(key(KeyCode::Down));
    app.update(key(KeyCode::Tab));
    assert_eq!(app.chat.input, "/retry");
    app.chat.input.clear();
    type_text(&mut app, "/sea");
    assert!(app.update(key(KeyCode::Enter)).is_empty());
    assert_eq!(app.chat.input, "/search ");
    assert!(app.menu().is_empty(), "closed once there's an argument");

    // Esc closes the list; typing opens it again.
    app.chat.input.clear();
    type_text(&mut app, "/");
    assert!(!app.menu().is_empty());
    app.update(key(KeyCode::Esc));
    assert!(app.menu().is_empty());
    assert_eq!(app.focus, Focus::Composer, "esc only closed the list");
    type_text(&mut app, "n");
    assert_eq!(app.menu()[0].name, "new");
}

#[test]
fn commands_run_from_the_composer() {
    let mut app = app();
    budget_open(&mut app, None);
    // Not offered on an older server, and says so when typed.
    type_text(&mut app, "/re");
    let names: Vec<&str> = app.menu().iter().map(|c| c.name).collect();
    assert_eq!(names, ["resume"]);
    app.chat.input.clear();
    type_text(&mut app, "/retry");
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![Effect::Remember("/retry".into())]
    );
    assert!(
        app.notice
            .as_ref()
            .unwrap()
            .text
            .contains("can't retry chats from here yet")
    );

    type_text(&mut app, "/frobnicate");
    app.update(key(KeyCode::Enter));
    assert!(app.notice.as_ref().unwrap().text.contains("no /frobnicate"));
    assert_eq!(app.chat.input, "/frobnicate", "kept to fix");
    app.chat.input.clear();

    // `//` asks something that starts with a slash.
    type_text(&mut app, "//etc/hosts?");
    let effects = app.update(key(KeyCode::Enter));
    assert!(effects.contains(&Effect::Chat(ChatCommand::Send {
        chat: Some(42),
        text: "/etc/hosts?".into(),
        model: None,
        project: None,
        attachments: vec![],
    })));

    let mut app = self::app();
    chats_ready(&mut app);
    app.update(char('i'));
    type_text(&mut app, "/search pla");
    let effects = app.update(key(KeyCode::Enter));
    assert_eq!(effects, vec![Effect::Remember("/search pla".into())]);
    assert_eq!(app.focus, Focus::Chats);
    assert_eq!(app.chat.visible().len(), 2);

    // /new, /log, /folders, /help and /exit.
    app.update(key(KeyCode::Esc));
    app.update(char('i'));
    type_text(&mut app, "/log");
    app.update(key(KeyCode::Enter));
    assert_eq!(app.view, View::Settings(Page::Activity));
    app.update(key(KeyCode::Esc));
    assert_eq!(app.view, View::Chat);
    app.focus = Focus::Composer;
    type_text(&mut app, "/folders");
    app.update(key(KeyCode::Enter));
    assert_eq!(app.view, View::Settings(Page::Folders));
    app.update(key(KeyCode::Esc));
    app.focus = Focus::Composer;
    type_text(&mut app, "/help");
    app.update(key(KeyCode::Enter));
    assert_eq!(app.modal, Some(Modal::Help { scroll: 0 }));
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("select a chat"), "{screen}");
    let mut end = app.clone();
    end.modal = Some(Modal::Help { scroll: 500 });
    let screen = render_to_string(&end, 100, 30);
    assert!(screen.contains("/exit"), "{screen}");
    app.update(key(KeyCode::Down));
    assert_eq!(app.modal, Some(Modal::Help { scroll: 1 }), "it scrolls");
    app.update(char('x'));
    assert_eq!(app.modal, None);
    type_text(&mut app, "/status");
    app.update(key(KeyCode::Enter));
    assert_eq!(
        app.notice.as_ref().unwrap().text,
        "Connected to chatwithwork.com as device 42 · 1 folder shared · answering · cww 0.1.0"
    );
    type_text(&mut app, "/quit");
    assert!(app.update(key(KeyCode::Enter)).contains(&Effect::Quit));
}

#[test]
fn folder_and_pairing_commands_confirm_first() {
    let mut app = app();
    chats_ready(&mut app);
    app.update(char('i'));
    type_text(&mut app, "/unshare work docs");
    app.update(key(KeyCode::Enter));
    assert!(matches!(app.modal, Some(Modal::ConfirmRemove { ref id, .. }) if id == "work-docs"));
    app.update(char('n'));
    app.focus = Focus::Composer;
    type_text(&mut app, "/share ~/Projects");
    assert!(
        app.update(key(KeyCode::Enter))
            .contains(&Effect::Daemon(DaemonCommand::AddRoot {
                path: "~/Projects".into(),
                label: None,
                i_know: false,
            }))
    );
    type_text(&mut app, "/pause");
    assert!(
        app.update(key(KeyCode::Enter))
            .contains(&Effect::Daemon(DaemonCommand::Pause))
    );
    type_text(&mut app, "/logout");
    app.update(key(KeyCode::Enter));
    assert_eq!(app.modal, Some(Modal::ConfirmLogout));
    assert_eq!(
        app.update(char('y')),
        vec![Effect::Daemon(DaemonCommand::Logout)]
    );
}

#[test]
fn up_recalls_questions_and_text_takes_several_lines() {
    let mut app = app().with_history(vec!["first".into(), "second".into()]);
    chats_ready(&mut app);
    app.update(char('n'));
    type_text(&mut app, "draft");
    app.update(key(KeyCode::Up));
    assert_eq!(app.chat.input, "second");
    app.update(key(KeyCode::Up));
    assert_eq!(app.chat.input, "first");
    app.update(key(KeyCode::Down));
    app.update(key(KeyCode::Down));
    assert_eq!(app.chat.input, "draft", "back to what was typed");

    // Shift-Enter, Alt-Enter, Ctrl-J and a trailing backslash break lines.
    app.chat.input.clear();
    type_text(&mut app, "one");
    app.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)));
    type_text(&mut app, "two");
    app.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)));
    type_text(&mut app, "three");
    app.update(ctrl('j'));
    type_text(&mut app, "four\\");
    app.update(key(KeyCode::Enter));
    type_text(&mut app, "five");
    assert_eq!(app.chat.input, "one\ntwo\nthree\nfour\nfive");
    // Up moves between the lines before it reaches the history.
    app.update(key(KeyCode::Up));
    assert_eq!(app.chat.input, "one\ntwo\nthree\nfour\nfive");
    assert_snapshot("chat_multiline", &app);

    // Pasted lines stay lines.
    app.chat.input.clear();
    app.update(Msg::Paste("a\r\nb".into()));
    assert_eq!(app.chat.input, "a\nb");

    // Ctrl-C clears what's typed rather than quitting.
    assert!(app.update(ctrl('c')).is_empty());
    assert!(app.chat.input.is_empty());
    assert!(!app.quit_armed);
    assert_eq!(app.update(ctrl('l')), vec![Effect::Redraw]);

    // What's sent is remembered, here and in the file.
    type_text(&mut app, "third");
    let effects = app.update(key(KeyCode::Enter));
    assert_eq!(effects[0], Effect::Remember("third".into()));
    assert_eq!(app.chat.history.entries().last().unwrap(), "third");
}

#[test]
fn snapshot_chat_model_picker() {
    let mut app = app();
    budget_open(&mut app, every_action());
    type_text(&mut app, "/model");
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![
            Effect::Remember("/model".into()),
            Effect::Chat(ChatCommand::Models)
        ]
    );
    assert_eq!(app.chat.models, ModelList::Loading);
    app.update(Msg::Chat(ChatMsg::Models(Ok(models()))));
    assert_snapshot("chat_model_picker", &app);

    // One that can't be used says why and stays open.
    type_text(&mut app, "opus");
    app.update(key(KeyCode::Enter));
    assert!(app.notice.as_ref().unwrap().text.contains("out of credits"));
    assert!(app.chat.picker.is_some());
    for _ in 0..4 {
        app.update(key(KeyCode::Backspace));
    }
    app.update(key(KeyCode::Down));
    app.update(key(KeyCode::Enter));
    assert_eq!(app.chat.picker, None);
    assert_eq!(app.chat.model_name(), Some("GPT-6 Sol"));
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains(" GPT-6 Sol "), "{screen}");

    // The next question asks with it; a refusal goes back to the chat's own.
    type_text(&mut app, "And Q4?");
    let effects = app.update(key(KeyCode::Enter));
    assert!(effects.contains(&Effect::Chat(ChatCommand::Send {
        chat: Some(42),
        text: "And Q4?".into(),
        model: Some("15".into()),
        project: None,
        attachments: vec![],
    })));
    app.update(Msg::Chat(ChatMsg::Sent {
        chat: Some(42),
        result: Err(Failure::new(
            "model_unavailable",
            "That model isn't available. Pick another one.",
        )),
    }));
    assert_eq!(app.chat.model, None);
    assert_eq!(app.chat.input, "And Q4?");

    // `/model gemini` picks it at once.
    app.chat.input.clear();
    type_text(&mut app, "/model gemini");
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![Effect::Remember("/model gemini".into())]
    );
    assert_eq!(app.chat.model_name(), Some("Gemini 3.8 Flash"));
}

#[test]
fn an_older_server_has_no_model_to_pick() {
    let mut app = app();
    budget_open(&mut app, None);
    type_text(&mut app, "/model");
    app.update(key(KeyCode::Enter));
    app.update(Msg::Chat(ChatMsg::Models(Err(Failure::new(
        "unsupported",
        "This Chat with Work server doesn't let you pick a model from here yet. Chats use your default model.",
    )))));
    assert_eq!(app.chat.picker, None);
    assert!(matches!(app.chat.models, ModelList::Unavailable(_)));
    assert!(
        app.notice
            .as_ref()
            .unwrap()
            .text
            .contains("doesn't let you pick")
    );
    // Asked again: the sentence, and nothing sent.
    type_text(&mut app, "/model");
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        Vec::<Effect>::new(),
        "already remembered"
    );
    assert!(
        app.notice
            .as_ref()
            .unwrap()
            .text
            .contains("doesn't let you pick")
    );
}

#[test]
fn projects_and_chats_are_picked_from_lists() {
    let mut app = app();
    budget_open(&mut app, every_action());
    type_text(&mut app, "/project falcon");
    let effects = app.update(key(KeyCode::Enter));
    assert!(
        effects.contains(&Effect::Chat(ChatCommand::Follow(None))),
        "a new chat"
    );
    assert_eq!(app.chat.open, None);
    let screen = render_to_string(&app, 100, 30);
    assert!(
        screen.contains("New chat") && screen.contains("Falcon"),
        "{screen}"
    );
    type_text(&mut app, "Plan?");
    let effects = app.update(key(KeyCode::Enter));
    assert!(effects.contains(&Effect::Chat(ChatCommand::Send {
        chat: None,
        text: "Plan?".into(),
        model: None,
        project: Some(3),
        attachments: vec![],
    })));

    let mut app = self::app();
    budget_open(&mut app, every_action());
    type_text(&mut app, "/resume");
    app.update(key(KeyCode::Enter));
    type_text(&mut app, "hiring");
    assert_snapshot("chat_resume_picker", &app);
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![
            Effect::Chat(ChatCommand::Open(27)),
            Effect::Chat(ChatCommand::Follow(Some(27)))
        ]
    );
}

fn approval() -> super::chat::Approval {
    super::chat::Approval {
        id: 31,
        service: "Slack".into(),
        effect: "write".into(),
        decidable: true,
        summary: "Post a message to #general".into(),
        details: vec![
            super::chat::Detail {
                label: "Channel".into(),
                value: "#general".into(),
            },
            super::chat::Detail {
                label: "Message".into(),
                value: "We shipped!".into(),
            },
        ],
        allow_for_rest_of_chat: true,
        waiting_for: None,
    }
}

fn waiting(
    app: &mut App,
    approvals: Vec<super::chat::Approval>,
    questions: Vec<super::chat::Question>,
) {
    open_budget(app);
    let mut transcript = budget_transcript("idle");
    transcript.approvals = approvals;
    transcript.questions = questions;
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(transcript),
    }));
}

#[test]
fn snapshot_chat_tool_approval() {
    let mut app = app();
    waiting(&mut app, vec![approval()], vec![]);
    assert_snapshot("chat_tool_approval", &app);

    // Typing doesn't land in the composer, but a command does.
    app.update(char('x'));
    assert!(app.chat.input.is_empty());
    type_text(&mut app, "/op");
    assert_eq!(app.menu()[0].name, "open");
    app.update(ctrl('c'));
    app.update(key(KeyCode::Down));
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![Effect::Chat(ChatCommand::Approve {
            chat: 42,
            tool_call: 31,
            for_rest_of_chat: true
        })]
    );
    assert!(app.update(char('y')).is_empty(), "one answer at a time");
    let mut busy = summary(42, "Q3 budget", "2026-09-25T08:14:03Z");
    busy.state = "processing".into();
    assert_eq!(
        app.update(Msg::Chat(ChatMsg::Decided {
            chat: 42,
            result: Ok(busy)
        })),
        vec![Effect::Chat(ChatCommand::Open(42))]
    );
    assert!(!app.chat.deciding);

    // Denied with what to do instead.
    let mut app = self::app();
    waiting(&mut app, vec![approval()], vec![]);
    app.update(char('4'));
    assert!(app.chat.reason);
    type_text(&mut app, "Post in #launch");
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![Effect::Chat(ChatCommand::Deny {
            chat: 42,
            tool_call: 31,
            reason: Some("Post in #launch".into())
        })]
    );
    let mut app = self::app();
    waiting(&mut app, vec![approval()], vec![]);
    assert_eq!(
        app.update(char('n')),
        vec![Effect::Chat(ChatCommand::Deny {
            chat: 42,
            tool_call: 31,
            reason: None
        })]
    );

    // Someone else's to decide: only whose it is.
    let mut app = self::app();
    let mut theirs = approval();
    theirs.decidable = false;
    theirs.waiting_for = Some("Ada".into());
    waiting(&mut app, vec![theirs], vec![]);
    let screen = render_to_string(&app, 100, 30);
    assert!(
        screen.contains("Waiting for Ada to approve a change in Slack"),
        "{screen}"
    );
    assert_eq!(app.chat.pending_decision(), None);
}

fn question() -> super::chat::Question {
    use super::chat::{Field, Question};
    Question {
        id: 32,
        service: "Notion".into(),
        decidable: true,
        message: "Which environment should the report cover?".into(),
        kind: "form".into(),
        fields: vec![
            Field {
                name: "environment".into(),
                title: "Environment".into(),
                kind: "string".into(),
                required: true,
                choices: Some(vec!["staging".into(), "production".into()]),
                ..Field::default()
            },
            Field {
                name: "days".into(),
                title: "Days".into(),
                kind: "integer".into(),
                ..Field::default()
            },
            Field {
                name: "include_drafts".into(),
                title: "Include drafts".into(),
                kind: "boolean".into(),
                default: Some(serde_json::json!(false)),
                ..Field::default()
            },
        ],
        note: Some(
            "Only Notion sees your answer. Never enter a password here; sign in on the \
             service's own page instead."
                .into(),
        ),
        ..Question::default()
    }
}

#[test]
fn snapshot_chat_tool_question() {
    let mut app = app();
    waiting(&mut app, vec![], vec![question()]);
    assert_eq!(app.chat.form.values, ["", "", "no"], "the defaults");
    app.update(key(KeyCode::Right));
    app.update(key(KeyCode::Right));
    assert_eq!(app.chat.form.values[0], "production");
    app.update(key(KeyCode::Down));
    app.update(key(KeyCode::Enter));
    assert_eq!(app.chat.form.editing, Some(1));
    type_text(&mut app, "30");
    assert_snapshot("chat_tool_question", &app);
    app.update(key(KeyCode::Enter));
    assert_eq!(app.chat.form.values[1], "30");
    assert_eq!(app.chat.form.row, 2, "on to the next");
    app.update(char(' '));
    app.update(key(KeyCode::Down));
    let effects = app.update(key(KeyCode::Enter));
    assert_eq!(
        effects,
        vec![Effect::Chat(ChatCommand::Answer {
            chat: 42,
            tool_call: 32,
            input: Some(serde_json::json!({
                "environment": "production",
                "days": "30",
                "include_drafts": true
            }))
        })]
    );

    // Declined.
    let mut app = self::app();
    waiting(&mut app, vec![], vec![question()]);
    for _ in 0..4 {
        app.update(key(KeyCode::Down));
    }
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![Effect::Chat(ChatCommand::Decline {
            chat: 42,
            tool_call: 32
        })]
    );

    // A page to open, then say it's done.
    let mut app = self::app();
    let mut page = question();
    page.kind = "url".into();
    page.fields.clear();
    page.url = Some("https://notion.so/connect".into());
    page.host = Some("notion.so".into());
    waiting(&mut app, vec![], vec![page]);
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("Open notion.so in the browser"), "{screen}");
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![Effect::Chat(ChatCommand::OpenUrl(
            "https://notion.so/connect".into()
        ))]
    );
    app.update(key(KeyCode::Down));
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![Effect::Chat(ChatCommand::Answer {
            chat: 42,
            tool_call: 32,
            input: None
        })]
    );
}

#[test]
fn files_go_with_the_next_question() {
    let mut app = app();
    budget_open(&mut app, every_action());
    type_text(&mut app, "/attach ~/notes.txt");
    assert_eq!(
        app.update(key(KeyCode::Enter)),
        vec![
            Effect::Remember("/attach ~/notes.txt".into()),
            Effect::Chat(ChatCommand::Upload("~/notes.txt".into()))
        ]
    );
    type_text(&mut app, "Summarize");
    let effects = app.update(key(KeyCode::Enter));
    assert!(effects.is_empty(), "still uploading");
    assert!(
        app.notice
            .as_ref()
            .unwrap()
            .text
            .contains("still uploading")
    );
    app.update(Msg::Chat(ChatMsg::Uploaded {
        path: "~/notes.txt".into(),
        result: Ok(super::chat::Uploaded {
            signed_id: "signed-1".into(),
            filename: "notes.txt".into(),
            byte_size: 2048,
            content_type: "text/plain".into(),
        }),
    }));
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("+ notes.txt · 2.0 KB"), "{screen}");
    let effects = app.update(key(KeyCode::Enter));
    assert!(effects.contains(&Effect::Chat(ChatCommand::Send {
        chat: Some(42),
        text: "Summarize".into(),
        model: None,
        project: None,
        attachments: vec!["signed-1".into()],
    })));
    assert!(app.chat.attachments.is_empty());

    // A refused file says why and is gone.
    type_text(&mut app, "/attach logo.svg");
    app.update(key(KeyCode::Enter));
    app.update(Msg::Chat(ChatMsg::Uploaded {
        path: "logo.svg".into(),
        result: Err(Failure::new(
            "attachment_refused",
            "logo.svg has a blocked file type",
        )),
    }));
    assert!(app.chat.attachments.is_empty());
    assert_eq!(
        app.notice.as_ref().unwrap().text,
        "logo.svg has a blocked file type"
    );
}

#[test]
fn chat_actions_ask_the_server() {
    let mut app = app();
    budget_open(&mut app, every_action());
    let run = |app: &mut App, line: &str| {
        app.focus = Focus::Composer;
        app.chat.input.clear();
        type_text(app, line);
        app.update(key(KeyCode::Enter))
            .into_iter()
            .filter(|e| !matches!(e, Effect::Remember(_)))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        run(&mut app, "/retry"),
        vec![Effect::Chat(ChatCommand::Act {
            chat: 42,
            action: ChatAction::Retry
        })]
    );
    assert!(run(&mut app, "/rename").is_empty());
    assert_eq!(app.chat.input, "/rename Q3 budget", "the title to edit");
    assert_eq!(
        run(&mut app, "/rename Q3 budget, final"),
        vec![Effect::Chat(ChatCommand::Act {
            chat: 42,
            action: ChatAction::Rename("Q3 budget, final".into())
        })]
    );
    assert!(run(&mut app, "/delete").is_empty());
    assert!(matches!(
        app.modal,
        Some(Modal::ConfirmDelete { chat: 42, .. })
    ));
    assert_eq!(
        app.update(char('y')),
        vec![Effect::Chat(ChatCommand::Act {
            chat: 42,
            action: ChatAction::Delete
        })]
    );
    let effects = app.update(Msg::Chat(ChatMsg::Acted {
        chat: 42,
        action: ChatAction::Delete,
        result: Ok(Acted::Deleted),
    }));
    assert!(effects.contains(&Effect::Chat(ChatCommand::Follow(None))));
    assert_eq!(app.chat.open, None);
    assert!(app.chat.list.chats.iter().all(|c| c.number != 42));

    let mut app = self::app();
    budget_open(&mut app, every_action());
    run(&mut app, "/share-chat");
    let mut shared_chat = summary(42, "Q3 budget", "2026-09-25T08:14:03Z");
    shared_chat.share = Some(super::chat::ShareLink {
        url: "https://chatwithwork.com/shared/abc".into(),
        expires_at: None,
    });
    let effects = app.update(Msg::Chat(ChatMsg::Acted {
        chat: 42,
        action: ChatAction::Share,
        result: Ok(Acted::Shared(super::chat::Shared {
            url: "https://chatwithwork.com/shared/abc".into(),
            expires_at: None,
            chat: shared_chat,
        })),
    }));
    assert_eq!(
        effects,
        vec![Effect::Copy("https://chatwithwork.com/shared/abc".into())]
    );
    assert_eq!(
        run(&mut app, "/unshare-chat"),
        vec![Effect::Chat(ChatCommand::Act {
            chat: 42,
            action: ChatAction::Unshare
        })]
    );
    assert_eq!(
        run(&mut app, "/copy"),
        vec![Effect::Copy(
            "The Q3 budget is **€40k**, signed off by *Ada* on 12 September.\n\n\
             - Marketing: €18k\n- Engineering: `€22k`\n\n\
             Details are in [the plan](https://drive.google.com/file/d/q3)."
                .into()
        )]
    );
    let branch = summary(44, "Branch of Q3 budget", "2026-09-25T08:20:00Z");
    let effects = app.update(Msg::Chat(ChatMsg::Acted {
        chat: 42,
        action: ChatAction::Branch,
        result: Ok(Acted::Chat(branch)),
    }));
    assert!(effects.contains(&Effect::Chat(ChatCommand::Open(44))));
    assert_eq!(app.chat.open, Some(44));
}

#[test]
fn the_mouse_selects_opens_and_scrolls() {
    use crossterm::event::MouseEventKind;
    let mut app = app();
    chats_ready(&mut app);
    // A chat in the sidebar.
    let (x, y) = find(&app, "Hiring plan");
    assert_eq!(
        click(&mut app, x, y),
        vec![
            Effect::Chat(ChatCommand::Open(27)),
            Effect::Chat(ChatCommand::Follow(Some(27)))
        ]
    );
    // Your name opens the settings; a page, a folder, and back.
    let (x, y) = find(&app, "Carmine");
    click(&mut app, x, y);
    assert_eq!(app.view, View::Settings(Page::Folders));
    let (x, y) = find(&app, "Activity");
    click(&mut app, x, y);
    assert_eq!(app.view, View::Settings(Page::Activity));
    let (x, y) = find(&app, "Shared folders");
    click(&mut app, x, y);
    let (x, y) = find(&app, "Work docs");
    click(&mut app, x, y);
    assert_eq!(app.selected_root, 0);
    let (x, y) = find(&app, "Chats");
    click(&mut app, x, y);
    assert_eq!(app.view, View::Chat);
    // The composer, then a row of the slash list.
    let (x, y) = find(&app, "Reply to Chat with Work");
    click(&mut app, x, y);
    assert_eq!(app.focus, Focus::Composer);
    type_text(&mut app, "/lo");
    let (x, y) = find(&app, "/log");
    click(&mut app, x, y);
    assert_eq!(app.view, View::Settings(Page::Activity));

    // The wheel scrolls the conversation, and links open.
    let mut app = self::app();
    budget_open(&mut app, every_action());
    let (x, y) = find(&app, "Marketing");
    mouse(&mut app, MouseEventKind::ScrollUp, x, y);
    assert_eq!(app.chat.scroll, 3);
    mouse(&mut app, MouseEventKind::ScrollDown, x, y);
    assert_eq!(app.chat.scroll, 0);
    let (x, y) = find(&app, "https://drive.google.com/file/d/q3");
    assert_eq!(
        click(&mut app, x, y),
        vec![Effect::Chat(ChatCommand::OpenUrl(
            "https://drive.google.com/file/d/q3".into()
        ))]
    );

    // The approval prompt's answers.
    let mut app = self::app();
    waiting(&mut app, vec![approval()], vec![]);
    let (x, y) = find(&app, "3. Deny");
    assert_eq!(
        click(&mut app, x, y),
        vec![Effect::Chat(ChatCommand::Deny {
            chat: 42,
            tool_call: 31,
            reason: None
        })]
    );

    // A picker's rows.
    let mut app = self::app();
    budget_open(&mut app, every_action());
    type_text(&mut app, "/resume");
    app.update(key(KeyCode::Enter));
    let (x, y) = find(&app, "Vendor contracts");
    assert_eq!(
        click(&mut app, x, y),
        vec![
            Effect::Chat(ChatCommand::Open(31)),
            Effect::Chat(ChatCommand::Follow(Some(31)))
        ]
    );
}

#[test]
fn thinking_shimmers_in_grey_rather_than_the_rainbow() {
    use super::theme::{FAINT, MUTED};
    let mut app = app();
    open_budget(&mut app);
    app.update(Msg::Chat(ChatMsg::Shown {
        chat: 42,
        result: Ok(budget_transcript("processing")),
    }));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| render(frame, &app, &Theme::new(Depth::TrueColor), NOW))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let (x, y) = find(&app, "Thinking");
    let between = |v: u8, a: u8, b: u8| a.min(b) <= v && v <= a.max(b);
    for i in 0..8 {
        let cell = &buffer[(x + i, y)];
        let ratatui::style::Color::Rgb(r, g, b) = cell.fg else {
            panic!("no colour on {:?}", cell.symbol());
        };
        assert!(
            between(r, FAINT.0, MUTED.0)
                && between(g, FAINT.1, MUTED.1)
                && between(b, FAINT.2, MUTED.2),
            "{} is ({r}, {g}, {b})",
            cell.symbol()
        );
    }
    // Writing, once the answer streams in.
    app.update(Msg::Chat(ChatMsg::Live {
        chat: 42,
        live: Live::Chunk {
            message_id: 7,
            text: "It's".into(),
        },
    }));
    let screen = render_to_string(&app, 100, 30);
    assert!(screen.contains("Writing"), "{screen}");
}
