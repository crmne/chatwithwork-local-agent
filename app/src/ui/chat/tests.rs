//! The Chat page, driven through its accessibility tree against the
//! stand-in daemon, which records every request it gets and serves a
//! synthetic organization's chats. The snapshot test renders the page
//! offscreen with wgpu and keeps PNGs in `app/tests/snapshots`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::Value;

use crate::control::Client;
use crate::demo::{self, Scenario};
use crate::model::Platform;
use crate::ui::tests::OPENED;
use crate::ui::theme::Theme;
use crate::ui::{AppState, Page, SettingsApp, Shared};

thread_local! {
    /// What the next file picker "picks": tests never open a window.
    pub static PICKED: std::cell::RefCell<Option<Vec<std::path::PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

struct Fixture {
    server: demo::Server,
    harness: Harness<'static, SettingsApp>,
}

struct Options {
    scenario: Scenario,
    size: [f32; 2],
    dark: bool,
    held: bool,
    /// The daemon refuses to keep the list current, as one from before
    /// live lists does.
    old_daemon: bool,
    renderer: Option<egui_kittest::wgpu::WgpuTestRenderer>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scenario: Scenario::Sample,
            size: [1280.0, 820.0],
            dark: false,
            held: true,
            old_daemon: false,
            renderer: None,
        }
    }
}

fn fixture(options: Options) -> Fixture {
    let server = demo::start(options.scenario).unwrap();
    server.hold_streams(options.held);
    if options.old_daemon {
        server.act_old();
    }
    if options.scenario == Scenario::Sample {
        AppState { onboarded: true }.save(&server.paths);
    }
    let shared = Shared::new(
        Client::new(server.paths.socket_path()),
        server.paths.clone(),
        server.account(),
    );
    let watcher = Arc::clone(&shared);
    shared
        .client
        .spawn_watch(move |status| watcher.set_status(status));
    let mut app = SettingsApp::new(shared, Theme::new(Platform::current(), options.dark, None));
    app.set_page(Page::Chat);
    let mut builder = Harness::builder().with_size(options.size);
    if let Some(renderer) = options.renderer {
        builder = builder.renderer(renderer);
    }
    // The whole window, without the harness's own margin.
    let harness = builder.build_ui_state(
        |ui, app: &mut SettingsApp| {
            let screen = ui.ctx().content_rect();
            let mut window = ui.new_child(egui::UiBuilder::new().max_rect(screen));
            window.set_clip_rect(screen);
            app.show(&mut window);
        },
        app,
    );
    let mut fixture = Fixture { server, harness };
    fixture.wait("the first status", |h| h.state().shared().is_known());
    fixture
}

