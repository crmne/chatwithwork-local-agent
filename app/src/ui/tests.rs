//! The settings window, driven through its accessibility tree against the
//! stand-in daemon, which records every request it gets.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::Value;

use super::theme::Theme;
use super::{AppState, Page, SettingsApp, Shared};
use crate::control::Client;
use crate::demo::{self, Scenario};
use crate::model::Platform;

thread_local! {
    /// URLs the window asked to open, instead of opening a browser.
    pub static OPENED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

struct Fixture {
    server: demo::Server,
    harness: Harness<'static, SettingsApp>,
    documents: PathBuf,
}

fn fixture(scenario: Scenario, page: Option<Page>) -> Fixture {
    let server = demo::start(scenario).unwrap();
    if scenario == Scenario::Sample {
        AppState { onboarded: true }.save(&server.paths);
    }
    let documents = server
        .paths
        .daemon
        .config_dir
        .parent()
        .unwrap()
        .join("Documents");
    std::fs::create_dir_all(&documents).unwrap();
    let shared = Shared::new(
        Client::new(server.paths.socket_path()),
        server.paths.clone(),
    );
    let watcher = Arc::clone(&shared);
    shared
        .client
        .spawn_watch(move |status| watcher.set_status(status));
    let mut app = SettingsApp::new(shared, Theme::new(Platform::current(), false, None));
    app.set_documents(Some(documents.clone()));
    if let Some(page) = page {
        app.set_page(page);
    }
    let harness = Harness::builder()
        .with_size([840.0, 700.0])
        .build_ui_state(|ui, app: &mut SettingsApp| app.show(ui), app);
    let mut fixture = Fixture {
        server,
        harness,
        documents,
    };
    fixture.wait("the first status", |h| h.state().shared.is_known());
    fixture
}

impl Fixture {
    /// Step the window until `done` holds; requests and status lines
    /// arrive from other threads.
    fn wait(&mut self, what: &str, done: impl Fn(&Harness<'static, SettingsApp>) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
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
            h.query_by_label_contains(&label).is_some()
        });
    }

    /// The requests that changed something (not status, subscribe, the log or the deny list).
    fn changes(&self) -> Vec<Value> {
        self.server
            .requests()
            .into_iter()
            .filter(|r| !matches!(r["cmd"].as_str(), Some("subscribe" | "audit_tail" | "deny")))
            .collect()
    }

    fn wait_for_request(&mut self, cmd: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(found) = self.changes().into_iter().find(|r| r["cmd"] == cmd) {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "no {cmd} request: {:?}",
                self.changes()
            );
            self.harness.step();
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn nothing_is_shared_until_the_user_says_so() {
    let mut fx = fixture(Scenario::Fresh, None);
    assert_eq!(fx.harness.state().page(), Page::Welcome, "first run");
    fx.wait_for_label("It's running in the background");
    fx.harness
        .get_by_label_contains("Share your Documents folder");
    // Plenty of frames, and still nothing shared.
    fx.harness.run_steps(30);
    assert!(fx.changes().is_empty(), "{:?}", fx.changes());

    fx.harness.get_by_label("Share Documents").click();
    let added = fx.wait_for_request("roots_add");
    assert_eq!(added["path"], serde_json::json!(fx.documents));
    fx.wait_for_label("Sharing Documents.");
    assert_eq!(fx.changes().len(), 1, "one folder, once");
}

#[test]
fn pairing_opens_the_browser_and_shows_the_code() {
    OPENED.with(|o| o.borrow_mut().clear());
    let mut fx = fixture(Scenario::Fresh, Some(Page::Account));
    fx.wait_for_label("This computer isn't paired");
    fx.harness.get_by_label("Pair with Chat with Work…").click();
    fx.wait_for_request("pair");
    fx.wait_for_label("WDJB-MJHT");
    let opened = OPENED.with(|o| o.borrow().clone());
    assert_eq!(opened, ["https://chatwithwork.com/device"], "opened once");

    fx.harness.get_by_label("Cancel").click();
    fx.wait_for_request("pair_cancel");
    fx.wait_for_label("This computer isn't paired");
}

#[test]
fn folders_are_renamed_and_removed_with_confirmation() {
    let mut fx = fixture(Scenario::Sample, Some(Page::Folders));
    fx.wait_for_label("Indexed · 1,532 files");

    fx.harness
        .get_all_by_label("Rename…")
        .next()
        .unwrap()
        .click();
    fx.harness.run_steps(2);
    let field = fx.harness.get_by_role(egui::accesskit::Role::TextInput);
    field.focus();
    field.type_text(" and notes");
    fx.harness.run_steps(2);
    fx.harness.get_by_label("Save").click();
    let renamed = fx.wait_for_request("roots_label");
    assert_eq!(renamed["root"], "documents");
    assert_eq!(renamed["label"], "Documents and notes");
    fx.wait_for_label("Documents and notes");

    // Removing asks first.
    fx.harness
        .get_all_by_label("Remove…")
        .next()
        .unwrap()
        .click();
    fx.harness.run_steps(3);
    assert!(!fx.changes().iter().any(|r| r["cmd"] == "roots_remove"));
    fx.harness
        .get_by_label_contains("Stop sharing “Documents and notes”?");
    fx.harness.get_by_label("Stop Sharing").click();
    let removed = fx.wait_for_request("roots_remove");
    assert_eq!(removed["root"], "documents");
    fx.wait("the folder to go", |h| {
        h.query_by_label_contains("Documents and notes").is_none()
    });
}

#[test]
fn the_deny_list_and_the_activity_log_are_shown() {
    let mut fx = fixture(Scenario::Sample, Some(Page::Privacy));
    fx.wait_for_label(".ssh");
    fx.harness.get_by_label_contains("*.secret");

    fx.harness.get_by_label("Activity").click();
    fx.wait_for_label("Read documents:Plans/Budget 2027.xlsx");
    fx.harness
        .get_by_label_contains("Read work-projects:Falcon/.env");
    // The filter, not the "Refused" badges on the rows.
    fx.harness
        .get_all_by_label("Refused")
        .find(|n| n.accesskit_node().role() == egui::accesskit::Role::Button)
        .expect("the Refused filter")
        .click();
    fx.wait("the refused filter", |h| {
        h.query_by_label_contains("Read documents:Plans/Budget 2027.xlsx")
            .is_none()
    });
    fx.harness
        .get_by_label_contains("Read work-projects:Falcon/.env");
}

#[test]
fn pausing_is_a_switch_that_screen_readers_can_read() {
    let mut fx = fixture(Scenario::Sample, Some(Page::General));
    fx.wait_for_label("Running · version 0.1.0");
    let switch = fx
        .harness
        .get_by_role_and_label(egui::accesskit::Role::CheckBox, "Pause sharing");
    assert_eq!(
        switch.accesskit_node().role(),
        egui::accesskit::Role::CheckBox
    );
    assert_eq!(
        switch.accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::False)
    );
    switch.click();
    let paused = fx.wait_for_request("pause");
    assert_eq!(paused["cmd"], "pause");
    fx.wait_for_label("Paused");
}

#[test]
fn a_stopped_agent_can_be_started_from_the_window() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::under(tmp.path());
    AppState { onboarded: true }.save(&paths);
    let shared = Shared::new(Client::new(paths.socket_path()), paths);
    shared.set_status(None);
    let app = SettingsApp::new(shared, Theme::new(Platform::current(), true, None));
    let mut harness = Harness::builder()
        .with_size([840.0, 600.0])
        .build_ui_state(|ui, app: &mut SettingsApp| app.show(ui), app);
    harness.run_steps(3);
    harness.get_by_label_contains("The Local Agent isn't running");
    harness.get_by_label("Start Local Agent");
    harness.get_by_label_contains("Not running");
}
