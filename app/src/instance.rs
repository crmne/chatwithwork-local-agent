//! One app per user, through fastframe-instance. A second launch (from the
//! launcher, or at login while the app already runs) asks the running one to
//! open its settings window, then exits.
//!
//! The running app holds a lock in a private per-user directory, which the
//! system releases when the process ends, even after a crash, and answers
//! later launches on a socket only the user can open (on Windows, a loopback
//! port guarded by a token in the user's profile). The demo has a slot of
//! its own, so it runs beside the real app.

use fastframe_instance::{Claim, Guard, Slot};

use crate::events::{AppEvent, Events};

/// The slot's name. Changing it would let a new version start beside an
/// older one that is still running.
const APP_ID: &str = "cww-app";

/// The real app's slot: one per user.
pub fn slot() -> Slot {
    Slot::new(APP_ID)
}

/// The demo's slot, beside the real app's: one demo at a time.
#[cfg_attr(not(feature = "demo"), allow(dead_code))]
pub fn demo_slot() -> Slot {
    slot().scoped("demo")
}

/// Whether this launch goes on.
#[derive(Debug)]
pub enum Start {
    /// This process is the app. Keep the guard until it exits.
    Run(Guard),
    /// The running app took the request; this launch is done.
    HandedOver,
}

/// What a launch asks the running app: `show` opens its window, `ping` (a
/// start at login) only checks it runs.
fn request(background: bool) -> &'static str {
    if background { "ping" } else { "show" }
}

/// The running app's answer to a later launch, on the listener's thread.
fn answer(events: &Events, request: &str) -> Option<String> {
    match request {
        "show" => events.send(AppEvent::OpenSettings),
        "ping" => {}
        _ => return None,
    }
    Some("ok".to_owned())
}

/// Becomes the app, or hands the launch to the one already running.
///
/// An app that holds the slot but does not answer is still running (the
/// lock goes with the process), so this launch does not start a second
/// copy, with a second tray item and window on the same settings: it stops
/// with an error that says what to do.
pub fn claim(slot: &Slot, background: bool, events: Events) -> anyhow::Result<Start> {
    match slot.claim(request(background), move |request| answer(&events, request)) {
        Claim::First(guard) => Ok(Start::Run(guard)),
        Claim::Running(_) => {
            log::info!("already running; handed the launch over");
            Ok(Start::HandedOver)
        }
        // The running copy refused this launch's request: nothing to do.
        Claim::Declined => Ok(Start::HandedOver),
        Claim::Unanswered => anyhow::bail!(
            "Chat with Work is already running but did not answer in {} seconds. \
             Quit it from its menu, or end the cww-app process, and open it again.",
            fastframe_instance::ANSWER_WAIT.as_secs()
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    use super::*;
    use crate::events::Waker;

    fn events() -> (Events, Receiver<AppEvent>) {
        Events::new(Waker::new(|| {}))
    }

    fn running(slot: &Slot) -> (Guard, Receiver<AppEvent>) {
        let (events, rx) = events();
        let Start::Run(guard) = claim(slot, false, events).unwrap() else {
            panic!("the first launch runs");
        };
        (guard, rx)
    }

    #[test]
    fn a_second_launch_asks_the_first_to_open_its_window() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = Slot::at(tmp.path(), APP_ID);
        let (_guard, rx) = running(&slot);

        let (later, later_rx) = events();
        assert!(matches!(
            claim(&slot, false, later).unwrap(),
            Start::HandedOver
        ));
        let event = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(event, AppEvent::OpenSettings);
        assert!(
            later_rx.try_recv().is_err(),
            "the later launch runs nothing"
        );
    }

    #[test]
    fn a_launch_at_login_leaves_the_window_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = Slot::at(tmp.path(), APP_ID);
        let (_guard, rx) = running(&slot);

        let (later, _) = events();
        assert!(matches!(
            claim(&slot, true, later).unwrap(),
            Start::HandedOver
        ));
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn unknown_requests_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = Slot::at(tmp.path(), APP_ID);
        let (_guard, rx) = running(&slot);

        assert_eq!(slot.send("ping").unwrap(), "ok");
        assert!(
            slot.send("quit").is_err(),
            "no reply to a request it refuses"
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn an_app_that_does_not_answer_is_not_started_twice() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = Slot::at(tmp.path(), APP_ID);
        // A copy that holds the lock but never listens (hung, say).
        let held = std::fs::File::create(tmp.path().join("instance.lock")).unwrap();
        held.lock().unwrap();

        let (later, later_rx) = events();
        let error = claim(&slot, false, later).unwrap_err();
        assert!(error.to_string().contains("already running"), "{error}");
        assert!(later_rx.try_recv().is_err());
    }

    #[test]
    fn the_slot_frees_up_when_the_app_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = Slot::at(tmp.path(), APP_ID);
        let (guard, _rx) = running(&slot);
        drop(guard);
        let (_guard, _rx) = running(&slot);
    }

    #[test]
    fn the_demo_runs_beside_the_app() {
        assert_ne!(demo_slot().dir(), slot().dir());
        assert!(demo_slot().dir().starts_with(slot().dir()));

        let tmp = tempfile::tempdir().unwrap();
        let app = Slot::at(tmp.path(), APP_ID);
        let (_app, app_rx) = running(&app);
        let (_demo, _demo_rx) = running(&app.clone().scoped("demo"));
        assert!(app_rx.try_recv().is_err(), "the demo did not knock");
    }
}
