//! The TUI's state and the reducer that changes it.
//!
//! [`App::update`] takes one message (a key, something the daemon said, a
//! chat event) and returns the effects to run. It does no I/O and never reads
//! the clock, so tests can drive it directly.

use std::collections::BTreeMap;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use super::chat::{AccessRequest, ChatList, ChatSummary, Entry, Failure, Live, Transcript};
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
    /// The proxy the daemon reaches the server through, without its password.
    pub proxy: Option<crate::proxy::ProxyInfo>,
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
    /// Read the list of chats.
    List,
    /// Read one chat as a transcript.
    Open(u64),
    /// Follow one chat live, or stop following.
    Follow(Option<u64>),
    /// Ask in a chat, or in a new one.
    Send {
        chat: Option<u64>,
        text: String,
    },
    Cancel(u64),
    /// Ask the owner to allow chats, then check back until they answer.
    RequestAccess,
    /// Check back until the owner answers a request made earlier.
    WaitForAccess,
    /// Open a page of the paired server in the browser.
    OpenUrl(String),
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
    Listed(Result<ChatList, Failure>),
    Shown {
        chat: u64,
        result: Result<Transcript, Failure>,
    },
    Sent {
        chat: Option<u64>,
        result: Result<ChatSummary, Failure>,
    },
    Cancelled(Result<(), Failure>),
    Access(Result<AccessRequest, Failure>),
    Live {
        chat: u64,
        live: Live,
    },
    /// Following a chat stopped with an error.
    FollowFailed {
        chat: u64,
        failure: Failure,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Resize,
    Daemon(DaemonMsg),
    Chat(ChatMsg),
}

/// Whether chats can be used here, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// Waiting for a running, paired daemon.
    Unknown,
    Loading,
    Ready,
    /// The owner hasn't allowed this computer to use their chats yet.
    NeedsApproval {
        /// They were asked, in Settings.
        requested: bool,
        url: Option<String>,
    },
    /// Chats can't be used here right now, for this reason.
    Unavailable(Failure),
}

/// How the open chat's live updates are doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Following {
    No,
    Starting,
    Live,
    /// The daemon lost Chat with Work for a moment and keeps trying.
    Offline,
    /// The server refused to follow this chat.
    Refused,
    /// The daemon's connection to the server can't follow chats.
    Unsupported,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatPane {
    pub access: Access,
    pub list: ChatList,
    /// 0 is "New chat", then the chats the search leaves, in order.
    pub selected: usize,
    /// What's typed after `/`, while searching.
    pub search: Option<String>,
    /// The open chat; `None` is a new one.
    pub open: Option<u64>,
    pub transcript: Option<Transcript>,
    /// A read of the open chat is under way; `stale` asks for one more.
    pub loading: bool,
    pub stale: bool,
    pub following: Following,
    /// Answer text streamed in and not read back yet, by message.
    pub streamed: BTreeMap<u64, String>,
    /// What the running step reports doing.
    pub progress: Option<String>,
    /// The question just sent, until the chat shows it.
    pub pending_question: Option<String>,
    pub input: String,
    pub sending: bool,
    pub stopping: bool,
    pub error: Option<String>,
    /// Transcript lines scrolled up from the newest.
    pub scroll: usize,
    /// Show every tool step under its activity line.
    pub show_steps: bool,
}

impl Default for ChatPane {
    fn default() -> Self {
        Self {
            access: Access::Unknown,
            list: ChatList::default(),
            selected: 0,
            search: None,
            open: None,
            transcript: None,
            loading: false,
            stale: false,
            following: Following::No,
            streamed: BTreeMap::new(),
            progress: None,
            pending_question: None,
            input: String::new(),
            sending: false,
            stopping: false,
            error: None,
            scroll: 0,
            show_steps: false,
        }
    }
}

impl ChatPane {
    pub fn ready(&self) -> bool {
        self.access == Access::Ready
    }

