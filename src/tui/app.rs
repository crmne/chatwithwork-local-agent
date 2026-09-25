//! The TUI's state and the reducer that changes it.
//!
//! [`App::update`] takes one message (a key, something the daemon said, a
//! chat event) and returns the effects to run. It does no I/O and never reads
//! the clock, so tests can drive it directly.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use super::chat::{Availability, ChatEvent, ChatId, ChatMessage, ChatSummary, Role, ToolState};
use crate::audit::AuditEntry;
use crate::control::PROTOCOL_VERSION;

/// Audit entries kept in memory.
const AUDIT_KEEP: usize = 1000;

/// Shown when the daemon goes away; cleared when it comes back.
const DAEMON_STOPPED: &str = "The daemon stopped.";

const LOOKING: &str = "Looking for the daemon…";

/// One animation frame, while something animates.
pub const FRAME: Duration = Duration::from_millis(120);

/// The daemon's `status`, as CONTROL.md describes it. Unknown fields are
/// ignored and state names stay strings, so a newer daemon still parses.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct DaemonStatus {
    pub protocol: u32,
    pub version: String,
    pub paired: bool,
    pub server: Option<String>,
    pub device_id: Option<String>,
    pub paused: bool,
    pub connection: Link,
    pub roots: Vec<RootState>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Link {
    /// `not_paired`, `connecting`, `connected`, `offline` or `revoked`.
    pub connection: String,
    pub since: Option<String>,
    pub last_error: Option<String>,
}

