//! `cww tui`: the terminal UI.
//!
//! Three kinds of input feed one channel: keys (a thread blocked in
//! crossterm's `event::read`), the daemon (a thread blocked on the control
//! subscription, see [`daemon`]), and chats (worker threads that ask the
//! daemon, and one that follows the open chat live). The loop blocks on that
//! channel and redraws only when something arrives. It sets a timeout only
//! while something on screen moves (a spinner, an answer being written) or a
//! relative time like "2s ago" is about to change, so an idle TUI uses no
//! CPU.

pub mod app;
pub mod chat;
mod daemon;
pub mod markdown;
pub mod theme;
pub mod ui;

#[cfg(test)]
mod tests;

use std::io::stdout;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use crossterm::execute;
use ratatui::DefaultTerminal;
use time::{OffsetDateTime, UtcOffset};

use self::app::{App, ChatCommand, ChatMsg, Effect, Msg};
use self::chat::Chats;
use self::theme::Theme;
use crate::control::Closer;
use crate::paths::Paths;

/// How often to check whether the owner allowed chats, after asking.
const ACCESS_POLL: Duration = Duration::from_secs(5);
/// How long to keep checking.
const ACCESS_WAIT: Duration = Duration::from_secs(15 * 60);

pub fn run(paths: Paths) -> Result<()> {
    // Only safe to ask before any thread starts.
    let utc_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let theme = Theme::detect();
    let mut app = App::new(utc_offset);

    let (tx, rx) = mpsc::channel();
    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    spawn_input(tx.clone());
    let chats = ChatRunner::new(Chats::new(&paths.socket_path()), tx.clone());
    let link = daemon::Link::spawn(paths, tx.clone());

    let effects = app.start();
    let mut result = Ok(());
    if run_effects(effects, &link, &chats) {
        result = event_loop(&mut terminal, &mut app, &theme, &rx, &link, &chats);
    }
    chats.follow(None);
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    theme: &Theme,
    rx: &mpsc::Receiver<Msg>,
    link: &daemon::Link,
    chats: &ChatRunner,
) -> Result<()> {
    loop {
        let now = OffsetDateTime::now_utc();
        terminal.draw(|frame| ui::render(frame, app, theme, now))?;
        let first = match app.next_wakeup(now) {
            Some(wait) => match rx.recv_timeout(wait) {
                Ok(msg) => Some(msg),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            },
            None => match rx.recv() {
                Ok(msg) => Some(msg),
                Err(_) => return Ok(()),
            },
        };
        // Apply a burst of messages before drawing again.
        for msg in first.into_iter().chain(rx.try_iter()) {
            let effects = app.update(msg);
            if !run_effects(effects, link, chats) {
                return Ok(());
            }
        }
    }
}

/// False when it's time to quit.
fn run_effects(effects: Vec<Effect>, link: &daemon::Link, chats: &ChatRunner) -> bool {
    for effect in effects {
        match effect {
            Effect::Quit => return false,
            Effect::Daemon(command) => link.send(command),
            Effect::Chat(command) => chats.run(command),
        }
    }
    true
}

fn spawn_input(tx: Sender<Msg>) {
    thread::spawn(move || {
        loop {
            let msg = match event::read() {
                // Windows also reports releases.
                Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => Msg::Key(key),
                Ok(Event::Paste(text)) => Msg::Paste(text),
                Ok(Event::Resize(..)) => Msg::Resize,
                Ok(_) => continue,
                Err(_) => return,
            };
            if tx.send(msg).is_err() {
                return;
            }
        }
    });
}

/// The chat pane's calls to the daemon. Each blocks, so each gets a thread.
struct ChatRunner {
    chats: Chats,
    tx: Sender<Msg>,
    /// The chat followed now: set its flag to stop it, and close its
    /// connection to stop it at once where the platform allows.
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

impl ChatRunner {
    fn new(chats: Chats, tx: Sender<Msg>) -> Self {
        Self {
            chats,
            tx,
            following: Mutex::new(None),
            access_wait: Arc::new(AtomicU64::new(0)),
        }
    }

    fn run(&self, command: ChatCommand) {
        let (chats, tx) = (self.chats.clone(), self.tx.clone());
        let send = move |msg: ChatMsg| {
            let _ = tx.send(Msg::Chat(msg));
        };
        match command {
            ChatCommand::List => {
                thread::spawn(move || send(ChatMsg::Listed(chats.list())));
            }
            ChatCommand::Open(chat) => {
                thread::spawn(move || {
                    send(ChatMsg::Shown {
                        chat,
                        result: chats.show(chat),
                    });
                });
            }
            ChatCommand::Send { chat, text } => {
                thread::spawn(move || {
                    send(ChatMsg::Sent {
                        chat,
                        result: chats.send(chat, &text),
                    });
                });
            }
            ChatCommand::Cancel(chat) => {
                thread::spawn(move || send(ChatMsg::Cancelled(chats.cancel(chat))));
            }
            ChatCommand::RequestAccess => {
                let generation = self.access_wait.fetch_add(1, Ordering::SeqCst) + 1;
                let current = Arc::clone(&self.access_wait);
                thread::spawn(move || {
                    let result = chats.request_access();
                    let waiting = result.as_ref().is_ok_and(|r| !r.granted);
                    send(ChatMsg::Access(result));
                    if waiting {
                        wait_for_access(&chats, &send, || {
                            current.load(Ordering::SeqCst) == generation
                        });
                    }
                });
            }
            ChatCommand::Follow(chat) => self.follow(chat),
            ChatCommand::OpenUrl(url) => {
                crate::browser::open(&url);
            }
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
        let (chats, tx) = (self.chats.clone(), self.tx.clone());
        thread::spawn(move || {
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
                        Some(live) => tx.send(Msg::Chat(ChatMsg::Live { chat, live })).is_ok(),
                        None => true,
                    }
                },
            );
            if let Err(failure) = result
                && !stop.load(Ordering::SeqCst)
            {
                let _ = tx.send(Msg::Chat(ChatMsg::FollowFailed { chat, failure }));
            }
        });
    }
}

/// Check back until the owner answers the request for chats, then tell the
/// pane. Stops early when `current` says a newer wait took over.
fn wait_for_access(chats: &Chats, send: &impl Fn(ChatMsg), current: impl Fn() -> bool) {
    let started = Instant::now();
    while started.elapsed() < ACCESS_WAIT && current() {
        thread::sleep(ACCESS_POLL);
        if !current() {
            return;
        }
        match chats.list() {
            Err(failure) if failure.code == "chat_access_required" => {}
            result => return send(ChatMsg::Listed(result)),
        }
    }
}