impl Fixture {
    fn wait(&mut self, what: &str, done: impl Fn(&Harness<'static, SettingsApp>) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            self.harness.step();
            if done(&self.harness) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_label(&mut self, label: &str) {
        let label = label.to_string();
        self.wait(&format!("“{label}”"), move |h| {
            h.query_all_by_label_contains(&label).next().is_some()
        });
    }

    fn wait_for_request(&mut self, cmd: &str, check: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(found) = self
                .server
                .requests()
                .into_iter()
                .find(|r| r["cmd"] == cmd && check(r))
            {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "no {cmd} request: {:?}",
                self.server.requests()
            );
            self.harness.step();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn chat(&mut self) -> &mut super::ChatPage {
        self.harness.state_mut().chat().expect("the chat page")
    }

    fn open(&mut self, number: u64) {
        self.harness.get_by_label(&self.title(number)).click();
        self.harness.run_steps(2);
    }

    fn title(&self, number: u64) -> String {
        match number {
            12 => "Q3 budget review",
            11 => "Vendor contract renewal",
            _ => "Weekly sync summary",
        }
        .into()
    }
}

#[test]
fn chats_are_listed_by_day_and_open_with_their_answers() {
    let mut fx = fixture(Options::default());
    fx.wait_for_label("Vendor contract renewal");
    // One Recent list, as the web's sidebar has.
    fx.harness.get_by_label("Recent");
    assert!(fx.harness.query_by_label("Yesterday").is_none());
    fx.chat().set_greeting(4);
    fx.harness.run_steps(1);
    fx.harness
        .get_by_label_contains("What are we working on, Alex?");
    fx.harness.get_by_label("Vendor contract renewal").click();
    fx.wait_for_request("chat", |r| r["chat"] == "11");
    fx.wait_for_request("subscribe", |r| r["chat"] == "11");
    fx.wait_for_label("When does the Acme contract renew");
    fx.harness
        .get_by_label_contains("The Acme contract renews on March 1, 2027");
    fx.harness
        .get_by_label_contains("Annual fee · €40,000 · €42,000");
    fx.harness.get_by_label("Source 1: Acme renewal 2026.pdf");
    // A Drive link of another shape still finds its source.
    fx.harness.get_by_label("Source 2: Acme MSA 2024.pdf");
    fx.harness.get_by_label_contains("\"auto_renew\": true");
    fx.harness.get_by_label_contains("Acme renews on March 1.");
    // The open chat is the selected one.
    let row = fx.harness.get_by_label("Vendor contract renewal");
    let node = row.accesskit_node();
    assert!(
        node.is_selected() == Some(true) || node.toggled() == Some(egui::accesskit::Toggled::True),
        "the open chat is selected"
    );
}

#[test]
fn the_activity_line_opens_into_its_steps() {
    let mut fx = fixture(Options::default());
    fx.wait_for_label("Vendor contract renewal");
    fx.open(11);
    fx.wait_for_label("Searched Drive and Slack · 3 searches, read 2 files");
    assert!(
        fx.harness
            .query_by_label("Searched Drive for “Acme contract”")
            .is_none()
    );
    // Above the fold: clicked as a screen reader would.
    fx.harness
        .get_by_label("Searched Drive and Slack · 3 searches, read 2 files")
        .click_accesskit();
    fx.wait_for_label("Searched Drive for “Acme contract”");
    fx.harness
        .get_by_label("Acme renewal 2026.pdf, Acme MSA 2024.pdf");
}

#[test]
fn sources_open_in_the_browser() {
    OPENED.with(|o| o.borrow_mut().clear());
    let mut fx = fixture(Options::default());
    fx.wait_for_label("Vendor contract renewal");
    fx.open(11);
    fx.wait_for_label("2 sources");
    // Let it scroll to the latest question first: this answer is above it,
    // so it's clicked as a screen reader would.
    fx.harness.run_steps(5);
    fx.harness.get_by_label("2 sources").click_accesskit();
    fx.wait_for_label("1. Acme renewal 2026.pdf");
    fx.harness
        .get_by_label("2. Acme MSA 2024.pdf")
        .click_accesskit();
    fx.harness.run_steps(2);
    // The chip is above the fold now: clicked as a screen reader would.
    fx.harness
        .get_by_label("Source 1: Acme renewal 2026.pdf")
        .click_accesskit();
    fx.harness.run_steps(2);
    let opened = OPENED.with(|o| o.borrow().clone());
    assert_eq!(
        opened,
        [
            "https://drive.google.com/file/d/1AcmeMsa2024Signed/view",
            "https://drive.google.com/file/d/1AcmeRenewal2026Final/view",
        ]
    );
}

#[test]
fn a_question_streams_its_answer_in() {
    let mut fx = fixture(Options {
        held: false,
        ..Options::default()
    });
    fx.wait_for_label("Vendor contract renewal");
    let field = fx
        .harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput);
    field.focus();
    field.type_text("Who owns the budget?");
    fx.harness.run_steps(2);
    fx.harness.get_by_label("Send message").click();
    let sent = fx.wait_for_request("chat_send", |_| true);
    assert_eq!(sent["text"], "Who owns the budget?");
    assert!(sent.get("chat").is_none(), "a new chat: {sent}");
    // The new chat follows its answer, which streams in and settles.
    fx.wait_for_request("subscribe", |r| r["chat"] == "13");
    fx.wait("the question in the chat", |h| {
        h.query_all_by_label("Who owns the budget?").count() >= 2
    });
    fx.wait_for_label("Stop response");
    fx.wait_for_label("Here's what I found in Drive and Slack");
    fx.wait_for_label("Want me to draft the next review's agenda?");
    fx.wait_for_label("Send message");
    fx.harness.get_by_label_contains("Searched Drive and Slack");
    assert!(!fx.chat().state().working());
}

#[test]
fn stop_cancels_the_answer_being_written() {
    let mut fx = fixture(Options::default());
    fx.wait_for_label("Q3 budget review");
    fx.open(12);
    fx.wait_for_label("Q3 spending came in at €1.28M");
    fx.harness
        .get_by_label("Searching Drive and Slack · Reading Q3 actuals.xlsx");
    fx.harness.get_by_label("Stop response").click();
    fx.wait_for_request("chat_cancel", |r| r["chat"] == "12");
    fx.wait_for_label("Send message");
}

#[test]
fn the_composer_says_why_it_is_locked_and_the_way_out() {
    let mut fx = fixture(Options {
        scenario: Scenario::Fresh,
        ..Options::default()
    });
    fx.wait_for_label("This computer isn't paired with Chat with Work.");
    let send = fx.harness.get_by_label("Send message");
    assert!(send.accesskit_node().is_disabled());
    fx.harness.get_by_label("Pair it").click();
    fx.harness.run_steps(2);
    assert_eq!(fx.harness.state().page(), Page::Account);
    assert!(
        !fx.server.requests().iter().any(|r| r["cmd"] == "chats"),
        "no chats asked for while unpaired"
    );
}

#[test]
fn a_tool_with_its_own_view_gets_a_place_for_it() {
    let mut fx = fixture(Options::default());
    // A project's chat isn't under Recent, as on the web: open it as its
    // pin or its link would.
    fx.wait_for_label("Vendor contract renewal");
    fx.chat().open(10);
    fx.harness.run_steps(2);
    fx.wait_for_label("Linear's view");
    fx.harness.get_by_label("Open in browser");
}

#[test]
fn a_settled_chat_asks_for_no_more_frames() {
    let mut fx = fixture(Options::default());
    fx.wait_for_label("Vendor contract renewal");
    // Chat 12's answer runs: its shimmer and dot keep the window drawing.
    assert!(fx.harness.try_run().is_err(), "a running chat animates");
    fx.server.finish(12, 123, "Done.");
    fx.wait("the list to show it settled", |h| {
        h.state()
            .chat_state()
            .is_some_and(|s| s.list.chats.iter().all(|c| !c.processing()))
    });
    fx.open(11);
    fx.wait_for_label("The Acme contract renews");
    // Hovers and rises settle, then nothing more is asked for.
    fx.harness.try_run().expect("a settled page stops drawing");
}

impl Fixture {
    /// How many times the page has read the list so far.
    fn lists(&self) -> usize {
        self.server
            .requests()
            .iter()
            .filter(|r| r["cmd"] == "chats")
            .count()
    }

    /// Wait until the server keeps the list current, and the catch-up
    /// read is in.
    fn live(&mut self) {
        self.wait_for_request("subscribe", |r| r["topics"] == serde_json::json!(["chats"]));
        self.wait("the list to be live", |h| {
            h.state().chat_state().is_some_and(|s| s.list_live)
        });
        self.harness.run_steps(4);
    }
}

#[test]
fn the_list_stays_current_as_chats_change_elsewhere() {
    let mut fx = fixture(Options::default());
    fx.wait_for_label("Vendor contract renewal");
    fx.live();
    let listed = fx.lists();
    // Started on the web: it shows up, being answered, then settles.
    fx.server.start_elsewhere(
        30,
        "Draft the Q4 hiring plan",
        "Draft the Q4 hiring plan from the headcount sheet.",
    );
    fx.wait_for_label("Draft the Q4 hiring plan");
    assert!(
        fx.harness
            .state()
            .chat_state()
            .is_some_and(|s| s.list.chats[0].number == 30 && s.list.chats[0].processing()),
        "newest first, being answered"
    );
    fx.server.finish(30, 302, "Here's a first draft.");
    fx.wait("chat 30 to settle", |h| {
        h.state().chat_state().is_some_and(|s| {
            s.list
                .chats
                .iter()
                .any(|c| c.number == 30 && !c.processing())
        })
    });
    // Renamed elsewhere: the row says so.
    fx.server
        .edit_elsewhere(30, |c| c["title"] = serde_json::json!("Q4 hiring plan"));
    fx.wait("the new title", |h| {
        h.query_by_label("Q4 hiring plan").is_some()
    });
    assert!(
        fx.harness
            .query_by_label("Draft the Q4 hiring plan")
            .is_none()
    );
    // The open chat deleted elsewhere closes, saying so.
    fx.open(11);
    fx.wait_for_label("When does the Acme contract renew");
    fx.server.delete_elsewhere(11);
    fx.wait_for_label(super::state::GONE);
    assert!(
        fx.harness
            .query_by_label("Vendor contract renewal")
            .is_none()
    );
    assert_eq!(fx.chat().state().open, None);
    // None of it needed the list read again.
    assert_eq!(fx.lists(), listed, "no reads while the list is live");
}

#[test]
fn an_older_daemon_still_gets_the_list_read_again() {
    let mut fx = fixture(Options {
        old_daemon: true,
        ..Options::default()
    });
    fx.wait_for_label("Vendor contract renewal");
    fx.wait_for_request("subscribe", |r| r["topics"] == serde_json::json!(["chats"]));
    let listed = fx.lists();
    // Chat 12 is answered elsewhere: the list is read again until it's done.
    fx.server.finish(12, 123, "Done.");
    fx.wait("the list to show it settled", |h| {
        h.state()
            .chat_state()
            .is_some_and(|s| s.list.chats.iter().all(|c| !c.processing()))
    });
    assert!(fx.lists() > listed);
    assert!(!fx.chat().state().list_live);
}

#[test]
fn the_webs_logos_come_from_the_server_once_and_are_kept() {
    let mut fx = fixture(Options::default());
    fx.ready();
    // Nothing runs, so nothing but the logos could ask for frames.
    fx.server.finish(12, 123, "Done.");
    fx.open_waiting(11, "Want me to add it to the Q4 checklist?");
    fx.harness
        .get_by_label("Searched Drive and Slack · 3 searches, read 2 files")
        .click_accesskit();
    fx.logos();
    let asked: Vec<String> = fx
        .server
        .requests()
        .iter()
        .filter(|r| r["cmd"] == "asset")
        .map(|r| r["path"].as_str().unwrap_or_default().to_string())
        .collect();
    for path in [
        "/assets/providers/vertexai-4d2e8f90.svg",
        "/assets/providers/drive-7a3c91e0.svg",
        "/assets/providers/slack-2b8d4f61.svg",
        "/assets/mimetypes/application-pdf-1e2d3c4b.svg",
        "/assets/mimetypes/x-office-spreadsheet-5a6b7c8d.svg",
    ] {
        assert_eq!(
            asked.iter().filter(|p| *p == path).count(),
            1,
            "{path} once: {asked:?}"
        );
    }
    let kept = fx
        .server
        .paths
        .daemon
        .data_dir
        .join("chat-assets/chatwithwork.com/providers__drive-7a3c91e0.svg");
    assert!(kept.is_file(), "kept at {}", kept.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&kept).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Drawn from memory from now on: nothing more is asked, nothing moves.
    let before = fx.server.requests().len();
    fx.harness.run_steps(4);
    assert_eq!(fx.server.requests().len(), before);
    fx.harness
        .try_run()
        .expect("loaded logos ask for no frames");
}

#[test]
fn the_sidebar_shows_whats_pinned_as_the_web_does() {
    OPENED.with(|o| o.borrow_mut().clear());
    let mut fx = fixture(Options::default());
    fx.ready();
    fx.wait_for_label("Pinned");
    // The web shows the organization only to someone in more than one,
    // which the chat API doesn't say: none, as for someone in one.
    assert!(fx.harness.query_by_label_contains("Northwind").is_none());
    for label in ["Recent", "Pinned", "All projects", "Falcon"] {
        fx.harness.get_by_label(label);
    }
    // A pinned chat stays in Recent too; the pin opens it.
    let pinned: Vec<_> = fx
        .harness
        .get_all_by_label("Weekly report for leadership")
        .collect();
    assert_eq!(pinned.len(), 2);
    pinned[1].click();
    fx.wait_for_request("chat", |r| r["chat"] == "4");
    // Chats opens the web's page of every chat.
    fx.harness.get_by_label("Chats").click();
    fx.harness.run_steps(2);
    assert_eq!(
        OPENED.with(|o| o.borrow().clone()),
        ["https://chatwithwork.com/northwind/chats"]
    );
}

#[test]
fn projects_open_on_the_web_as_the_web_sidebar_links_them() {
    OPENED.with(|o| o.borrow_mut().clear());
    let mut fx = fixture(Options::default());
    fx.ready();
    fx.wait_for_label("Falcon");
    // A pinned project opens its page, and All projects the web's list.
    fx.harness.get_by_label("Falcon").click();
    fx.harness.run_steps(2);
    fx.harness.get_by_label("All projects").click();
    fx.harness.run_steps(2);
    assert_eq!(
        OPENED.with(|o| o.borrow().clone()),
        [
            "https://chatwithwork.com/northwind/projects/7",
            "https://chatwithwork.com/northwind/projects",
        ]
    );
    // Nothing opens in the app: no picker, the same chat list, and the
    // next question starts a chat of its own.
    assert!(fx.harness.query_by_label("People").is_none());
    assert!(fx.harness.query_by_label_contains("Show all").is_none());
    let field = fx
        .harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput);
    field.focus();
    field.type_text("What's left before launch?");
    fx.harness.run_steps(2);
    fx.harness.get_by_label("Send message").click();
    let sent = fx.wait_for_request("chat_send", |_| true);
    assert!(sent["project"].is_null(), "{sent}");
}

#[test]
fn a_pinned_project_is_current_while_one_of_its_chats_is_open() {
    let mut fx = fixture(Options::default());
    fx.ready();
    fx.wait_for_label("Falcon");
    // The pin, not the project's line over the open chat, which links too.
    let current = |fx: &Fixture| {
        let pin = fx
            .harness
            .get_all_by_label("Falcon")
            .find(|n| n.accesskit_node().role() != egui::accesskit::Role::Link)
            .expect("the pin");
        let node = pin.accesskit_node();
        node.is_selected() == Some(true) || node.toggled() == Some(egui::accesskit::Toggled::True)
    };
    assert!(!current(&fx));
    fx.chat().open(10);
    fx.harness.run_steps(2);
    assert!(current(&fx), "a chat in Falcon is open");
}

#[test]
fn your_name_opens_settings_on_the_web_and_this_computer_its_own() {
    OPENED.with(|o| o.borrow_mut().clear());
    let mut fx = fixture(Options::default());
    fx.ready();
    // As on the web, your name links to Settings: no menu.
    fx.harness.get_by_label("Settings and usage").click();
    fx.harness.run_steps(2);
    assert_eq!(
        OPENED.with(|o| o.borrow().clone()),
        ["https://chatwithwork.com/northwind/settings?tab=account"]
    );
    assert!(fx.harness.query_by_label("Usage").is_none(), "no menu");
    assert_eq!(fx.harness.state().page(), Page::Chat);
    // This computer's settings are one link of their own.
    fx.harness.get_by_label("This computer").click();
    fx.harness.run_steps(2);
    assert_eq!(fx.harness.state().page(), Page::Folders);
}

#[test]
fn the_logotype_starts_a_new_chat_as_on_the_web() {
    let mut fx = fixture(Options::default());
    fx.ready();
    fx.open(11);
    fx.wait("the chat", |h| {
        h.state().chat_state().is_some_and(|s| s.open == Some(11))
    });
    fx.harness.get_by_label("Chat with Work").click();
    fx.harness.run_steps(2);
    assert_eq!(fx.harness.state().chat_state().unwrap().open, None);
}

#[test]
fn credits_and_projects_change_in_place() {
    let mut fx = fixture(Options::default());
    fx.ready();
    fx.live();
    assert!(fx.harness.query_by_label_contains("credits left").is_none());
    // Running low: the meter shows under the name, as on the web.
    fx.server.spend(410);
    fx.wait_for_label("410 of 5,000 credits left");
    // A project joined elsewhere is in the list.
    let mut projects = crate::demo::projects();
    projects.push(crate::demo::project(9, "Board", "folder-simple"));
    fx.server.set_projects(projects);
    fx.wait("the new project", |h| {
        h.state()
            .chat_state()
            .is_some_and(|s| s.list.projects.iter().any(|p| p.id == 9))
    });
}

#[test]
fn a_question_sent_is_pinned_near_the_top_with_its_answer_under_it() {
    let mut fx = fixture(Options::default());
    fx.ready();
    fx.open_waiting(11, "Want me to add it to the Q4 checklist?");
    // Opened, the latest question sits 24 under the top, as on the web.
    let top = fx
        .harness
        .get_by_label("Draft a short reminder for the team.")
        .rect()
        .top();
    assert!((top - 34.0).abs() < 4.0, "pinned at {top}");
    let field = fx
        .harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput);
    field.focus();
    field.type_text("Who signs the renewal?");
    fx.harness.run_steps(2);
    fx.harness.get_by_label("Send message").click();
    fx.wait_for_request("chat_send", |_| true);
    // The new question scrolls up to the same place, smoothly, with room
    // under it for the answer to come.
    fx.wait("the question pinned", |h| {
        h.query_all_by_label("Who signs the renewal?")
            .last()
            .is_some_and(|q| (q.rect().top() - 34.0).abs() < 4.0)
    });
    assert!(
        fx.harness
            .query_by_label("Draft a short reminder for the team.")
            .is_none_or(|q| q.rect().bottom() < 34.0),
        "the earlier one scrolled up"
    );
}

