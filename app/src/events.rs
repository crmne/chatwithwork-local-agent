//! How the tray, the watch thread and other instances reach the main loop.
//!
//! Senders push an [`AppEvent`] and wake the event loop, which is otherwise
//! asleep: nothing runs until one of these arrives or the window needs it.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::model::Status;

// A status is a few hundred bytes and arrives once per change; boxing it
// would buy nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    /// The daemon's status changed; `None` while it isn't running.
    Status(Option<Status>),
    OpenSettings,
    TogglePause,
    Quit,
}

/// Wakes the event loop from any thread.
#[derive(Clone)]
pub struct Waker(Arc<dyn Fn() + Send + Sync>);

impl Waker {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(wake))
    }

    pub fn wake(&self) {
        (self.0)();
    }
}

#[derive(Clone)]
pub struct Events {
    tx: Sender<AppEvent>,
    waker: Waker,
}

impl Events {
    pub fn new(waker: Waker) -> (Self, Receiver<AppEvent>) {
        let (tx, rx) = channel();
        (Self { tx, waker }, rx)
    }

    pub fn send(&self, event: AppEvent) {
        if self.tx.send(event).is_ok() {
            self.waker.wake();
        }
    }

    pub fn waker(&self) -> &Waker {
        &self.waker
    }
}
