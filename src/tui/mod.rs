//! `cww tui`: the terminal UI.
//!
//! Three kinds of input feed one channel: keys (a thread blocked in
//! crossterm's `event::read`), the daemon (a thread blocked on the control
//! subscription, see [`daemon`]), and chat replies. The loop blocks on that
//! channel and redraws only when something arrives. It sets a timeout only
//! while something on screen moves (a spinner) or a relative time like
//! "2s ago" is about to change, so an idle TUI uses no CPU.

pub mod app;
pub mod chat;
mod daemon;
pub mod theme;
pub mod ui;

#[cfg(test)]
mod tests;

use std::io::stdout;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;

use anyhow::Result;
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use crossterm::execute;
use ratatui::DefaultTerminal;
use time::{OffsetDateTime, UtcOffset};

use self::app::{App, ChatCommand, ChatMsg, Effect, Msg};
use self::chat::{ChatBackend, ChatEvent, NotConnected};
use self::theme::Theme;
use crate::paths::Paths;

pub fn run(paths: Paths) -> Result<()> {
    // Only safe to ask before any thread starts.
    let utc_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let theme = Theme::detect();
    let chat: Arc<dyn ChatBackend> = Arc::new(NotConnected);
    let mut app = App::new(chat.availability(), utc_offset);

    let (tx, rx) = mpsc::channel();
    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    spawn_input(tx.clone());
    let link = daemon::Link::spawn(paths, tx.clone());

    let effects = app.start();
    let mut result = Ok(());
    if run_effects(effects, &link, &chat, &tx) {
        result = event_loop(&mut terminal, &mut app, &theme, &rx, &link, &chat, &tx);
    }
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
    chat: &Arc<dyn ChatBackend>,
    tx: &Sender<Msg>,
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
            if !run_effects(effects, link, chat, tx) {
                return Ok(());
            }
        }
    }
}

/// False when it's time to quit.
fn run_effects(
    effects: Vec<Effect>,
    link: &daemon::Link,
    chat: &Arc<dyn ChatBackend>,
    tx: &Sender<Msg>,
) -> bool {
    for effect in effects {
        match effect {
            Effect::Quit => return false,
            Effect::Daemon(command) => link.send(command),
            Effect::Chat(command) => run_chat(chat, command, tx),
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

/// Chat calls block on the network, so each gets a thread.
fn run_chat(backend: &Arc<dyn ChatBackend>, command: ChatCommand, tx: &Sender<Msg>) {
    let backend = Arc::clone(backend);
    let tx = tx.clone();
    let error = |e: anyhow::Error| format!("{e:#}");
    thread::spawn(move || {
        let send = |msg: ChatMsg| tx.send(Msg::Chat(msg)).is_ok();
        match command {
            ChatCommand::List => {
                send(ChatMsg::Chats(backend.list_chats().map_err(error)));
            }
            ChatCommand::Open(chat) => {
                let result = backend.messages(&chat).map_err(error);
                send(ChatMsg::Messages { chat, result });
            }
            ChatCommand::Send { chat, text } => match backend.send(chat.as_deref(), &text) {
                Ok(stream) => {
                    for event in stream {
                        let event = event.unwrap_or_else(|e| ChatEvent::Error(error(e)));
                        let last = matches!(event, ChatEvent::Done | ChatEvent::Error(_));
                        if !send(ChatMsg::Event(event)) || last {
                            return;
                        }
                    }
                    send(ChatMsg::Event(ChatEvent::Done));
                }
                Err(e) => {
                    send(ChatMsg::Event(ChatEvent::Error(error(e))));
                }
            },
            ChatCommand::Cancel(chat) => {
                if let Err(e) = backend.cancel(&chat) {
                    send(ChatMsg::Event(ChatEvent::Error(error(e))));
                }
            }
        }
    });
}