#[test]
fn a_narrow_window_keeps_the_sidebar_in_a_drawer() {
    let mut fx = fixture(Options {
        size: [760.0, 820.0],
        ..Options::default()
    });
    fx.wait("the chats", |h| {
        h.state().chat_state().is_some_and(|s| s.ready())
    });
    assert!(
        fx.harness
            .query_by_label("Vendor contract renewal")
            .is_none()
    );
    fx.harness.get_by_label("Open sidebar").click();
    fx.wait_for_label("Vendor contract renewal");
    fx.harness.get_by_label("Vendor contract renewal").click();
    fx.wait_for_request("chat", |r| r["chat"] == "11");
    // Choosing a chat closes the drawer.
    fx.wait("the drawer to close", |h| {
        h.query_by_label("Close sidebar").is_none()
    });
}

/// The page rendered offscreen, light and dark, wide and narrow, against
/// the snapshots in `app/tests/snapshots` (compared on Linux, where they
/// were made; `UPDATE_SNAPSHOTS=1` writes them). Skipped where wgpu finds
/// no adapter at all.
#[test]
fn renders_like_the_web() {
    let screens: [(&str, Option<u64>, [f32; 2]); 7] = [
        ("chat", Some(11), [1280.0, 820.0]),
        ("chat-narrow", Some(11), [760.0, 820.0]),
        ("chat-streaming", Some(12), [1280.0, 820.0]),
        ("chat-new", None, [1280.0, 820.0]),
        ("chat-new-narrow", None, [760.0, 820.0]),
        ("chat-approval", Some(5), [1280.0, 820.0]),
        ("chat-question", Some(4), [1280.0, 820.0]),
    ];
    for dark in [false, true] {
        for (name, chat, size) in screens {
            let Ok(renderer) = std::panic::catch_unwind(egui_kittest::wgpu::WgpuTestRenderer::new)
            else {
                eprintln!("no wgpu adapter here: skipping the snapshots");
                return;
            };
            let mut fx = fixture(Options {
                size,
                dark,
                renderer: Some(renderer),
                ..Options::default()
            });
            fx.ready();
            fx.chat().set_still(true);
            fx.chat().set_greeting(4);
            if let Some(number) = chat {
                fx.chat().open(number);
                fx.wait("the transcript", |h| {
                    h.state()
                        .chat_state()
                        .is_some_and(|s| s.transcript.is_some())
                });
            }
            if chat == Some(12) {
                fx.wait_for_label("Q3 spending came in at €1.28M");
                fx.harness
                    .get_by_label("Searching Drive and Slack · Reading Q3 actuals.xlsx")
                    .click();
            }
            fx.logos();
            // kittest paints a pointer wherever one hovers: take it away.
            fx.harness.event(egui::Event::PointerGone);
            // Let fonts and textures settle.
            fx.harness.run_steps(12);
            let theme = if dark { "dark" } else { "light" };
            let file = format!("{name}-{theme}");
            let image = fx.harness.render().expect("rendering");
            let options = egui_kittest::SnapshotOptions::new()
                .threshold(2.0)
                .max_failed_pixels(((size[0] * size[1]) * 0.01) as usize);
            if cfg!(target_os = "linux") {
                egui_kittest::image_snapshot_options(&image, &file, &options);
            } else {
                // Other platforms render text their own way: render only.
                assert!(image.width() > 0);
            }
        }
    }
}