    /// The chats the search leaves, newest first.
    pub fn visible(&self) -> Vec<&ChatSummary> {
        let query = self
            .search
            .as_deref()
            .map(|q| q.trim().trim_start_matches('#').to_lowercase())
            .filter(|q| !q.is_empty());
        match query {
            None => self.list.chats.iter().collect(),
            Some(query) => self
                .list
                .chats
                .iter()
                .filter(|chat| {
                    chat.title.to_lowercase().contains(&query)
                        || chat.number.to_string() == query
                        || chat
                            .project
                            .as_ref()
                            .is_some_and(|p| p.name.to_lowercase().contains(&query))
                })
                .collect(),
        }
    }

    /// The selected chat, unless "New chat" is selected.
    pub fn selected_chat(&self) -> Option<&ChatSummary> {
        let index = self.selected.checked_sub(1)?;
        self.visible().get(index).copied()
    }

    /// The open chat, as last read or listed.
    pub fn open_summary(&self) -> Option<&ChatSummary> {
        let number = self.open?;
        self.transcript
            .as_ref()
            .map(|t| &t.chat)
            .filter(|chat| chat.number == number)
            .or_else(|| self.list.chats.iter().find(|chat| chat.number == number))
    }

    /// An answer is on its way in the open chat.
    pub fn working(&self) -> bool {
        self.sending || self.open_summary().is_some_and(ChatSummary::processing)
    }

    /// Why a question can't be asked right now, as the composer says it.
    pub fn locked_reason(&self) -> Option<&str> {
        if self.open.is_some() {
            self.transcript
                .as_ref()
                .and_then(|t| t.locked_reason.as_deref())
        } else {
            self.list.locked_reason.as_deref()
        }
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
    pub fn new(utc_offset: UtcOffset) -> Self {
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
            chat: ChatPane::default(),
            utc_offset,
            quit: false,
        }
    }

