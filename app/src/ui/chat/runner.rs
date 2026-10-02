//! Runs the chat page's commands against the daemon, each on its own
//! thread, as the terminal UI does, and wakes the window when an answer
//! comes back. Following a chat holds one connection open, blocked on the
//! daemon's socket; heartbeats on it don't wake the window.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cww::control::Closer;
use cww::tui::chat::Chats;

use super::state::{Command, Msg};

/// How often to check whether the owner allowed chats, and for how long.
const ACCESS_POLL: Duration = Duration::from_secs(5);
const ACCESS_WAIT: Duration = Duration::from_secs(15 * 60);

pub struct Runner {
    chats: Chats,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
    following: Mutex<Option<Follower>>,
    /// Bumped to stop an earlier wait for chat access.
    access_wait: Arc<AtomicU64>,
}

struct Follower {
    stop: Arc<AtomicBool>,
    closer: Arc<Mutex<Option<Closer>>>,
}

impl Follower {
    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(closer) = self.closer.lock().expect("closer").take() {
            closer.close();
        }
    }
}

/// Sends a message and wakes the window to read it.
#[derive(Clone)]
struct Wake {
    tx: Sender<Msg>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
}

impl Wake {
    fn send(&self, msg: Msg) -> bool {
        let sent = self.tx.send(msg).is_ok();
        if let Some(ctx) = &*self.ctx.lock().expect("ctx") {
            ctx.request_repaint();
        }
        sent
    }
}

impl Runner {
    pub fn new(socket: &Path) -> Self {
        let (tx, rx) = channel();
        Self {
            chats: Chats::new(socket),
            tx,
            rx,
            ctx: Arc::new(Mutex::new(None)),
            following: Mutex::new(None),
            access_wait: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The window to wake when answers arrive.
    pub fn attach(&self, ctx: &egui::Context) {
        let mut slot = self.ctx.lock().expect("ctx");
        if slot.is_none() {
            *slot = Some(ctx.clone());
        }
    }

    /// Answers that came back since the last frame.
    pub fn drain(&self) -> Vec<Msg> {
        self.rx.try_iter().collect()
    }

    fn wake(&self) -> Wake {
        Wake {
            tx: self.tx.clone(),
            ctx: Arc::clone(&self.ctx),
        }
    }

    pub fn run(&self, command: Command) {
        let (chats, wake) = (self.chats.clone(), self.wake());
        let spawn = |name: &str, work: Box<dyn FnOnce() + Send>| {
            if let Err(e) = thread::Builder::new().name(name.into()).spawn(work) {
                log::error!("can't start a chat thread: {e}");
            }
        };
        match command {
            Command::List => spawn(
                "cww-chats",
                Box::new(move || {
                    wake.send(Msg::Listed(chats.list()));
                }),
            ),
            Command::Open(chat) => spawn(
                "cww-chat",
                Box::new(move || {
                    wake.send(Msg::Shown {
                        chat,
                        result: chats.show(chat),
                    });
                }),
            ),
            Command::Send { chat, text } => spawn(
                "cww-chat-send",
                Box::new(move || {
                    wake.send(Msg::Sent {
                        chat,
                        result: chats.send(chat, &text),
                    });
                }),
            ),
            Command::Cancel(chat) => spawn(
                "cww-chat-cancel",
                Box::new(move || {
                    wake.send(Msg::Cancelled(chats.cancel(chat)));
                }),
            ),
            Command::RequestAccess | Command::WaitForAccess => {
                let ask = command == Command::RequestAccess;
                let generation = self.access_wait.fetch_add(1, Ordering::SeqCst) + 1;
                let current = Arc::clone(&self.access_wait);
                spawn(
                    "cww-chat-access",
                    Box::new(move || {
                        if ask {
                            let result = chats.request_access();
                            let waiting = result.as_ref().is_ok_and(|r| !r.granted);
                            wake.send(Msg::Access(result));
                            if !waiting {
                                return;
                            }
                        }
                        wait_for_access(&chats, &wake, || {
                            current.load(Ordering::SeqCst) == generation
                        });
                    }),
                );
            }
            Command::Follow(chat) => self.follow(chat),
        }
    }

    /// Follow `chat` live, instead of whatever was followed before.
    fn follow(&self, chat: Option<u64>) {
        let mut following = self.following.lock().expect("following");
        if let Some(previous) = following.take() {
            previous.stop();
        }
        let Some(chat) = chat else { return };
        let follower = Follower {
            stop: Arc::new(AtomicBool::new(false)),
            closer: Arc::new(Mutex::new(None)),
        };
        let (stop, closer) = (Arc::clone(&follower.stop), Arc::clone(&follower.closer));
        *following = Some(follower);
        let (chats, wake) = (self.chats.clone(), self.wake());
        let spawned = thread::Builder::new()
            .name("cww-chat-follow".into())
            .spawn(move || {
                let result = chats.follow(
                    chat,
                    |c| {
                        *closer.lock().expect("closer") = c;
                        // Stopped while connecting.
                        if stop.load(Ordering::SeqCst)
                            && let Some(c) = closer.lock().expect("closer").take()
                        {
                            c.close();
                        }
                    },
                    |live| {
                        if stop.load(Ordering::SeqCst) {
                            return false;
                        }
                        match live {
                            Some(live) => wake.send(Msg::Live { chat, live }),
                            // A heartbeat: nothing to draw.
                            None => true,
                        }
                    },
                );
                if let Err(failure) = result
                    && !stop.load(Ordering::SeqCst)
                {
                    wake.send(Msg::FollowFailed { chat, failure });
                }
            });
        if let Err(e) = spawned {
            log::error!("can't follow chat {chat}: {e}");
        }
    }

    /// Stop following, as the window closes.
    pub fn stop(&self) {
        if let Some(follower) = self.following.lock().expect("following").take() {
            follower.stop();
        }
        self.access_wait.fetch_add(1, Ordering::SeqCst);
        *self.ctx.lock().expect("ctx") = None;
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Check back until the owner answers the request for chats. Stops early
/// when `current` says a newer wait took over.
fn wait_for_access(chats: &Chats, wake: &Wake, current: impl Fn() -> bool) {
    let started = Instant::now();
    while started.elapsed() < ACCESS_WAIT && current() {
        thread::sleep(ACCESS_POLL);
        if !current() {
            return;
        }
        match chats.list() {
            Err(failure) if failure.code == "chat_access_required" => {}
            result => {
                wake.send(Msg::Listed(result));
                return;
            }
        }
    }
}