/// A file dropped on the window, as the windowing layer hands it over.
#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

impl Fixture {
    /// Draw a few frames, so the page asks for the images it shows, and
    /// wait until they're all in.
    fn logos(&mut self) {
        self.harness.run_steps(2);
        self.wait("the logos", |h| {
            h.state().chat_ref().is_some_and(|c| c.logos_pending() == 0)
        });
    }

    /// Wait for the chats and the models.
    fn ready(&mut self) {
        self.wait("the models", |h| {
            h.state()
                .chat_state()
                .is_some_and(|s| matches!(s.models, super::state::ModelList::Ready(_)))
        });
    }

    fn open_waiting(&mut self, number: u64, label: &str) {
        self.ready();
        self.chat().open(number);
        self.wait_for_label(label);
        self.harness.run_steps(4);
    }
}

#[test]
fn a_model_picked_goes_with_the_question() {
    let mut fx = fixture(Options::default());
    fx.ready();
    fx.wait_for_label("Model: Gemini 3.8 Flash");
    fx.harness.get_by_label("Model: Gemini 3.8 Flash").click();
    fx.wait_for_label("Mistral Large 3");
    // One that can't be asked now says why, and stays unpicked.
    fx.harness.get_by_label("GPT-6 Sol").click();
    fx.wait_for_label("You're out of credits. Add credits in Settings to keep going.");
    fx.harness.get_by_label("Model: Gemini 3.8 Flash").click();
    fx.wait_for_label("Gemini 3.8 Pro");
    fx.harness.get_by_label("Gemini 3.8 Pro").click();
    fx.wait_for_label("Model: Gemini 3.8 Pro");
    let field = fx
        .harness
        .get_by_role(egui::accesskit::Role::MultilineTextInput);
    field.focus();
    field.type_text("Summarize the Acme contract.");
    fx.harness.run_steps(2);
    fx.harness.get_by_label("Send message").click();
    let sent = fx.wait_for_request("chat_send", |_| true);
    assert_eq!(sent["model"], "14");
}

