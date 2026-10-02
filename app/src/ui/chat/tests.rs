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

struct Fixture {
    server: demo::Server,
    harness: Harness<'static, SettingsApp>,
}

struct Options {
    scenario: Scenario,
    size: [f32; 2],
    dark: bool,
    held: bool,
    renderer: Option<egui_kittest::wgpu::WgpuTestRenderer>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scenario: Scenario::Sample,
            size: [1280.0, 820.0],
            dark: false,
            held: true,
            renderer: None,
        }
    }
}

fn fixture(options: Options) -> Fixture {
    let server = demo::start(options.scenario).unwrap();
    server.hold_streams(options.held);
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
            10 => "Falcon launch checklist",
            _ => "Weekly sync summary",
        }
        .into()
    }
}

#[test]
fn chats_are_listed_by_day_and_open_with_their_answers() {
    let mut fx = fixture(Options::default());
    fx.wait_for_label("Vendor contract renewal");
    for day in ["Today", "Yesterday", "Earlier"] {
        fx.harness.get_by_label(day);
    }
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
    // Let it scroll to the latest message first.
    fx.harness.run_steps(5);
    fx.harness.get_by_label("2 sources").click();
    fx.wait_for_label("1. Acme renewal 2026.pdf");
    fx.harness.get_by_label("2. Acme MSA 2024.pdf").click();
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
    fx.wait_for_label("Falcon launch checklist");
    fx.open(10);
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
    let screens: [(&str, Option<u64>, [f32; 2]); 5] = [
        ("chat", Some(11), [1280.0, 820.0]),
        ("chat-narrow", Some(11), [760.0, 820.0]),
        ("chat-streaming", Some(12), [1280.0, 820.0]),
        ("chat-new", None, [1280.0, 820.0]),
        ("chat-new-narrow", None, [760.0, 820.0]),
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
            fx.wait("the chats", |h| {
                h.state().chat_state().is_some_and(|s| s.ready())
            });
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