    /// Effects to run once, at start. Chats wait for the daemon's status.
    pub fn start(&self) -> Vec<Effect> {
        Vec::new()
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
            Msg::Chat(msg) => self.chat_msg(msg),
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
        if self.chat.ready() {
            vec![Focus::Chats, Focus::Composer, Focus::Roots]
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
        let chat = self.view == View::Chat
            && (self.chat.working() || (self.chat.loading && self.chat.transcript.is_none()));
        indexing || self.daemon.connection() == Some("connecting") || chat
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
        if self.focus == Focus::Chats && self.chat.search.is_some() {
            return self.search_key(key);
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
            KeyCode::Char('n') if self.chat.ready() => return self.new_chat(),
            KeyCode::Char('r') => {
                if matches!(self.daemon, Daemon::NotRunning(_)) {
                    self.notice(super::theme::Signal::Idle, LOOKING);
                }
                let mut effects = vec![Effect::Daemon(DaemonCommand::Retry)];
                if self.paired() && !matches!(self.chat.access, Access::Loading) {
                    if !self.chat.ready() {
                        self.chat.access = Access::Loading;
                    }
                    effects.push(Effect::Chat(ChatCommand::List));
                    if let Some(open) = self.chat.open {
                        if matches!(
                            self.chat.following,
                            Following::Refused | Following::Unsupported
                        ) {
                            self.chat.following = Following::Starting;
                            effects.push(Effect::Chat(ChatCommand::Follow(Some(open))));
                        }
                        effects.extend(self.refresh_chat(open));
                    }
                }
                return effects;
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('/') if self.focus == Focus::Chats && self.chat.ready() => {
                self.chat.search = Some(String::new());
                self.chat.selected = usize::from(!self.chat.list.chats.is_empty());
            }
            KeyCode::Char('o') => return self.open_in_browser(),
            KeyCode::Char('e') if self.view == View::Chat => {
                self.chat.show_steps = !self.chat.show_steps;
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp if self.view == View::Chat => self.scroll_transcript(10),
            KeyCode::PageDown if self.view == View::Chat => self.scroll_transcript(-10),
            KeyCode::PageUp => self.scroll_log(10),
            KeyCode::PageDown => self.scroll_log(-10),
            KeyCode::Home if self.view == View::Log => self.log_scroll = self.audit.len(),
            KeyCode::End | KeyCode::Char('G') if self.view == View::Log => self.log_scroll = 0,
            KeyCode::End if self.view == View::Chat => self.chat.scroll = 0,
            KeyCode::Enter if self.focus == Focus::Chats && self.chat.ready() => {
                return self.open_selected();
            }
            KeyCode::Char('i') if self.chat.ready() => self.focus = Focus::Composer,
            _ => {}
        }
        Vec::new()
    }

    /// Typing after `/`: the list narrows as you type.
    fn search_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.chat.search = None,
            KeyCode::Enter => {
                let chosen = self.chat.selected_chat().map(|c| c.number);
                self.chat.search = None;
                return match chosen {
                    Some(number) => {
                        self.select_chat(number);
                        self.open_chat(number)
                    }
                    None => self.new_chat(),
                };
            }
            KeyCode::Backspace => {
                if self.chat.search.get_or_insert_default().pop().is_none() {
                    self.chat.search = None;
                }
            }
            KeyCode::Char('u') if ctrl => self.chat.search = Some(String::new()),
            KeyCode::Char(c) if !ctrl => self.chat.search.get_or_insert_default().push(c),
            KeyCode::Up => return self.move_and(-1),
            KeyCode::Down => return self.move_and(1),
            KeyCode::Tab => self.cycle_focus(true),
            KeyCode::BackTab => self.cycle_focus(false),
            _ => return Vec::new(),
        }
        // The best match leads while typing.
        if matches!(key.code, KeyCode::Char(_) | KeyCode::Backspace) {
            self.chat.selected = usize::from(!self.chat.visible().is_empty());
        }
        let len = self.chat.visible().len();
        self.chat.selected = self.chat.selected.min(len);
        Vec::new()
    }

    fn move_and(&mut self, delta: isize) -> Vec<Effect> {
        self.move_selection(delta);
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
            // Esc stops an answer being written, like the web's stop button.
            KeyCode::Esc if self.chat.working() && !self.chat.stopping => {
                if let Some(chat) = self.chat.open {
                    self.chat.stopping = true;
                    self.notice(super::theme::Signal::Idle, "Stopping the answer…");
                    return vec![Effect::Chat(ChatCommand::Cancel(chat))];
                }
            }
            KeyCode::Esc => {
                self.focus = if self.chat.ready() {
                    Focus::Chats
                } else {
                    Focus::Roots
                };
            }
            KeyCode::Tab => self.cycle_focus(true),
            KeyCode::BackTab => self.cycle_focus(false),
            KeyCode::Enter => return self.send(),
            KeyCode::PageUp => self.scroll_transcript(10),
            KeyCode::PageDown => self.scroll_transcript(-10),
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
        if text.is_empty() || !self.chat.ready() || self.chat.working() {
            return Vec::new();
        }
        if let Some(reason) = self.chat.locked_reason().map(str::to_string) {
            self.notice(super::theme::Signal::Attention, &reason);
            return Vec::new();
        }
        self.chat.input.clear();
        self.chat.error = None;
        self.chat.sending = true;
        self.chat.pending_question = Some(text.clone());
        self.chat.scroll = 0;
        vec![Effect::Chat(ChatCommand::Send {
            chat: self.chat.open,
            text,
        })]
    }

    fn paste(&mut self, text: &str) {
        let text = text.replace(['\r', '\n'], " ");
        match &mut self.modal {
            Some(Modal::AddRoot { input }) => input.push_str(text.trim()),
            Some(_) => {}
            None if self.focus == Focus::Composer => self.chat.input.push_str(&text),
            None if self.focus == Focus::Chats && self.chat.search.is_some() => {
                self.chat
                    .search
                    .get_or_insert_default()
                    .push_str(text.trim());
            }
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
        // "New chat" comes first.
        let chats = self.chat.visible().len() + 1;
        let (selected, len) = match self.focus {
            Focus::Chats => (&mut self.chat.selected, chats),
            _ => (&mut self.selected_root, self.daemon.roots().len()),
        };
        if len > 0 {
            *selected = selected.saturating_add_signed(delta).min(len - 1);
        }
    }

    fn scroll_transcript(&mut self, delta: isize) {
        self.chat.scroll = self.chat.scroll.saturating_add_signed(delta).min(10_000);
    }

    fn open_selected(&mut self) -> Vec<Effect> {
        match self.chat.selected_chat().map(|c| c.number) {
            Some(number) => self.open_chat(number),
            None => self.new_chat(),
        }
    }

    /// Show a chat, read it, and follow it live.
    fn open_chat(&mut self, number: u64) -> Vec<Effect> {
        self.view = View::Chat;
        self.focus = Focus::Composer;
        if self.chat.open == Some(number) && self.chat.transcript.is_some() {
            return Vec::new();
        }
        let chat = &mut self.chat;
        chat.open = Some(number);
        chat.transcript = None;
        chat.streamed.clear();
        chat.progress = None;
        chat.pending_question = None;
        chat.error = None;
        chat.scroll = 0;
        chat.stopping = false;
        chat.loading = true;
        chat.stale = false;
        chat.following = Following::Starting;
        vec![
            Effect::Chat(ChatCommand::Open(number)),
            Effect::Chat(ChatCommand::Follow(Some(number))),
        ]
    }

    /// An empty conversation: the first question starts the chat.
    fn new_chat(&mut self) -> Vec<Effect> {
        let followed = self.chat.open.is_some();
        let chat = &mut self.chat;
        chat.open = None;
        chat.transcript = None;
        chat.streamed.clear();
        chat.progress = None;
        chat.pending_question = None;
        chat.error = None;
        chat.scroll = 0;
        chat.loading = false;
        chat.stale = false;
        chat.stopping = false;
        chat.following = Following::No;
        chat.selected = 0;
        self.view = View::Chat;
        self.focus = Focus::Composer;
        if followed {
            vec![Effect::Chat(ChatCommand::Follow(None))]
        } else {
            Vec::new()
        }
    }

    /// Point the sidebar at `number`, outside any search.
    fn select_chat(&mut self, number: u64) {
        if let Some(i) = self.chat.visible().iter().position(|c| c.number == number) {
            self.chat.selected = i + 1;
        }
    }

    /// Read the open chat again, or once more after the read under way.
    fn refresh_chat(&mut self, number: u64) -> Vec<Effect> {
        if self.chat.open != Some(number) {
            Vec::new()
        } else if self.chat.loading {
            self.chat.stale = true;
            Vec::new()
        } else {
            self.chat.loading = true;
            vec![Effect::Chat(ChatCommand::Open(number))]
        }
    }

    /// `o`: the page that helps right now, in the browser. Asking for chat
    /// access comes first when nobody asked yet.
    fn open_in_browser(&mut self) -> Vec<Effect> {
        let url = match &self.chat.access {
            Access::NeedsApproval {
                requested: false, ..
            } => {
                self.notice(super::theme::Signal::Idle, "Asking to use your chats…");
                return vec![Effect::Chat(ChatCommand::RequestAccess)];
            }
            Access::NeedsApproval { url, .. } => url.clone(),
            Access::Ready => self
                .chat
                .open_summary()
                .or_else(|| self.chat.selected_chat())
                .map(|c| c.url.clone()),
            _ => None,
        };
        match url.filter(|url| self.on_server(url)) {
            Some(url) => vec![Effect::Chat(ChatCommand::OpenUrl(url))],
            None => Vec::new(),
        }
    }

    /// Whether `url` is a page of the paired server, the only kind the TUI
    /// opens, so a server can't send the browser anywhere else.
    fn on_server(&self, url: &str) -> bool {
        self.daemon.server().is_some_and(|server| {
            let origin = format!("{}/", server.trim_end_matches('/'));
            url.starts_with(&origin)
        })
    }

    /// A running daemon, paired and not revoked: chats can be asked for.
    fn paired(&self) -> bool {
        match &self.daemon {
            Daemon::Running(status) => status.paired && status.connection.connection != "revoked",
            _ => false,
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
        let was_connected = self.daemon.connection() == Some("connected");
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
        effects.extend(self.load_chats(was_connected));
        effects
    }

    /// Chats are asked for once a running daemon says it's paired, and
    /// again when the connection comes back after a failure. They're
    /// forgotten when the daemon goes away or the pairing ends.
    fn load_chats(&mut self, was_connected: bool) -> Vec<Effect> {
        if !self.paired() {
            if self.chat.access != Access::Unknown {
                self.chat.access = Access::Unknown;
                self.chat.following = Following::No;
            }
            return Vec::new();
        }
        let reconnected = !was_connected && self.daemon.connection() == Some("connected");
        let retry = match &self.chat.access {
            Access::Unknown => true,
            Access::Unavailable(_) | Access::NeedsApproval { .. } => reconnected,
            Access::Loading | Access::Ready => false,
        };
        if !retry {
            return Vec::new();
        }
        self.chat.access = Access::Loading;
        let mut effects = vec![Effect::Chat(ChatCommand::List)];
        if let Some(open) = self.chat.open {
            // A restarted daemon forgot what this TUI followed.
            self.chat.following = Following::Starting;
            effects.push(Effect::Chat(ChatCommand::Follow(Some(open))));
            effects.extend(self.refresh_chat(open));
        }
        effects
    }

    fn chat_msg(&mut self, msg: ChatMsg) -> Vec<Effect> {
        use super::theme::Signal;
        match msg {
            ChatMsg::Listed(Ok(list)) => {
                let was_ready = self.chat.ready();
                if !was_ready && self.focus == Focus::Roots && self.modal.is_none() {
                    self.focus = Focus::Chats;
                }
                self.chat.access = Access::Ready;
                self.chat.list = list;
                let len = self.chat.visible().len();
                self.chat.selected = self.chat.selected.min(len);
                // Chats are back (allowed again, say): follow the open one
                // again, unless that's already underway. A list that works
                // after a refusal leaves it be: the refusal was about the
                // chat, and following again would only be refused again.
                if let Some(open) = self.chat.open
                    && !was_ready
                    && self.chat.following != Following::Starting
                {
                    self.chat.following = Following::Starting;
                    let mut effects = vec![Effect::Chat(ChatCommand::Follow(Some(open)))];
                    effects.extend(self.refresh_chat(open));
                    return effects;
                }
            }
            ChatMsg::Listed(Err(failure)) => {
                self.chat_failure(failure);
                // Asked already, maybe from another terminal: check back
                // until the owner answers, as if asked from here.
                if matches!(
                    self.chat.access,
                    Access::NeedsApproval {
                        requested: true,
                        ..
                    }
                ) {
                    return vec![Effect::Chat(ChatCommand::WaitForAccess)];
                }
            }
            ChatMsg::Shown { chat, result } => {
                if self.chat.open != Some(chat) {
                    return Vec::new();
                }
                self.chat.loading = false;
                match result {
                    Ok(transcript) => self.take_transcript(transcript),
                    Err(failure) if failure.code == "not_found" => {
                        self.chat.error =
                            Some("This chat is gone, or you can't see it anymore.".into());
                    }
                    Err(failure) if is_access_failure(&failure) => self.chat_failure(failure),
                    Err(failure) => self.chat.error = Some(failure.message),
                }
                if self.chat.stale {
                    self.chat.stale = false;
                    return self.refresh_chat(chat);
                }
            }
            ChatMsg::Sent { chat, result } => {
                self.chat.sending = false;
                match result {
                    Ok(summary) => {
                        let number = summary.number;
                        self.chat.list.chats.retain(|c| c.number != number);
                        self.chat.list.chats.insert(0, summary);
                        let mut effects = vec![Effect::Chat(ChatCommand::List)];
                        if chat.is_none() && self.chat.open.is_none() {
                            // The first question made the chat: follow it.
                            self.chat.open = Some(number);
                            self.chat.following = Following::Starting;
                            self.chat.loading = true;
                            self.chat.selected = 1;
                            effects.push(Effect::Chat(ChatCommand::Follow(Some(number))));
                            effects.push(Effect::Chat(ChatCommand::Open(number)));
                        } else {
                            effects.extend(self.refresh_chat(number));
                        }
                        return effects;
                    }
                    Err(failure) => {
                        // Give the question back, to try again.
                        if let Some(question) = self.chat.pending_question.take()
                            && self.chat.input.is_empty()
                        {
                            self.chat.input = question;
                        }
                        if is_access_failure(&failure) {
                            self.chat_failure(failure);
                        } else {
                            self.notice(Signal::Negative, &failure.message);
                        }
                    }
                }
            }
            ChatMsg::Cancelled(Ok(())) => {}
            ChatMsg::Cancelled(Err(failure)) => {
                self.chat.stopping = false;
                self.notice(Signal::Negative, &failure.message);
            }
            ChatMsg::Access(Ok(request)) if request.granted => {
                self.chat.access = Access::Loading;
                return vec![Effect::Chat(ChatCommand::List)];
            }
            ChatMsg::Access(Ok(request)) => {
                self.notice(
                    Signal::Idle,
                    "Asked. Allow it in Chat with Work: Settings, Computers.",
                );
                let url = request.approve_url.clone();
                self.chat.access = Access::NeedsApproval {
                    requested: true,
                    url: url.clone(),
                };
                if let Some(url) = url.filter(|url| self.on_server(url)) {
                    return vec![Effect::Chat(ChatCommand::OpenUrl(url))];
                }
            }
            ChatMsg::Access(Err(failure)) => self.chat_failure(failure),
            ChatMsg::Live { chat, live } => {
                if self.chat.open != Some(chat) {
                    return Vec::new();
                }
                match live {
                    Live::Chunk { message_id, text } => {
                        self.chat.following = Following::Live;
                        self.chat.progress = None;
                        self.chat
                            .streamed
                            .entry(message_id)
                            .or_default()
                            .push_str(&text);
                    }
                    Live::Progress(text) => self.chat.progress = Some(text),
                    Live::Changed | Live::Watching => {
                        self.chat.following = Following::Live;
                        return self.refresh_chat(chat);
                    }
                    Live::Offline => self.chat.following = Following::Offline,
                    // Maybe chats were taken back: the list says.
                    Live::Refused => {
                        self.chat.following = Following::Refused;
                        if self.chat.ready() {
                            return vec![Effect::Chat(ChatCommand::List)];
                        }
                    }
                    // Not a refusal, just a connection that can't follow:
                    // nothing the list would explain.
                    Live::Unsupported => self.chat.following = Following::Unsupported,
                }
            }
            ChatMsg::FollowFailed { chat, failure } => {
                if self.chat.open == Some(chat) {
                    self.chat.following = Following::Refused;
                    if is_access_failure(&failure) {
                        self.chat_failure(failure);
                    }
                }
            }
        }
        Vec::new()
    }

    /// A chat read back: it replaces what streamed in, once it has it.
    fn take_transcript(&mut self, transcript: Transcript) {
        let chat = &mut self.chat;
        for entry in &transcript.entries {
            if let Entry::Assistant { id, content, .. } = entry
                && !content.trim().is_empty()
            {
                chat.streamed.remove(id);
            }
        }
        if !transcript.chat.processing() {
            chat.streamed.clear();
            chat.progress = None;
            chat.stopping = false;
        }
        let asked = transcript.entries.iter().rev().find_map(|e| match e {
            Entry::User { content, .. } => Some(content.trim()),
            _ => None,
        });
        if chat.pending_question.as_deref().map(str::trim) == asked {
            chat.pending_question = None;
        }
        if let Some(listed) = chat
            .list
            .chats
            .iter_mut()
            .find(|c| c.number == transcript.chat.number)
        {
            *listed = transcript.chat.clone();
        }
        chat.error = None;
        chat.transcript = Some(transcript);
    }

    fn chat_failure(&mut self, failure: Failure) {
        self.chat.access = match failure.code.as_str() {
            "chat_access_required" => Access::NeedsApproval {
                requested: failure.requested,
                url: failure.approve_url,
            },
            "daemon_stopped" | "not_paired" => Access::Unknown,
            _ => Access::Unavailable(failure),
        };
        self.chat.search = None;
        if self.focus != Focus::Roots {
            self.focus = Focus::Roots;
        }
    }
}

/// Failures that are about using chats here at all, rather than one call.
fn is_access_failure(failure: &Failure) -> bool {
    matches!(
        failure.code.as_str(),
        "chat_access_required" | "daemon_stopped" | "not_paired" | "revoked" | "unsupported"
    )
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