#[test]
fn files_attach_from_the_picker_and_by_dropping() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("Q3 plan.pdf");
    std::fs::write(&pdf, b"%PDF-1.7 synthetic").unwrap();
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, b"Synthetic notes").unwrap();
    let svg = dir.path().join("logo.svg");
    std::fs::write(&svg, b"<svg/>").unwrap();
    let mut fx = fixture(Options::default());
    fx.ready();
    PICKED.with(|p| *p.borrow_mut() = Some(vec![pdf]));
    fx.harness.get_by_label("Attach files").click();
    let upload = fx.wait_for_request("chat_upload", |_| true);
    assert_eq!(upload["filename"], "Q3 plan.pdf");
    assert_eq!(upload["content_type"], "application/pdf");
    fx.wait_for_label("Q3 plan.pdf, 18.00 B");
    // Dropped on the window: the composer takes the text file, and says
    // it can't take the SVG, as the web does.
    fx.harness.input_mut().dropped_files = vec![
        std::sync::Arc::new(Dropped(notes)),
        std::sync::Arc::new(Dropped(svg)),
    ];
    fx.harness.run_steps(2);
    fx.wait_for_label("That file can't be attached");
    fx.harness
        .get_by_label_contains("isn't a supported file type");
    fx.harness.get_by_label("OK").click();
    fx.wait_for_request("chat_upload", |r| r["filename"] == "notes.txt");
    fx.wait_for_label("notes.txt, 15.00 B");
    // Taken off again.
    fx.harness.get_by_label("Remove notes.txt").click();
    fx.harness.run_steps(2);
    assert!(fx.harness.query_by_label("notes.txt, 15.00 B").is_none());
    // Files alone can be sent.
    fx.harness.get_by_label("Send message").click();
    let sent = fx.wait_for_request("chat_send", |_| true);
    assert_eq!(sent["attachments"], serde_json::json!(["demo-upload-1"]));
    // The question shows its file.
    fx.wait_for_label("Q3 plan.pdf, 18 Bytes");
}