impl Default for Link {
    fn default() -> Self {
        Self {
            connection: "not_paired".into(),
            since: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct RootState {
    pub id: String,
    pub label: String,
    pub available: bool,
    /// `pending`, `indexing`, `ready`, `error` or `disabled`; empty when the
    /// daemon isn't running.
    pub index: String,
    pub indexed_files: u64,
    pub local_path: String,
}

impl Default for RootState {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            available: true,
            index: String::new(),
            indexed_files: 0,
            local_path: String::new(),
        }
    }
}

/// What `config.toml` says, when the daemon isn't there to ask.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Offline {
    pub paired: bool,
    pub server: Option<String>,
    pub device_id: Option<String>,
    pub paused: bool,
    pub roots: Vec<RootState>,
    /// Why the control channel or the config couldn't be read, if it's more
    /// than "nothing is listening".
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Suggestion {
    pub path: String,
    pub label: String,
    pub exists: bool,
    pub shared: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Daemon {
    /// Not heard from yet.
    Unknown,
    Running(Box<DaemonStatus>),
    NotRunning(Box<Offline>),
}

impl Daemon {
    pub fn roots(&self) -> &[RootState] {
        match self {
            Self::Unknown => &[],
            Self::Running(s) => &s.roots,
            Self::NotRunning(o) => &o.roots,
        }
    }

    pub fn paused(&self) -> Option<bool> {
        match self {
            Self::Unknown => None,
            Self::Running(s) => Some(s.paused),
            Self::NotRunning(o) => Some(o.paused),
        }
    }

    pub fn server(&self) -> Option<&str> {
        match self {
            Self::Unknown => None,
            Self::Running(s) => s.server.as_deref(),
            Self::NotRunning(o) => o.server.as_deref(),
        }
    }

    /// The tunnel's state, when the daemon is running.
    pub fn connection(&self) -> Option<&str> {
        match self {
            Self::Running(s) => Some(&s.connection.connection),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Chat,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Chats,
    Roots,
    Composer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    AddRoot {
        input: String,
    },
    ConfirmRemove {
        id: String,
        label: String,
        path: String,
    },
    /// The daemon refused a folder as too broad; ask before `i_know`.
    ConfirmBroad {
        path: String,
        reason: String,
    },
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub signal: super::theme::Signal,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonCommand {
    Pause,
    Resume,
    AddRoot {
        path: String,
        label: Option<String>,
        i_know: bool,
    },
    RemoveRoot {
        id: String,
    },
    /// Look for the daemon again now.
    Retry,
    /// Pair this computer (the device flow), from inside the TUI.
    Pair,
    /// Stop waiting for the pairing to be approved.
    CancelPairing,
    /// `cww daemon install`: register and start the background service.
    InstallService,
}

/// A pairing started from the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pairing {
    /// Asking the server for a code.
    Starting,
    /// The user has to approve this code on the server.
    Waiting {
        code: String,
        url: String,
        name: String,
        fingerprint: String,
        /// The page was opened in the browser.
        opened: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingMsg {
    Code {
        code: String,
        url: String,
        name: String,
        fingerprint: String,
        opened: bool,
    },
    /// Paired with this server, or why not.
    Finished(Result<String, String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatCommand {
    List,
    Open(ChatId),
    Send { chat: Option<ChatId>, text: String },
    Cancel(ChatId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Daemon(DaemonCommand),
    Chat(ChatCommand),
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DaemonMsg {
    Status(Box<DaemonStatus>),
    NotRunning {
        offline: Box<Offline>,
        audit: Vec<AuditEntry>,
        suggestion: Option<Suggestion>,
    },
    Audit(Box<AuditEntry>),
    AuditHistory(Vec<AuditEntry>),
    Suggestions(Vec<Suggestion>),
    /// The subscription ended: the daemon stopped.
    Lost,
    Pairing(PairingMsg),
    /// A command finished, with a sentence for the person or an error.
    Done {
        command: DaemonCommand,
        result: Result<String, String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChatMsg {
    Chats(Result<Vec<ChatSummary>, String>),
    Messages {
        chat: ChatId,
        result: Result<Vec<ChatMessage>, String>,
    },
    Event(ChatEvent),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Resize,
    Daemon(DaemonMsg),
    Chat(ChatMsg),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatPane {
    pub availability: Availability,
    pub chats: Vec<ChatSummary>,
    pub selected: usize,
    pub open: Option<ChatId>,
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub streaming: bool,
    pub error: Option<String>,
}

impl ChatPane {
    pub fn available(&self) -> bool {
        self.availability == Availability::Available
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub daemon: Daemon,
    /// Oldest first.
    pub audit: Vec<AuditEntry>,
    pub suggestions: Vec<Suggestion>,
    pub view: View,
    pub focus: Focus,
    pub selected_root: usize,
    /// Log lines scrolled up from the newest.
    pub log_scroll: usize,
    pub modal: Option<Modal>,
    pub notice: Option<Notice>,
    pub pairing: Option<Pairing>,
    /// The first-run offer was answered.
    pub offer_dismissed: bool,
    pub chat: ChatPane,
    /// For showing times in local time.
    pub utc_offset: UtcOffset,
    pub quit: bool,
}

impl App {
    pub fn new(availability: Availability, utc_offset: UtcOffset) -> Self {
        Self {
            daemon: Daemon::Unknown,
            audit: Vec::new(),
            suggestions: Vec::new(),
            view: View::Chat,
            focus: Focus::Roots,
            selected_root: 0,
            log_scroll: 0,
            modal: None,
            notice: None,
            pairing: None,
            offer_dismissed: false,
            chat: ChatPane {
                availability,
                chats: Vec::new(),
                selected: 0,
                open: None,
                messages: Vec::new(),
                input: String::new(),
                streaming: false,
                error: None,
            },
            utc_offset,
            quit: false,
        }
    }

    /// Effects to run once, at start.
    pub fn start(&self) -> Vec<Effect> {
        if self.chat.available() {
            vec![Effect::Chat(ChatCommand::List)]
        } else {
            Vec::new()
        }
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => self.key(key),
            Msg::Paste(text) => {
                self.paste(&text);
                Vec::new()
            }
            Msg::Resize => Vec::new(),
            Msg::Daemon(msg) => self.daemon_msg(msg),
            Msg::Chat(msg) => {
                self.chat_msg(msg);
                Vec::new()
            }
        }
    }

    /// The first-run offer to share Documents, when it applies: nothing is
    /// shared, the folder exists, and the person hasn't answered yet.
    pub fn offer(&self) -> Option<&Suggestion> {
        if self.offer_dismissed
            || self.view != View::Chat
            || matches!(self.daemon, Daemon::Unknown)
            || !self.daemon.roots().is_empty()
        {
            return None;
        }
        self.suggestions.iter().find(|s| s.exists)
    }

    /// The folder to prefill when adding one.
    fn default_folder(&self) -> String {
        self.suggestions
            .iter()
            .find(|s| s.exists && !self.daemon.roots().iter().any(|r| r.local_path == s.path))
            .map(|s| s.path.clone())
            .unwrap_or_default()
    }

    /// Focus targets, in Tab order.
    pub fn focus_order(&self) -> Vec<Focus> {
        if self.chat.available() {
            vec![Focus::Chats, Focus::Roots, Focus::Composer]
        } else {
            vec![Focus::Roots]
        }
    }

    /// Whether something on screen moves, so the loop needs frames.
    pub fn animating(&self) -> bool {
        let indexing = matches!(self.daemon, Daemon::Running(_))
            && self
                .daemon
                .roots()
                .iter()
                .any(|r| r.index == "indexing" || r.index == "pending");
        indexing || self.daemon.connection() == Some("connecting") || self.chat.streaming
    }

    /// How long the loop may sleep before the screen goes stale: a frame
    /// while animating, otherwise until a relative time like "2s ago"
    /// changes, and forever when nothing on screen depends on the clock.
    pub fn next_wakeup(&self, now: OffsetDateTime) -> Option<Duration> {
        if self.animating() {
            return Some(FRAME);
        }
        if self.view != View::Chat {
            return None;
        }
        let ts = parse_ts(&self.audit.last()?.ts)?;
        let age = (now - ts).whole_milliseconds().max(0) as u64;
        let unit = match age {
            0..60_000 => 1_000,
            60_000..3_600_000 => 60_000,
            _ => return None,
        };
        Some(Duration::from_millis(unit - age % unit))
    }

    fn key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return vec![Effect::Quit];
        }
        self.notice = None;
        if let Some(modal) = self.modal.take() {
            return self.modal_key(modal, key);
        }
        if self.focus == Focus::Composer {
            return self.composer_key(key);
        }
        let offer = self.offer().map(|s| (s.path.clone(), s.label.clone()));
        match key.code {
            KeyCode::Char('q') => {
                self.quit = true;
                return vec![Effect::Quit];
            }
            KeyCode::Tab => self.cycle_focus(true),
            KeyCode::BackTab => self.cycle_focus(false),
            KeyCode::Char('l') => {
                self.view = match self.view {
                    View::Chat => View::Log,
                    View::Log => View::Chat,
                };
                self.log_scroll = 0;
            }
            KeyCode::Esc if self.pairing.is_some() => {
                self.pairing = None;
                return vec![Effect::Daemon(DaemonCommand::CancelPairing)];
            }
            KeyCode::Esc if self.view == View::Log => self.view = View::Chat,
            KeyCode::Char('c') if self.can_pair() && self.pairing.is_none() => {
                self.pairing = Some(Pairing::Starting);
                return vec![Effect::Daemon(DaemonCommand::Pair)];
            }
            KeyCode::Char('s') if matches!(self.daemon, Daemon::NotRunning(_)) => {
                self.notice(super::theme::Signal::Idle, "Starting the daemon…");
                return vec![Effect::Daemon(DaemonCommand::InstallService)];
            }
            KeyCode::Char('p') => return self.toggle_pause(),
            KeyCode::Char('a') => {
                self.modal = Some(Modal::AddRoot {
                    input: self.default_folder(),
                });
            }
            KeyCode::Char('d') | KeyCode::Char('x') | KeyCode::Delete => self.confirm_remove(),
            KeyCode::Char('y') => {
                if let Some((path, label)) = offer {
                    self.offer_dismissed = true;
                    return self.add_root(path, Some(label), false);
                }
            }
            KeyCode::Char('n') if offer.is_some() => self.offer_dismissed = true,
            KeyCode::Char('r') => {
                if matches!(self.daemon, Daemon::NotRunning(_)) {
                    self.notice(super::theme::Signal::Idle, LOOKING);
                }
                return vec![Effect::Daemon(DaemonCommand::Retry)];
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.scroll_log(10),
            KeyCode::PageDown => self.scroll_log(-10),
            KeyCode::Home if self.view == View::Log => self.log_scroll = self.audit.len(),
            KeyCode::End | KeyCode::Char('G') if self.view == View::Log => self.log_scroll = 0,
            KeyCode::Enter if self.focus == Focus::Chats && self.chat.available() => {
                if let Some(chat) = self.chat.chats.get(self.chat.selected) {
                    let id = chat.id.clone();
                    self.chat.open = Some(id.clone());
                    self.chat.messages.clear();
                    self.view = View::Chat;
                    self.focus = Focus::Composer;
                    return vec![Effect::Chat(ChatCommand::Open(id))];
                }
            }
            KeyCode::Char('i') if self.chat.available() => self.focus = Focus::Composer,
            _ => {}
        }
        Vec::new()
    }

    fn modal_key(&mut self, modal: Modal, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match modal {
            Modal::AddRoot { mut input } => {
                match key.code {
                    KeyCode::Esc => return Vec::new(),
                    KeyCode::Enter => {
                        let path = input.trim().to_string();
                        if !path.is_empty() {
                            return self.add_root(path, None, false);
                        }
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char('u') if ctrl => input.clear(),
                    KeyCode::Char('w') if ctrl => delete_word(&mut input),
                    KeyCode::Char(c) if !ctrl => input.push(c),
                    _ => {}
                }
                self.modal = Some(Modal::AddRoot { input });
            }
            Modal::ConfirmRemove { id, label, path } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.notice(
                        super::theme::Signal::Idle,
                        &format!("Stopping sharing {label}…"),
                    );
                    return vec![Effect::Daemon(DaemonCommand::RemoveRoot { id })];
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.modal = Some(Modal::ConfirmRemove { id, label, path }),
            },
            Modal::ConfirmBroad { path, reason } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return self.add_root(path, None, true),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.modal = Some(Modal::ConfirmBroad { path, reason }),
            },
            Modal::Help => {}
        }
        Vec::new()
    }

    fn composer_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc if self.chat.streaming => {
                if let Some(chat) = self.chat.open.clone() {
                    return vec![Effect::Chat(ChatCommand::Cancel(chat))];
                }
            }
            KeyCode::Esc => self.focus = Focus::Roots,
            KeyCode::Tab => self.cycle_focus(true),
            KeyCode::BackTab => self.cycle_focus(false),
            KeyCode::Enter => return self.send(),
            KeyCode::Backspace => {
                self.chat.input.pop();
            }
            KeyCode::Char('u') if ctrl => self.chat.input.clear(),
            KeyCode::Char('w') if ctrl => delete_word(&mut self.chat.input),
            KeyCode::Char(c) if !ctrl => self.chat.input.push(c),
            _ => {}
        }
        Vec::new()
    }

    fn send(&mut self) -> Vec<Effect> {
        let text = self.chat.input.trim().to_string();
        if text.is_empty() || self.chat.streaming || !self.chat.available() {
            return Vec::new();
        }
        self.chat.input.clear();
        self.chat.error = None;
        self.chat.streaming = true;
        self.chat.messages.push(ChatMessage {
            id: String::new(),
            role: Role::User,
            text: text.clone(),
            tools: Vec::new(),
        });
        vec![Effect::Chat(ChatCommand::Send {
            chat: self.chat.open.clone(),
            text,
        })]
    }

    fn paste(&mut self, text: &str) {
        let text = text.replace(['\r', '\n'], " ");
        match &mut self.modal {
            Some(Modal::AddRoot { input }) => input.push_str(text.trim()),
            Some(_) => {}
            None if self.focus == Focus::Composer => self.chat.input.push_str(&text),
            None => {}
        }
    }

    fn cycle_focus(&mut self, forward: bool) {
        let order = self.focus_order();
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if forward {
            (at + 1) % order.len()
        } else {
            (at + order.len() - 1) % order.len()
        };
        self.focus = order[next];
    }

    fn move_selection(&mut self, delta: isize) {
        if self.view == View::Log {
            return self.scroll_log(-delta * 3);
        }
        let (selected, len) = match self.focus {
            Focus::Chats => (&mut self.chat.selected, self.chat.chats.len()),
            _ => (&mut self.selected_root, self.daemon.roots().len()),
        };
        if len > 0 {
            *selected = selected.saturating_add_signed(delta).min(len - 1);
        }
    }

    fn scroll_log(&mut self, delta: isize) {
        if self.view == View::Log {
            self.log_scroll = self
                .log_scroll
                .saturating_add_signed(delta)
                .min(self.audit.len().saturating_sub(1));
        }
    }

    fn toggle_pause(&mut self) -> Vec<Effect> {
        match self.daemon.paused() {
            Some(true) => {
                self.notice(super::theme::Signal::Idle, "Resuming…");
                vec![Effect::Daemon(DaemonCommand::Resume)]
            }
            Some(false) => {
                self.notice(super::theme::Signal::Idle, "Pausing…");
                vec![Effect::Daemon(DaemonCommand::Pause)]
            }
            None => {
                self.notice(super::theme::Signal::Idle, "Still looking for the daemon.");
                Vec::new()
            }
        }
    }

    fn confirm_remove(&mut self) {
        match self.daemon.roots().get(self.selected_root) {
            Some(root) => {
                self.modal = Some(Modal::ConfirmRemove {
                    id: root.id.clone(),
                    label: root.label.clone(),
                    path: root.local_path.clone(),
                });
            }
            None => self.notice(super::theme::Signal::Idle, "Nothing is shared."),
        }
    }

    fn add_root(&mut self, path: String, label: Option<String>, i_know: bool) -> Vec<Effect> {
        self.notice(super::theme::Signal::Idle, &format!("Sharing {path}…"));
        vec![Effect::Daemon(DaemonCommand::AddRoot {
            path,
            label,
            i_know,
        })]
    }

    /// Whether this computer needs pairing: not paired yet, or revoked.
    pub fn can_pair(&self) -> bool {
        match &self.daemon {
            Daemon::Running(status) => {
                matches!(
                    status.connection.connection.as_str(),
                    "not_paired" | "revoked"
                )
            }
            Daemon::NotRunning(offline) => !offline.paired,
            Daemon::Unknown => false,
        }
    }

    fn notice(&mut self, signal: super::theme::Signal, text: &str) {
        self.notice = Some(Notice {
            signal,
            text: text.to_string(),
        });
    }

    fn daemon_msg(&mut self, msg: DaemonMsg) -> Vec<Effect> {
        let mut effects = Vec::new();
        use super::theme::Signal;
        if matches!(msg, DaemonMsg::Status(_) | DaemonMsg::NotRunning { .. })
            && self.notice.as_ref().is_some_and(|n| n.text == LOOKING)
        {
            self.notice = None;
        }
        match msg {
            DaemonMsg::Status(status) => {
                // Back after a restart (a new sandbox, an upgrade): the
                // "stopped" notice no longer holds.
                if self
                    .notice
                    .as_ref()
                    .is_some_and(|n| n.text == DAEMON_STOPPED)
                {
                    self.notice = None;
                }
                if status.protocol > PROTOCOL_VERSION {
                    self.notice(
                        Signal::Attention,
                        &format!(
                            "The daemon speaks control protocol {}; this cww knows {PROTOCOL_VERSION}. \
                             Update cww.",
                            status.protocol
                        ),
                    );
                }
                self.daemon = Daemon::Running(status);
            }
            DaemonMsg::NotRunning {
                offline,
                audit,
                suggestion,
            } => {
                self.daemon = Daemon::NotRunning(offline);
                self.audit = audit;
                self.suggestions = suggestion.into_iter().collect();
            }
            DaemonMsg::Audit(entry) => {
                self.audit.push(*entry);
                if self.log_scroll > 0 {
                    // Keep the same lines in view while scrolled up.
                    self.log_scroll += 1;
                }
            }
            DaemonMsg::AuditHistory(history) => {
                // Entries that arrived live before the history came in may
                // be in it already.
                let live: Vec<AuditEntry> = std::mem::take(&mut self.audit)
                    .into_iter()
                    .filter(|e| !history.contains(e))
                    .collect();
                self.audit = history;
                self.audit.extend(live);
            }
            DaemonMsg::Suggestions(suggestions) => self.suggestions = suggestions,
            DaemonMsg::Lost => {
                self.daemon = Daemon::Unknown;
                self.notice(Signal::Attention, DAEMON_STOPPED);
            }
            DaemonMsg::Pairing(PairingMsg::Code {
                code,
                url,
                name,
                fingerprint,
                opened,
            }) => {
                if self.pairing.is_some() {
                    self.pairing = Some(Pairing::Waiting {
                        code,
                        url,
                        name,
                        fingerprint,
                        opened,
                    });
                }
            }
            DaemonMsg::Pairing(PairingMsg::Finished(result)) => {
                let cancelled = self.pairing.is_none();
                self.pairing = None;
                match result {
                    Ok(server) => {
                        self.notice(Signal::Positive, &format!("Paired with {server}."));
                        effects.push(Effect::Daemon(DaemonCommand::Retry));
                    }
                    Err(_) if cancelled => {}
                    Err(e) => self.notice(Signal::Negative, &capitalize(&e)),
                }
            }
            DaemonMsg::Done {
                command: DaemonCommand::InstallService,
                result,
            } => {
                match result {
                    Ok(text) => self.notice(Signal::Positive, &text),
                    Err(e) => self.notice(Signal::Negative, &e),
                }
                effects.push(Effect::Daemon(DaemonCommand::Retry));
            }
            DaemonMsg::Done { command, result } => match (command, result) {
                (_, Ok(text)) if !text.is_empty() => self.notice(Signal::Positive, &text),
                (_, Ok(_)) => self.notice = None,
                (
                    DaemonCommand::AddRoot {
                        path,
                        i_know: false,
                        ..
                    },
                    Err(e),
                ) if e.contains("--i-know") => {
                    // "refusing to share <path>: <reason>. Share a narrower
                    // folder, or pass --i-know ..." (see roots.rs).
                    let reason = e.split(". Share a narrower").next().unwrap_or(&e);
                    let reason = reason
                        .split_once(": ")
                        .filter(|(head, _)| head.contains("refusing to share"))
                        .map_or(reason, |(_, r)| r);
                    self.notice = None;
                    self.modal = Some(Modal::ConfirmBroad {
                        path,
                        reason: capitalize(reason),
                    });
                }
                (_, Err(e)) => self.notice(Signal::Negative, &e),
            },
        }
        if self.audit.len() > AUDIT_KEEP {
            self.audit.drain(..self.audit.len() - AUDIT_KEEP);
        }
        let roots = self.daemon.roots().len();
        self.selected_root = self.selected_root.min(roots.saturating_sub(1));
        effects
    }

    fn chat_msg(&mut self, msg: ChatMsg) {
        match msg {
            ChatMsg::Chats(Ok(chats)) => {
                self.chat.selected = self.chat.selected.min(chats.len().saturating_sub(1));
                self.chat.chats = chats;
            }
            ChatMsg::Chats(Err(e)) => self.chat.error = Some(e),
            ChatMsg::Messages { chat, result } => {
                if self.chat.open.as_ref() != Some(&chat) {
                    return;
                }
                match result {
                    Ok(messages) => self.chat.messages = messages,
                    Err(e) => self.chat.error = Some(e),
                }
            }
            ChatMsg::Event(event) => self.chat_event(event),
        }
    }

    fn chat_event(&mut self, event: ChatEvent) {
        let pane = &mut self.chat;
        match event {
            ChatEvent::Started {
                chat_id,
                message_id,
            } => {
                pane.open = Some(chat_id);
                pane.messages.push(ChatMessage {
                    id: message_id,
                    role: Role::Assistant,
                    text: String::new(),
                    tools: Vec::new(),
                });
            }
            ChatEvent::TextDelta(text) => {
                if let Some(message) = last_answer(pane) {
                    message.text.push_str(&text);
                }
            }
            ChatEvent::Tool(step) => {
                if let Some(message) = last_answer(pane) {
                    match message.tools.iter_mut().find(|s| s.id == step.id) {
                        Some(existing) => *existing = step,
                        None => message.tools.push(step),
                    }
                }
            }
            ChatEvent::Done => pane.streaming = false,
            ChatEvent::Error(e) => {
                pane.streaming = false;
                pane.error = Some(e);
                if let Some(message) = last_answer(pane) {
                    for step in &mut message.tools {
                        if step.state == ToolState::Running {
                            step.state = ToolState::Failed;
                        }
                    }
                }
            }
        }
    }
}

fn last_answer(pane: &mut ChatPane) -> Option<&mut ChatMessage> {
    pane.messages
        .last_mut()
        .filter(|m| m.role == Role::Assistant)
}

fn delete_word(input: &mut String) {
    let trimmed = input.trim_end_matches([' ', '/', '\\']);
    let cut = trimmed.rfind([' ', '/', '\\']).map_or(0, |i| i + 1);
    input.truncate(cut);
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

pub fn parse_ts(ts: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(ts, &Rfc3339).ok()
}