#[test]
fn answers_offer_what_the_chat_allows() {
    let mut fx = fixture(Options::default());
    fx.ready();
    fx.wait_for_label("Vendor contract renewal");
    fx.open(11);
    fx.wait_for_label("Want me to add it to the Q4 checklist?");
    fx.harness.run_steps(5);
    // The latest answer keeps its actions in view.
    for label in [
        "Retry this answer",
        "Branch into a new chat",
        "Share a public link to this chat",
    ] {
        fx.harness.get_by_label(label);
    }
    // Retry asks first.
    fx.harness.get_by_label("Retry this answer").click();
    fx.wait_for_label("Retry this answer?");
    fx.harness.get_by_label("Retry").click();
    let retried = fx.wait_for_request("chat_retry", |_| true);
    assert_eq!(
        (retried["chat"].as_str(), retried["message"].as_str()),
        (Some("11"), Some("115"))
    );
    // A project chat started by someone else: no retry, and no share.
    fx.chat().open(6);
    fx.wait_for_label("Interviews for the third role finish on Thursday.");
    fx.harness.run_steps(5);
    fx.harness.get_by_label("Branch into a new chat");
    assert!(fx.harness.query_by_label("Retry this answer").is_none());
    assert!(
        fx.harness
            .query_by_label("Share a public link to this chat")
            .is_none()
    );
    // Branching opens the new chat.
    fx.harness.get_by_label("Branch into a new chat").click();
    let branched = fx.wait_for_request("chat_branch", |_| true);
    assert_eq!(branched["message"], "62");
    fx.wait_for_label("Branch of Hiring pipeline status");
    fx.wait("the branch to open", |h| {
        h.state().chat_state().is_some_and(|s| s.open == Some(13))
    });
}

#[test]
fn a_chat_is_shared_and_stops_being_shared_from_its_dialog() {
    let mut fx = fixture(Options::default());
    fx.ready();
    fx.open_waiting(11, "Want me to add it to the Q4 checklist?");
    fx.harness
        .get_by_label("Share a public link to this chat")
        .click();
    fx.wait_for_label("Public");
    fx.harness
        .get_by_label("https://chatwithwork.com/shared/9aXk2pQ7mN4rT8vW1yZ3bC5d");
    fx.harness.get_by_label("Stop sharing").click();
    fx.wait_for_request("chat_unshare", |r| r["chat"] == "11");
    fx.wait_for_label("Private");
    fx.harness.get_by_label("Create public link").click();
    fx.wait_for_request("chat_share", |r| r["chat"] == "11");
    // The link is copied as it's made.
    fx.wait_for_label("Copied");
    fx.harness.get_by_label("Done").click();
    fx.harness.run_steps(2);
    assert!(fx.harness.query_by_label("Share this chat").is_none());
}

#[test]
fn a_change_is_approved_for_the_rest_of_the_chat() {
    let mut fx = fixture(Options::default());
    fx.open_waiting(5, "Waiting for your approval.");
    fx.harness
        .get_by_label_contains("Needs your approval · Slack: Post a message to #launch");
    fx.harness.get_by_label("Message: Falcon is live! Thanks, everyone: the release notes are in Drive, and the status page is up.");
    fx.harness
        .get_by_label("Allow this in Slack for the rest of this chat")
        .click();
    fx.harness.run_steps(1);
    fx.harness.get_by_label("Approve").click();
    let approved = fx.wait_for_request("chat_approve", |_| true);
    assert_eq!(approved["tool_call"], "531");
    assert_eq!(approved["for_rest_of_chat"], true);
    // The card goes once nothing waits, and the answer carries on.
    fx.wait("the card to go", |h| h.query_by_label("Approve").is_none());
    assert!(
        fx.harness
            .query_by_label("Waiting for your approval.")
            .is_none()
    );
}

#[test]
fn a_change_is_denied_with_a_reason() {
    let mut fx = fixture(Options::default());
    fx.open_waiting(5, "Waiting for your approval.");
    fx.harness.get_by_label("Deny with a reason").click();
    fx.harness.run_steps(2);
    let field = fx.harness.get_by_label("Why you're denying it");
    field.focus();
    field.type_text("Wait for the status page");
    fx.harness.run_steps(1);
    fx.harness.get_by_label("Deny").click();
    let denied = fx.wait_for_request("chat_deny", |_| true);
    assert_eq!(denied["reason"], "Wait for the status page");
}

#[test]
fn a_tools_question_is_answered_with_its_form() {
    let mut fx = fixture(Options::default());
    fx.open_waiting(4, "Waiting for your answer.");
    fx.harness
        .get_by_label_contains("Only Notion sees your answer.");
    fx.harness.get_by_label("Environment").click();
    fx.wait_for_label("production");
    fx.harness.get_by_label("production").click();
    fx.harness.run_steps(1);
    fx.harness.get_by_label("Hiring").click();
    fx.harness.get_by_label("Include drafts").click();
    let days = fx.harness.get_by_label("Days (optional)");
    days.focus();
    days.type_text("0");
    fx.harness.run_steps(1);
    // The card's foot is below the fold, under the question pinned at top.
    fx.harness.get_by_label("Send").click_accesskit();
    let answered = fx.wait_for_request("chat_answer", |_| true);
    assert_eq!(answered["tool_call"], "432");
    assert_eq!(
        answered["input"],
        serde_json::json!({
            "environment": "production",
            "days": "70",
            "sections": ["Revenue", "Hiring"],
            "include_drafts": true,
        })
    );
}

#[test]
fn a_page_to_visit_opens_in_the_browser_then_is_answered() {
    OPENED.with(|o| o.borrow_mut().clear());
    let mut fx = fixture(Options::default());
    fx.open_waiting(3, "Waiting for your answer.");
    fx.harness
        .get_by_label("It asks you to open a page on linear.app, then come back.");
    fx.harness.get_by_label("Open linear.app").click();
    fx.harness.run_steps(2);
    assert_eq!(
        OPENED.with(|o| o.borrow().clone()),
        ["https://linear.app/oauth/authorize?client_id=northwind-demo"]
    );
    fx.harness.get_by_label("I've done it").click();
    let answered = fx.wait_for_request("chat_answer", |_| true);
    assert!(answered.get("input").is_none(), "{answered}");
}

#[test]
fn a_question_can_be_declined() {
    let mut fx = fixture(Options::default());
    fx.open_waiting(3, "Waiting for your answer.");
    fx.harness.get_by_label("Decline").click();
    let declined = fx.wait_for_request("chat_decline", |_| true);
    assert_eq!(declined["tool_call"], "332");
}

#[test]
fn chats_are_renamed_and_deleted_from_their_row_menu() {
    let mut fx = fixture(Options::default());
    fx.ready();
    fx.wait_for_label("Vendor contract renewal");
    // Chat 11's row is the list's second.
    fx.harness
        .get_all_by_label("Chat options")
        .nth(1)
        .unwrap()
        .click();
    fx.wait_for_label("Rename");
    fx.harness.get_by_label("Rename").click();
    fx.harness.run_steps(2);
    let field = fx.harness.get_by_label("Chat title");
    field.focus();
    field.type_text(" 2027");
    fx.harness.run_steps(1);
    fx.harness.get_by_label("Save title").click();
    let renamed = fx.wait_for_request("chat_rename", |_| true);
    assert_eq!(renamed["title"], "Vendor contract renewal 2027");
    fx.wait_for_label("Vendor contract renewal 2027");
    fx.harness
        .get_all_by_label("Chat options")
        .nth(1)
        .unwrap()
        .click();
    fx.wait_for_label("Delete");
    fx.harness.get_by_label("Delete").click();
    fx.wait_for_label("Are you sure?");
    fx.harness.run_steps(2);
    fx.harness.get_by_label("Delete").click();
    fx.wait_for_request("chat_delete", |r| r["chat"] == "11");
    fx.wait("the chat to go", |h| {
        h.query_by_label("Vendor contract renewal 2027").is_none()
    });
}

#[test]
fn answer_text_is_selected_with_the_mouse_and_copied() {
    let mut fx = fixture(Options::default());
    fx.ready();
    fx.open_waiting(11, "Want me to add it to the Q4 checklist?");
    let rect = fx
        .harness
        .get_by_label("Want me to add it to the Q4 checklist?")
        .rect();
    let (from, to) = (
        rect.left_center() + egui::vec2(1.0, 0.0),
        rect.right_center() - egui::vec2(1.0, 0.0),
    );
    fx.harness.event(egui::Event::PointerMoved(from));
    fx.harness.step();
    fx.harness.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    fx.harness.step();
    fx.harness.event(egui::Event::PointerMoved(to));
    fx.harness.step();
    fx.harness.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    fx.harness.step();
    fx.harness.event(egui::Event::Copy);
    fx.harness.step();
    let copied: Vec<String> = fx
        .harness
        .output()
        .platform_output
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(copied, ["Want me to add it to the Q4 checklist?"]);
}

/// One screen for the review images: what to open and set up.
struct Screen {
    name: &'static str,
    chat: Option<u64>,
    setup: fn(&mut Fixture),
}

fn review_screens() -> Vec<Screen> {
    fn nothing(_: &mut Fixture) {}
    vec![
        Screen {
            name: "picker",
            chat: None,
            setup: |fx| {
                fx.wait_for_label("Model: Gemini 3.8 Flash");
                fx.harness.get_by_label("Model: Gemini 3.8 Flash").click();
            },
        },
        Screen {
            name: "files",
            chat: None,
            setup: |fx| {
                let state = fx.chat().state_mut();
                state.input = "What changed between these two?".into();
                state.attachments = vec![
                    super::state::Attached {
                        key: 900,
                        path: "/tmp/Q3 plan.pdf".into(),
                        name: "Q3 plan.pdf".into(),
                        size: 48_210,
                        content_type: "application/pdf".into(),
                        signed_id: Some("demo".into()),
                        icon: None,
                    },
                    super::state::Attached {
                        key: 901,
                        path: "/tmp/Vendor list.xlsx".into(),
                        name: "Vendor list.xlsx".into(),
                        size: 18_432,
                        content_type:
                            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                                .into(),
                        signed_id: None,
                        icon: None,
                    },
                ];
            },
        },
        Screen {
            name: "chat",
            chat: Some(11),
            setup: nothing,
        },
        Screen {
            name: "approval",
            chat: Some(5),
            setup: nothing,
        },
        Screen {
            name: "steps",
            chat: Some(11),
            setup: |fx| {
                fx.harness
                    .get_by_label("Searched Drive and Slack · 3 searches, read 2 files")
                    .click_accesskit();
            },
        },
        Screen {
            name: "computer",
            chat: Some(8),
            setup: |fx| {
                fx.harness
                    .get_by_label("Searched Alex's MacBook · 1 search, read 1 file")
                    .click_accesskit();
            },
        },
        Screen {
            name: "plan",
            chat: Some(7),
            setup: |fx| {
                fx.harness
                    .get_by_label("Made a plan · 2 steps")
                    .click_accesskit();
            },
        },
        Screen {
            name: "sent",
            chat: Some(11),
            setup: |fx| {
                fx.chat().state_mut().input = "Who signs the renewal?".into();
                fx.harness.run_steps(2);
                fx.harness.get_by_label("Send message").click();
                fx.wait_for_request("chat_send", |_| true);
                fx.harness.run_steps(20);
            },
        },
        Screen {
            name: "notice",
            chat: Some(9),
            setup: |fx| {
                fx.chat().state_mut().notice =
                    Some("An answer is being written in this chat.".into())
            },
        },
        Screen {
            name: "account",
            chat: Some(11),
            setup: nothing,
        },
        Screen {
            name: "project-chat",
            chat: Some(10),
            setup: nothing,
        },
        Screen {
            name: "credits",
            chat: Some(11),
            setup: |fx| {
                fx.server.spend(410);
                fx.wait("the meter", |h| {
                    h.query_by_label("410 of 5,000 credits left").is_some()
                });
            },
        },
        Screen {
            name: "sources",
            chat: Some(11),
            setup: |fx| fx.chat().conversation_mut().sources_open = Some(113),
        },
        Screen {
            name: "question",
            chat: Some(4),
            setup: nothing,
        },
        Screen {
            name: "url",
            chat: Some(3),
            setup: nothing,
        },
        Screen {
            name: "menu",
            chat: Some(11),
            setup: |fx| fx.chat().state_mut().menu = Some(11),
        },
        Screen {
            name: "rename",
            chat: Some(11),
            setup: |fx| {
                fx.chat().state_mut().renaming = Some((11, "Vendor contract renewal".into()))
            },
        },
        Screen {
            name: "share",
            chat: Some(11),
            setup: |fx| {
                fx.chat().state_mut().dialog = Some(super::state::Dialog::Share {
                    chat: 11,
                    copied: false,
                })
            },
        },
        Screen {
            name: "share-private",
            chat: Some(5),
            setup: |fx| {
                fx.chat().state_mut().dialog = Some(super::state::Dialog::Share {
                    chat: 5,
                    copied: false,
                })
            },
        },
        Screen {
            name: "retry",
            chat: Some(11),
            setup: |fx| {
                fx.chat().state_mut().dialog = Some(super::state::Dialog::Retry {
                    chat: 11,
                    message: 115,
                    user: false,
                })
            },
        },
        Screen {
            name: "delete",
            chat: Some(11),
            setup: |fx| {
                fx.chat().state_mut().dialog = Some(super::state::Dialog::Delete { chat: 11 })
            },
        },
        Screen {
            name: "unsupported",
            chat: None,
            setup: |fx| {
                fx.chat().state_mut().dialog = Some(super::state::Dialog::Unsupported {
                    name: "logo.svg".into(),
                })
            },
        },
    ]
}

/// Renders the new controls offscreen for review beside the web's
/// references, into `$CWW_PARITY_DIR` (skipped without it, or without a
/// wgpu adapter).
#[test]
fn renders_review_screens() {
    let Some(dir) = std::env::var_os("CWW_PARITY_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    let only = std::env::var("CWW_PARITY_ONLY").ok();
    std::fs::create_dir_all(&dir).unwrap();
    for dark in [false, true] {
        for screen in review_screens() {
            if only
                .as_deref()
                .is_some_and(|o| !o.split(',').any(|n| n == screen.name))
            {
                continue;
            }
            let Ok(renderer) = std::panic::catch_unwind(egui_kittest::wgpu::WgpuTestRenderer::new)
            else {
                eprintln!("no wgpu adapter here");
                return;
            };
            let mut fx = fixture(Options {
                dark,
                renderer: Some(renderer),
                ..Options::default()
            });
            fx.wait("the chats", |h| {
                h.state().chat_state().is_some_and(|s| s.ready())
            });
            fx.wait("the models", |h| {
                h.state()
                    .chat_state()
                    .is_some_and(|s| matches!(s.models, super::state::ModelList::Ready(_)))
            });
            fx.chat().set_still(true);
            fx.chat().set_greeting(4);
            if let Some(number) = screen.chat {
                fx.chat().open(number);
                fx.wait("the transcript", |h| {
                    h.state()
                        .chat_state()
                        .is_some_and(|s| s.transcript.is_some())
                });
            }
            fx.harness.run_steps(4);
            (screen.setup)(&mut fx);
            fx.logos();
            fx.harness.event(egui::Event::PointerGone);
            // Your name, hovered, as the web's link shows it.
            if screen.name == "account" {
                fx.harness.get_by_label("Settings and usage").hover();
            }
            fx.harness.run_steps(12);
            let image = fx.harness.render().expect("rendering");
            let theme = if dark { "dark" } else { "light" };
            image
                .save(dir.join(format!("egui-{}-{theme}.png", screen.name)))
                .unwrap();
        }
    }
}

#[test]
fn copying_a_message_says_copied_then_turns_back() {
    let mut fx = fixture(Options::default());
    fx.wait_for_label("Vendor contract renewal");
    fx.open(11);
    fx.wait_for_label("The Acme contract renews");
    // The latest answer's actions sit below the fold of the test window.
    fx.harness
        .query_all_by_label("Copy message")
        .last()
        .expect("a Copy button")
        .click_accesskit();
    fx.wait_for_label("Copied");
    // The harness steps a quarter of a second at a time.
    fx.harness.run_steps(10);
    assert!(fx.harness.query_all_by_label("Copied").next().is_none());
    fx.harness.get_by_label("Copy message");
}
