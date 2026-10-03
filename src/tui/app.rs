//! The TUI's state and the reducer that changes it.
//!
//! [`App::update`] takes one message (a key, something the daemon said, a
//! chat event) and returns the effects to run. It does no I/O and never reads
//! the clock, so tests can drive it directly.

use std::collections::BTreeMap;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use super::chat::{
    AccessRequest, Approval, ChatList, ChatSummary, Entry, Failure, ListLive, Live, Model, Models,
    Project, Shared, Transcript, Uploaded,
};
use super::commands::{self, Cmd, Command};
use super::composer::{Composer, History};
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
    /// Where its settings and its activity log are.
    pub config_file: Option<String>,
    pub audit_file: Option<String>,
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
    /// The user allowed changes in it.
    pub writable: bool,
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
            writable: false,
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
    pub config_file: Option<String>,
    pub audit_file: Option<String>,
}

/// The deny list in effect, as the daemon (or `config.toml`) says it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct DenyList {
    /// The built-in patterns still in effect.
    pub builtin: Vec<String>,
    /// Patterns added in `config.toml`.
    pub extra: Vec<String>,
    /// Built-in patterns dropped in `config.toml`.
    pub removed: Vec<String>,
    /// The Local Agent's own folders.
    pub own_dirs: Vec<String>,
    pub allow_hardlinks: bool,
    pub config_file: Option<String>,
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

    /// Where the settings file and the activity log are.
    pub fn files(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::Unknown => (None, None),
            Self::Running(s) => (s.config_file.as_deref(), s.audit_file.as_deref()),
            Self::NotRunning(o) => (o.config_file.as_deref(), o.audit_file.as_deref()),
        }
    }
}

/// The chats, or the settings on one of their pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Chat,
    Settings(Page),
}

/// A page of the settings, as the desktop app has them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Folders,
    Privacy,
    Activity,
    Account,
    General,
}

impl Page {
    /// In the order the settings list them.
    pub const ALL: [Page; 5] = [
        Page::Folders,
        Page::Privacy,
        Page::Activity,
        Page::Account,
        Page::General,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::Folders => "Shared folders",
            Page::Privacy => "Always private",
            Page::Activity => "Activity",
            Page::Account => "Account",
            Page::General => "General",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Page::Folders => {
                "Chat with Work can search and read the files in these folders, and nothing \
                 else on this computer. It changes files only in folders where you allow it."
            }
            Page::Privacy => {
                "These files are never shared, even inside a shared folder. Chat with Work \
                 can't change this list."
            }
            Page::Activity => {
                "Every request from Chat with Work, as logged on this computer. The log never \
                 leaves it."
            }
            Page::Account => "This computer's pairing with your Chat with Work account.",
            Page::General => "The Local Agent that answers Chat with Work in the background.",
        }
    }

    /// The page after this one, or before it, going round.
    fn step(self, forward: bool) -> Page {
        let at = Page::ALL.iter().position(|p| *p == self).unwrap_or(0);
        let len = Page::ALL.len();
        Page::ALL[if forward {
            (at + 1) % len
        } else {
            (at + len - 1) % len
        }]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Chats,
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
    /// Allow changes in a shared folder: say what that means first.
    ConfirmChanges {
        id: String,
        label: String,
    },
    /// A new label for a shared folder.
    RenameRoot {
        id: String,
        input: String,
    },
    /// The daemon refused a folder as too broad; ask before `i_know`.
    ConfirmBroad {
        path: String,
        reason: String,
    },
    /// `/delete`: deleting a chat can't be undone.
    ConfirmDelete {
        chat: u64,
        title: String,
    },
    /// `/logout`: forgetting the pairing.
    ConfirmLogout,
    /// Keys and commands, scrolled down by `scroll` lines.
    Help {
        scroll: usize,
    },
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
    /// Show a shared folder to Chat with Work under another label.
    LabelRoot {
        id: String,
        label: String,
    },
    /// Allow changes in a shared folder, or make it read-only again.
    SetWritable {
        id: String,
        writable: bool,
    },
    /// Look for the daemon again now.
    Retry,
    /// Pair this computer (the device flow), from inside the TUI.
    Pair,
    /// Stop waiting for the pairing to be approved.
    CancelPairing,
    /// `cww daemon install`: register and start the background service.
    InstallService,
    /// `cww logout`: forget the pairing.
    Logout,
    /// Read the deny list in effect.
    LoadDeny,
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
    /// Keep the chat list current, or stop.
    FollowList(bool),
    /// Ask in a chat, or in a new one.
    Send {
        chat: Option<u64>,
        text: String,
        /// A model picked with `/model`.
        model: Option<String>,
        /// A project for a new chat, picked with `/project`.
        project: Option<u64>,
        /// Uploaded files, by `signed_id`.
        attachments: Vec<String>,
    },
    Cancel(u64),
    /// The models to pick from.
    Models,
    /// Approve a change the answer stopped at.
    Approve {
        chat: u64,
        tool_call: u64,
        for_rest_of_chat: bool,
    },
    /// Deny it, maybe saying what to do instead.
    Deny {
        chat: u64,
        tool_call: u64,
        reason: Option<String>,
    },
    /// Answer a question from a tool's server: the form's values, or
    /// nothing for a page that was visited.
    Answer {
        chat: u64,
        tool_call: u64,
        input: Option<serde_json::Value>,
    },
    Decline {
        chat: u64,
        tool_call: u64,
    },
    /// Read a file and upload it for the next question.
    Upload(String),
    /// Retry, branch, rename, delete or share a chat.
    Act {
        chat: u64,
        action: ChatAction,
    },
    /// Ask the owner to allow chats, then check back until they answer.
    RequestAccess,
    /// Check back until the owner answers a request made earlier.
    WaitForAccess,
    /// Open a page of the paired server in the browser.
    OpenUrl(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAction {
    /// The latest question, answered again.
    Retry,
    /// The whole conversation, in a new chat.
    Branch,
    Rename(String),
    Delete,
    Share,
    Unshare,
}

/// What a chat action answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Acted {
    Chat(ChatSummary),
    Deleted,
    Shared(Shared),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Daemon(DaemonCommand),
    Chat(ChatCommand),
    /// Put text on the clipboard (OSC 52).
    Copy(String),
    /// Ctrl-L: draw the whole screen again.
    Redraw,
    /// Keep a question in the history file.
    Remember(String),
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
    /// The deny list in effect, or why it can't be read.
    Deny(Result<DenyList, String>),
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
    /// A change to the chat list.
    ListLive(ListLive),
    /// Keeping the list current stopped with an error.
    ListFollowFailed(Failure),
    Models(Result<Models, Failure>),
    /// A change was approved or denied.
    Decided {
        chat: u64,
        result: Result<ChatSummary, Failure>,
    },
    /// A file for the next question was uploaded, or not.
    Uploaded {
        path: String,
        result: Result<Uploaded, Failure>,
    },
    Acted {
        chat: u64,
        action: ChatAction,
        result: Result<Acted, Failure>,
    },
}

// One message at a time goes through the channel; a read chat is the big one.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Key(KeyEvent),
    /// A click or the wheel, on what the last frame drew there.
    Mouse(MouseEvent),
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
    /// The server keeps the list current, so it needn't be read again
    /// after each change made here.
    pub list_live: bool,
    /// The selected row of the sidebar, in [`ChatPane::side_items`]: 0 is
    /// "New chat", then the chats the search leaves, then the projects.
    pub selected: usize,
    /// What's typed after `/`, while searching.
    pub search: Option<String>,
    /// The project whose chats the sidebar shows, picked in its list.
    pub filter: Option<u64>,
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
    pub input: Composer,
    /// Questions asked before, for Up and Down.
    pub history: History,
    /// The selected row of the slash command list.
    pub menu: usize,
    /// Esc closed the list; it opens again when the text changes.
    pub menu_closed: bool,
    /// A list to pick from (`/resume`, `/model`, `/project`).
    pub picker: Option<Picker>,
    pub models: ModelList,
    /// The model picked with `/model`, for the chat it was picked in.
    pub model: Option<ModelChoice>,
    /// The project the next new chat starts in.
    pub project: Option<Project>,
    /// Files for the next question.
    pub attachments: Vec<Attached>,
    /// The selected answer to the change waiting for approval.
    pub decision: usize,
    /// Which change `decision` is for.
    pub decision_for: Option<u64>,
    /// A decision is on its way.
    pub deciding: bool,
    /// Typing what to do instead of the denied change.
    pub reason: bool,
    /// The answers to the question waiting, as they're filled in.
    pub form: Form,
    pub sending: bool,
    pub stopping: bool,
    pub error: Option<String>,
    /// Transcript lines scrolled up from the newest.
    pub scroll: usize,
    /// After sending, the newest question stays near the top with its
    /// answer growing under it, as the web scrolls, until the person
    /// scrolls themselves.
    pub pinned: bool,
    /// Where a pinned view was last drawn, in lines up from the newest,
    /// so scrolling away from it starts there.
    pub pinned_scroll: std::cell::Cell<usize>,
    /// Show every tool step under its activity line.
    pub show_steps: bool,
}

impl Default for ChatPane {
    fn default() -> Self {
        Self {
            access: Access::Unknown,
            list: ChatList::default(),
            list_live: false,
            selected: 0,
            search: None,
            filter: None,
            open: None,
            transcript: None,
            loading: false,
            stale: false,
            following: Following::No,
            streamed: BTreeMap::new(),
            progress: None,
            pending_question: None,
            input: Composer::default(),
            history: History::default(),
            menu: 0,
            menu_closed: false,
            picker: None,
            models: ModelList::Unknown,
            model: None,
            project: None,
            attachments: Vec::new(),
            decision: 0,
            decision_for: None,
            deciding: false,
            reason: false,
            form: Form::default(),
            sending: false,
            stopping: false,
            error: None,
            scroll: 0,
            pinned: false,
            pinned_scroll: std::cell::Cell::new(0),
            show_steps: false,
        }
    }
}

/// A row of the sidebar that can be selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideItem {
    NewChat,
    /// A chat in Recent.
    Chat(u64),
    /// A pinned project, in Pinned.
    Project(u64),
    /// A pinned chat, in Pinned (it stays in Recent too, as on the web).
    PinnedChat(u64),
}

/// Whether `a` was updated after `b`, both RFC 3339.
fn newer(a: &str, b: &str) -> bool {
    match (parse_ts(a), parse_ts(b)) {
        (Some(a), Some(b)) => a > b,
        _ => a > b,
    }
}

/// What `/model` can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelList {
    Unknown,
    Loading,
    Ready(Models),
    /// This server or daemon can't offer a choice, as this sentence says.
    Unavailable(String),
}

/// A model picked with `/model`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// The chat it was picked in; `None` for the next new chat.
    pub chat: Option<u64>,
    pub id: String,
    pub name: String,
}

/// A file for the next question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    pub path: String,
    pub name: String,
    /// Its `signed_id` and size once uploaded.
    pub uploaded: Option<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    pub kind: PickerKind,
    pub query: String,
    pub selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Chats,
    Models,
    Projects,
}

/// One row of a picker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PickItem<'a> {
    Chat(&'a ChatSummary),
    Model(&'a Model),
    /// `None` is "no project".
    Project(Option<&'a Project>),
}

/// The answers to a change waiting for approval, as the prompt lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Approve,
    /// Approve, and don't ask again for this tool in this chat.
    ApproveAll,
    Deny,
    /// Deny, and say what to do instead.
    DenyWithReason,
}

/// A question's form, as it's filled in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form {
    /// The question it answers.
    pub question: Option<u64>,
    /// Each field's value, as typed or picked; empty leaves it out.
    pub values: Vec<String>,
    /// The selected row.
    pub row: usize,
    /// The field being typed in the composer.
    pub editing: Option<usize>,
}

impl Form {
    /// The values to send, by field name, typed as the fields ask.
    pub fn input(&self, question: &super::chat::Question) -> serde_json::Value {
        let mut input = serde_json::Map::new();
        for (field, value) in question.fields.iter().zip(&self.values) {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            let json = match field.kind.as_str() {
                "boolean" => serde_json::Value::Bool(value == "yes"),
                "array" => value
                    .split(',')
                    .map(|v| serde_json::Value::String(v.trim().to_string()))
                    .filter(|v| v.as_str() != Some(""))
                    .collect(),
                // Numbers go as typed: the server reads them as a form does.
                _ => serde_json::Value::String(value.to_string()),
            };
            input.insert(field.name.clone(), json);
        }
        serde_json::Value::Object(input)
    }
}

/// A row of a question's form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormRow {
    Field(usize),
    /// A page to visit: open it in the browser.
    OpenPage,
    /// Send the answer (for a page: it was visited).
    Send,
    Decline,
}

/// A field picked from a list: its choices, or yes and no.
pub fn field_has_choices(field: &super::chat::Field) -> bool {
    !field_options(field).is_empty() && field.kind != "array"
}

/// What a field can be set to with Left and Right.
pub fn field_options(field: &super::chat::Field) -> Vec<String> {
    if field.kind == "boolean" {
        return vec!["yes".into(), "no".into()];
    }
    match &field.choices {
        Some(choices) if field.kind != "array" => {
            let mut options = choices.clone();
            if !field.required {
                options.push(String::new());
            }
            options
        }
        _ => Vec::new(),
    }
}

/// What the mouse can click or scroll, where the last frame drew it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    /// A settings page in the list.
    Page(Page),
    /// Back to the chats, from the settings.
    Back,
    /// Your name, or the status indicator: the settings.
    Settings,
    /// The search row.
    Search,
    NewChat,
    Chat(u64),
    /// The chat list, for the wheel.
    Chats,
    Root(usize),
    /// A project in the sidebar: its chats, and a new chat in it.
    Project(u64),
    /// A page of the web's settings, by its place in the list's links.
    Web(usize),
    /// The web's page of every project.
    AllProjects,
    Composer,
    /// A row of the slash command list.
    Menu(usize),
    Pick(usize),
    /// A row of the approval prompt.
    Choice(usize),
    /// A row of a question's form.
    Ask(usize),
    Link(String),
    Transcript,
    Log,
}

/// Where things are on screen, from the last frame drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hits(Vec<(Rect, Hit)>);

impl Hits {
    pub fn add(&mut self, rect: Rect, hit: Hit) {
        if rect.width > 0 && rect.height > 0 {
            self.0.push((rect, hit));
        }
    }

    /// What's at a cell: the last thing drawn there wins.
    pub fn at(&self, x: u16, y: u16) -> Option<&Hit> {
        self.0
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(ratatui::layout::Position::new(x, y)))
            .map(|(_, hit)| hit)
    }
}

impl ChatPane {
    pub fn ready(&self) -> bool {
        self.access == Access::Ready
    }

    /// Recent: the chats the search and the project picked leave, newest
    /// first, pinned ones included, as the web lists them.
    pub fn visible(&self) -> Vec<&ChatSummary> {
        self.matching()
    }

    /// Pinned, under Recent as on the web: the pinned projects, in pin order.
    pub fn pinned_projects(&self) -> Vec<&Project> {
        let projects = &self.list.projects;
        self.list
            .pins
            .projects
            .iter()
            .filter_map(|id| projects.iter().find(|p| p.id == *id))
            .collect()
    }

    /// Pinned, after the projects: the pinned chats, in pin order.
    pub fn pinned_chats(&self) -> Vec<&ChatSummary> {
        let chats = &self.list.chats;
        self.list
            .pins
            .chats
            .iter()
            .filter_map(|n| chats.iter().find(|c| c.number == *n))
            .collect()
    }

    /// The chats the search and the project picked leave, newest first.
    fn matching(&self) -> Vec<&ChatSummary> {
        let query = self
            .search
            .as_deref()
            .map(|q| q.trim().trim_start_matches('#').to_lowercase())
            .filter(|q| !q.is_empty());
        self.list
            .chats
            .iter()
            .filter(|chat| {
                self.filter
                    .is_none_or(|id| chat.project.as_ref().is_some_and(|p| p.id == id))
            })
            .filter(|chat| match &query {
                None => true,
                Some(query) => {
                    chat.title.to_lowercase().contains(query)
                        || chat.number.to_string() == *query
                        || chat
                            .project
                            .as_ref()
                            .is_some_and(|p| p.name.to_lowercase().contains(query))
                }
            })
            .collect()
    }

    /// The sidebar's rows that can be selected, in order: "New chat",
    /// Recent, then Pinned's projects and chats (not while searching).
    pub fn side_items(&self) -> Vec<SideItem> {
        let mut items = vec![SideItem::NewChat];
        items.extend(self.visible().iter().map(|c| SideItem::Chat(c.number)));
        if self.search.is_none() {
            items.extend(
                self.pinned_projects()
                    .iter()
                    .map(|p| SideItem::Project(p.id)),
            );
            items.extend(
                self.pinned_chats()
                    .iter()
                    .map(|c| SideItem::PinnedChat(c.number)),
            );
        }
        items
    }

    /// The selected chat, unless something else is selected.
    pub fn selected_chat(&self) -> Option<&ChatSummary> {
        match self.side_items().get(self.selected)? {
            SideItem::Chat(number) | SideItem::PinnedChat(number) => {
                self.list.chats.iter().find(|c| c.number == *number)
            }
            _ => None,
        }
    }

    /// The project the sidebar shows the chats of.
    pub fn filter_project(&self) -> Option<&Project> {
        let id = self.filter?;
        self.list.projects.iter().find(|p| p.id == id)
    }

    /// Change the list, keeping the same row selected where it's still
    /// there. Live updates and reads of the list go through here.
    fn keep_selection(&mut self, change: impl FnOnce(&mut Self)) {
        let before = self.side_items().get(self.selected).copied();
        change(self);
        let items = self.side_items();
        self.selected = before
            .and_then(|item| items.iter().position(|i| *i == item))
            .unwrap_or(self.selected)
            .min(items.len() - 1);
    }

    /// The whole list, as read.
    pub fn set_list(&mut self, list: ChatList) {
        self.keep_selection(|pane| {
            pane.list = list;
            if pane.filter_project().is_none() {
                pane.filter = None;
            }
        });
    }

    /// A chat as the server last sent it: new ones go in, others are
    /// replaced, and the list stays newest first.
    pub fn upsert_chat(&mut self, chat: ChatSummary) {
        self.keep_selection(|pane| {
            let chats = &mut pane.list.chats;
            chats.retain(|c| c.number != chat.number);
            let at = chats
                .iter()
                .position(|c| newer(&chat.updated_at, &c.updated_at))
                .unwrap_or(chats.len());
            chats.insert(at, chat.clone());
            if let Some(transcript) = pane
                .transcript
                .as_mut()
                .filter(|t| t.chat.number == chat.number)
            {
                transcript.chat = chat;
            }
        });
    }

    /// A chat that's gone from the list.
    pub fn remove_chat(&mut self, number: u64) {
        self.keep_selection(|pane| pane.list.chats.retain(|c| c.number != number));
    }

    /// The projects, as the server last listed them.
    pub fn set_projects(&mut self, projects: Vec<Project>) {
        self.keep_selection(|pane| {
            pane.list.projects = projects;
            if pane.filter_project().is_none() {
                pane.filter = None;
            }
        });
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

    /// The change this person can decide now in the open chat.
    pub fn pending_decision(&self) -> Option<&Approval> {
        self.transcript
            .as_ref()
            .filter(|t| Some(t.chat.number) == self.open && !t.chat.processing())
            .and_then(Transcript::next_decision)
    }

    /// The question from a tool's server this person can answer now, once
    /// no change waits for them first.
    pub fn pending_question(&self) -> Option<&super::chat::Question> {
        if self.pending_decision().is_some() {
            return None;
        }
        self.transcript
            .as_ref()
            .filter(|t| Some(t.chat.number) == self.open && !t.chat.processing())
            .and_then(Transcript::next_question)
    }

    /// The rows of a question's form: each field, then send and decline;
    /// for a page, open it, then done and decline.
    pub fn question_rows(question: &super::chat::Question) -> Vec<FormRow> {
        let mut rows: Vec<FormRow> = if question.is_url() {
            vec![FormRow::OpenPage]
        } else {
            (0..question.fields.len()).map(FormRow::Field).collect()
        };
        rows.extend([FormRow::Send, FormRow::Decline]);
        rows
    }

    /// The answers the prompt offers for `approval`.
    pub fn choices(approval: &Approval) -> Vec<Choice> {
        let mut choices = vec![Choice::Approve];
        if approval.allow_for_rest_of_chat {
            choices.push(Choice::ApproveAll);
        }
        choices.extend([Choice::Deny, Choice::DenyWithReason]);
        choices
    }

    /// The model the next question is asked with, by name: the one picked
    /// for this chat, else the chat's own.
    pub fn model_name(&self) -> Option<&str> {
        match &self.model {
            Some(choice) if choice.chat == self.open => Some(&choice.name),
            _ => self
                .open_summary()
                .and_then(|c| c.model.as_ref())
                .map(|m| m.name.as_str()),
        }
    }

    /// The model picked for the next question here, if any.
    fn model_for_question(&self) -> Option<String> {
        self.model
            .as_ref()
            .filter(|choice| choice.chat == self.open)
            .map(|choice| choice.id.clone())
    }

    /// The rows the open picker shows, filtered by its query.
    pub fn pick_items(&self) -> Vec<PickItem<'_>> {
        let Some(picker) = &self.picker else {
            return Vec::new();
        };
        let query = picker.query.trim().to_lowercase();
        let has = |text: &str| query.is_empty() || text.to_lowercase().contains(&query);
        match picker.kind {
            PickerKind::Chats => self
                .list
                .chats
                .iter()
                .filter(|c| {
                    has(&c.title)
                        || c.number.to_string() == query.trim_start_matches('#')
                        || c.project.as_ref().is_some_and(|p| has(&p.name))
                })
                .map(PickItem::Chat)
                .collect(),
            PickerKind::Models => match &self.models {
                ModelList::Ready(models) => models
                    .models
                    .iter()
                    .filter(|m| has(&m.name) || has(&m.provider) || m.id == query)
                    .map(PickItem::Model)
                    .collect(),
                _ => Vec::new(),
            },
            PickerKind::Projects => {
                let mut items = Vec::new();
                if query.is_empty() || "no project".contains(&query) {
                    items.push(PickItem::Project(None));
                }
                items.extend(
                    self.list
                        .projects
                        .iter()
                        .filter(|p| has(&p.name))
                        .map(|p| PickItem::Project(Some(p))),
                );
                items
            }
        }
    }

    /// The model the open chat (or a new one) uses now, to mark it.
    pub fn current_model_id(&self) -> Option<&str> {
        match &self.model {
            Some(choice) if choice.chat == self.open => Some(&choice.id),
            _ => match self.open_summary() {
                Some(chat) => chat.model.as_ref().map(|m| m.id.as_str()),
                None if self.open.is_none() => match &self.models {
                    ModelList::Ready(models) => models.default_model_id.as_deref(),
                    _ => None,
                },
                None => None,
            },
        }
    }

    /// The last answer, as Markdown.
    pub fn last_answer(&self) -> Option<&str> {
        let read = self.transcript.as_ref().and_then(|t| {
            t.entries.iter().rev().find_map(|e| match e {
                Entry::Assistant { content, .. } if !content.trim().is_empty() => {
                    Some(content.as_str())
                }
                _ => None,
            })
        });
        let streamed = self
            .streamed
            .values()
            .next_back()
            .map(String::as_str)
            .filter(|t| !t.trim().is_empty());
        streamed.or(read)
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
    /// The deny list, once read for its page.
    pub deny: Option<Result<DenyList, String>>,
    pub view: View,
    pub focus: Focus,
    pub selected_root: usize,
    /// Log lines scrolled up from the newest.
    pub log_scroll: usize,
    /// The selected page of the web's settings, on the Account page.
    pub selected_link: usize,
    /// Lines the Always private page is scrolled down by.
    pub page_scroll: usize,
    pub modal: Option<Modal>,
    pub notice: Option<Notice>,
    pub pairing: Option<Pairing>,
    /// The first-run offer was answered.
    pub offer_dismissed: bool,
    pub chat: ChatPane,
    /// For showing times in local time.
    pub utc_offset: UtcOffset,
    pub quit: bool,
    /// Ctrl-C was pressed once with nothing to clear: again quits.
    pub quit_armed: bool,
    /// What the last frame drew where, for the mouse.
    pub hits: Hits,
    /// Effects to run after the message at hand, such as reading what a
    /// page shows when it opens.
    later: Vec<Effect>,
}

impl App {
    pub fn new(utc_offset: UtcOffset) -> Self {
        Self {
            daemon: Daemon::Unknown,
            audit: Vec::new(),
            suggestions: Vec::new(),
            deny: None,
            view: View::Chat,
            focus: Focus::Chats,
            selected_root: 0,
            log_scroll: 0,
            selected_link: 0,
            page_scroll: 0,
            modal: None,
            notice: None,
            pairing: None,
            offer_dismissed: false,
            chat: ChatPane::default(),
            utc_offset,
            quit: false,
            quit_armed: false,
            hits: Hits::default(),
            later: Vec::new(),
        }
    }

    /// With the questions asked in earlier runs, for Up.
    pub fn with_history(mut self, history: Vec<String>) -> Self {
        self.chat.history = History::new(history);
        self
    }

    /// Effects to run once, at start. Chats wait for the daemon's status.
    pub fn start(&self) -> Vec<Effect> {
        Vec::new()
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        let mut effects = self.handle(msg);
        effects.append(&mut self.later);
        effects
    }

    fn handle(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => self.key(key),
            Msg::Mouse(event) => self.mouse(event),
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
            vec![Focus::Chats, Focus::Composer]
        } else {
            vec![Focus::Chats]
        }
    }

    /// Whether something on screen moves, so the loop needs frames: a
    /// folder being indexed while its page shows, the connection coming
    /// up, an answer being written.
    pub fn animating(&self) -> bool {
        let indexing = self.view == View::Settings(Page::Folders)
            && matches!(self.daemon, Daemon::Running(_))
            && self
                .daemon
                .roots()
                .iter()
                .any(|r| r.index == "indexing" || r.index == "pending");
        // A chat being answered shimmers in the list, while the list is
        // current enough to say when it stops.
        let listed = self.chat.list_live && self.chat.list.chats.iter().any(|c| c.processing());
        let chat = self.view == View::Chat
            && (self.chat.working()
                || listed
                || (self.chat.loading && self.chat.transcript.is_none()));
        indexing || self.daemon.connection() == Some("connecting") || chat
    }

    /// How long the loop may sleep before the screen goes stale: a frame
    /// while something moves, otherwise forever. Nothing on screen counts
    /// time by itself.
    pub fn next_wakeup(&self, _now: OffsetDateTime) -> Option<Duration> {
        self.animating().then_some(FRAME)
    }

    /// The settings page that helps most right now: pairing when this
    /// computer isn't paired, the daemon when it isn't running, else the
    /// shared folders.
    pub fn attention_page(&self) -> Page {
        match &self.daemon {
            Daemon::NotRunning(_) => Page::General,
            _ if self.can_pair() || self.pairing.is_some() => Page::Account,
            Daemon::Running(s) if s.paused => Page::General,
            Daemon::Running(s) if s.connection.connection == "offline" => Page::Account,
            _ => Page::Folders,
        }
    }

    /// Open the settings on `page`. The deny list is read each time its
    /// page opens, since only `config.toml` changes it.
    fn settings(&mut self, page: Page) {
        if !matches!(self.view, View::Settings(_)) {
            self.log_scroll = 0;
        }
        if page == Page::Privacy && self.view != View::Settings(Page::Privacy) {
            self.later.push(Effect::Daemon(DaemonCommand::LoadDeny));
            self.page_scroll = 0;
        }
        self.view = View::Settings(page);
        self.chat.search = None;
    }

    fn key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return self.ctrl_c();
        }
        if self.quit_armed {
            self.quit_armed = false;
            self.notice = None;
        }
        if ctrl && key.code == KeyCode::Char('l') {
            return vec![Effect::Redraw];
        }
        self.notice = None;
        if let Some(modal) = self.modal.take() {
            return self.modal_key(modal, key);
        }
        let in_chat = self.view == View::Chat;
        if in_chat && self.focus == Focus::Composer {
            return self.composer_key(key);
        }
        if in_chat && self.chat.search.is_some() {
            return self.search_key(key);
        }
        if let View::Settings(page) = self.view
            && let Some(effects) = self.page_key(page, key)
        {
            return effects;
        }
        let offer = self.offer().map(|s| (s.path.clone(), s.label.clone()));
        match key.code {
            KeyCode::Char('q') => {
                self.quit = true;
                return vec![Effect::Quit];
            }
            KeyCode::Tab if in_chat => self.cycle_focus(true),
            KeyCode::BackTab if in_chat => self.cycle_focus(false),
            KeyCode::Char(',') if in_chat => self.settings(self.attention_page()),
            KeyCode::Char(',') => self.view = View::Chat,
            KeyCode::Char('l') if self.view == View::Settings(Page::Activity) => {
                self.view = View::Chat;
            }
            KeyCode::Char('l') => {
                self.settings(Page::Activity);
                self.log_scroll = 0;
            }
            KeyCode::Esc if self.pairing.is_some() => {
                self.pairing = None;
                return vec![Effect::Daemon(DaemonCommand::CancelPairing)];
            }
            KeyCode::Esc if !in_chat => self.view = View::Chat,
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
            KeyCode::Char('y') => {
                if let Some((path, label)) = offer {
                    self.offer_dismissed = true;
                    return self.add_root(path, Some(label), false);
                }
            }
            KeyCode::Char('n') if offer.is_some() => self.offer_dismissed = true,
            KeyCode::Char('n') if in_chat && self.chat.ready() => return self.new_chat(),
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
            KeyCode::Char('?') => self.modal = Some(Modal::Help { scroll: 0 }),
            KeyCode::Char('/') if in_chat && self.chat.ready() => {
                self.chat.search = Some(String::new());
                self.focus = Focus::Chats;
                self.chat.selected = usize::from(!self.chat.list.chats.is_empty());
            }
            KeyCode::Char('o') => return self.open_in_browser(),
            KeyCode::Char('e') if in_chat => self.chat.show_steps = !self.chat.show_steps,
            KeyCode::Up | KeyCode::Char('k') if in_chat => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') if in_chat => self.move_selection(1),
            KeyCode::PageUp if in_chat => self.scroll_transcript(10),
            KeyCode::PageDown if in_chat => self.scroll_transcript(-10),
            KeyCode::End if in_chat => {
                self.chat.scroll = 0;
                self.chat.pinned = false;
            }
            KeyCode::Enter if in_chat && self.chat.ready() => return self.open_selected(),
            KeyCode::Char('i') if in_chat && self.chat.ready() => self.focus = Focus::Composer,
            _ => {}
        }
        Vec::new()
    }

    /// Keys a settings page has of its own, or `None` for the ones every
    /// screen has.
    fn page_key(&mut self, page: Page, key: KeyEvent) -> Option<Vec<Effect>> {
        match (page, key.code) {
            (_, KeyCode::Tab | KeyCode::Right) => self.settings(page.step(true)),
            (_, KeyCode::BackTab | KeyCode::Left) => self.settings(page.step(false)),
            (_, KeyCode::Char(c @ '1'..='9')) => {
                let at = c as usize - '1' as usize;
                self.settings(*Page::ALL.get(at)?);
            }
            (Page::Folders, KeyCode::Up | KeyCode::Char('k')) => self.move_root(-1),
            (Page::Folders, KeyCode::Down | KeyCode::Char('j')) => self.move_root(1),
            (Page::Folders, KeyCode::Char('r')) => self.rename_root(),
            (Page::Folders, KeyCode::Char('w')) => return Some(self.toggle_changes()),
            (Page::Folders, KeyCode::Char('d') | KeyCode::Char('x') | KeyCode::Delete) => {
                self.confirm_remove()
            }
            (Page::Privacy, KeyCode::Up | KeyCode::Char('k')) => {
                self.page_scroll = self.page_scroll.saturating_sub(1);
            }
            (Page::Privacy, KeyCode::Down | KeyCode::Char('j')) => self.page_scroll += 1,
            (Page::Privacy, KeyCode::PageUp) => {
                self.page_scroll = self.page_scroll.saturating_sub(10);
            }
            (Page::Privacy, KeyCode::PageDown) => self.page_scroll += 10,
            (Page::Activity, KeyCode::Up | KeyCode::Char('k')) => self.scroll_log(3),
            (Page::Activity, KeyCode::Down | KeyCode::Char('j')) => self.scroll_log(-3),
            (Page::Activity, KeyCode::PageUp) => self.scroll_log(10),
            (Page::Activity, KeyCode::PageDown) => self.scroll_log(-10),
            (Page::Activity, KeyCode::Home) => self.log_scroll = self.audit.len(),
            (Page::Activity, KeyCode::End | KeyCode::Char('G')) => self.log_scroll = 0,
            (Page::Account, KeyCode::Char('d')) if self.daemon.server().is_some() => {
                self.modal = Some(Modal::ConfirmLogout);
            }
            (Page::Account, KeyCode::Up | KeyCode::Char('k')) => {
                self.selected_link = self.selected_link.saturating_sub(1);
            }
            (Page::Account, KeyCode::Down | KeyCode::Char('j')) => {
                let len = self.chat.list.links.settings.len();
                self.selected_link = (self.selected_link + 1).min(len.saturating_sub(1));
            }
            (Page::Account, KeyCode::Enter) => return Some(self.open_web(self.selected_link)),
            _ => return None,
        }
        Some(Vec::new())
    }

    fn move_root(&mut self, delta: isize) {
        let len = self.daemon.roots().len();
        if len > 0 {
            self.selected_root = self.selected_root.saturating_add_signed(delta).min(len - 1);
        }
    }

    /// `w` on a shared folder: allow changes in it (after saying what that
    /// means), or make it read-only again at once.
    fn toggle_changes(&mut self) -> Vec<Effect> {
        let Some(root) = self.daemon.roots().get(self.selected_root).cloned() else {
            self.notice(super::theme::Signal::Idle, "Nothing is shared.");
            return Vec::new();
        };
        if root.writable {
            self.notice(
                super::theme::Signal::Idle,
                &format!("Making {} read-only…", root.label),
            );
            return vec![Effect::Daemon(DaemonCommand::SetWritable {
                id: root.id,
                writable: false,
            })];
        }
        self.modal = Some(Modal::ConfirmChanges {
            id: root.id,
            label: root.label,
        });
        Vec::new()
    }

    /// `r` on a shared folder: type its new label.
    fn rename_root(&mut self) {
        match self.daemon.roots().get(self.selected_root) {
            Some(root) => {
                self.modal = Some(Modal::RenameRoot {
                    id: root.id.clone(),
                    input: root.label.clone(),
                });
            }
            None => self.notice(super::theme::Signal::Idle, "Nothing is shared."),
        }
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
            Modal::RenameRoot { id, mut input } => {
                match key.code {
                    KeyCode::Esc => return Vec::new(),
                    KeyCode::Enter => {
                        let label = input.trim().to_string();
                        let same = self
                            .daemon
                            .roots()
                            .iter()
                            .any(|r| r.id == id && r.label == label);
                        if label.is_empty() || same {
                            return Vec::new();
                        }
                        self.notice(super::theme::Signal::Idle, "Renaming…");
                        return vec![Effect::Daemon(DaemonCommand::LabelRoot { id, label })];
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char('u') if ctrl => input.clear(),
                    KeyCode::Char('w') if ctrl => delete_word(&mut input),
                    KeyCode::Char(c) if !ctrl => input.push(c),
                    _ => {}
                }
                self.modal = Some(Modal::RenameRoot { id, input });
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
            Modal::ConfirmChanges { id, label } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.notice(
                        super::theme::Signal::Idle,
                        &format!("Allowing changes in {label}…"),
                    );
                    return vec![Effect::Daemon(DaemonCommand::SetWritable {
                        id,
                        writable: true,
                    })];
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.modal = Some(Modal::ConfirmChanges { id, label }),
            },
            Modal::ConfirmBroad { path, reason } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return self.add_root(path, None, true),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.modal = Some(Modal::ConfirmBroad { path, reason }),
            },
            Modal::ConfirmDelete { chat, title } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.notice(super::theme::Signal::Idle, &format!("Deleting {title}…"));
                    return vec![Effect::Chat(ChatCommand::Act {
                        chat,
                        action: ChatAction::Delete,
                    })];
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.modal = Some(Modal::ConfirmDelete { chat, title }),
            },
            Modal::ConfirmLogout => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.notice(super::theme::Signal::Idle, "Forgetting the pairing…");
                    return vec![Effect::Daemon(DaemonCommand::Logout)];
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.modal = Some(Modal::ConfirmLogout),
            },
            Modal::Help { scroll } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Some(Modal::Help {
                        scroll: scroll.saturating_sub(1),
                    });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.modal = Some(Modal::Help { scroll: scroll + 1 });
                }
                KeyCode::PageUp => {
                    self.modal = Some(Modal::Help {
                        scroll: scroll.saturating_sub(10),
                    });
                }
                KeyCode::PageDown => {
                    self.modal = Some(Modal::Help {
                        scroll: scroll + 10,
                    })
                }
                _ => {}
            },
        }
        Vec::new()
    }

    /// Ctrl-C clears what's typed; with nothing to clear, pressing it twice
    /// in a row quits.
    fn ctrl_c(&mut self) -> Vec<Effect> {
        let chat = &mut self.chat;
        let typed = !chat.input.is_empty() || chat.picker.is_some() || chat.reason;
        if self.focus == Focus::Composer && typed && self.modal.is_none() {
            chat.input.clear();
            chat.picker = None;
            chat.reason = false;
            chat.menu_closed = false;
            chat.history.reset();
            self.quit_armed = false;
            self.notice = None;
            return Vec::new();
        }
        if self.quit_armed {
            self.quit = true;
            return vec![Effect::Quit];
        }
        self.quit_armed = true;
        self.notice(super::theme::Signal::Idle, "Press ctrl-c again to quit.");
        Vec::new()
    }

    fn composer_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if self.chat.picker.is_some() {
            return self.picker_key(key);
        }
        if let Some(effects) = self.decision_key(key) {
            return effects;
        }
        if let Some(effects) = self.question_key(key) {
            return effects;
        }
        if let Some(effects) = self.menu_key(key) {
            return effects;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let reason = self.chat.reason;
        let editing = self.chat.form.editing.is_some();
        let stop = self.chat.working() && !self.chat.stopping;
        let input = &mut self.chat.input;
        let before = input.text().to_string();
        match key.code {
            KeyCode::Esc if reason => {
                self.chat.reason = false;
                self.chat.input.clear();
                return Vec::new();
            }
            KeyCode::Esc if editing => {
                self.chat.form.editing = None;
                self.chat.input.clear();
                return Vec::new();
            }
            // Esc stops an answer being written, like the web's stop button.
            KeyCode::Esc if stop => {
                if let Some(chat) = self.chat.open {
                    self.chat.stopping = true;
                    self.notice(super::theme::Signal::Idle, "Stopping the answer…");
                    return vec![Effect::Chat(ChatCommand::Cancel(chat))];
                }
            }
            KeyCode::Esc => self.focus = Focus::Chats,
            KeyCode::Tab => self.cycle_focus(true),
            KeyCode::BackTab => self.cycle_focus(false),
            // A new line: Shift-Enter where the terminal tells it apart,
            // Alt-Enter and Ctrl-J everywhere.
            KeyCode::Enter if shift || alt => input.insert('\n'),
            KeyCode::Char('j') if ctrl => input.insert('\n'),
            // A trailing backslash continues on a new line, as in a shell.
            KeyCode::Enter
                if input.text().ends_with('\\') && input.cursor() == input.text().len() =>
            {
                input.backspace();
                input.insert('\n');
            }
            KeyCode::Enter if reason => return self.deny_with_reason(),
            KeyCode::Enter if editing => {
                self.save_field();
                return Vec::new();
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Up => {
                if !input.up() {
                    if let Some(earlier) = self.chat.history.back(input.text()) {
                        let earlier = earlier.to_string();
                        self.chat.input.set(earlier);
                    }
                    return Vec::new();
                }
            }
            KeyCode::Down => {
                if !input.down() {
                    if let Some(later) = self.chat.history.forward() {
                        self.chat.input.set(later);
                    }
                    return Vec::new();
                }
            }
            KeyCode::Left => input.left(),
            KeyCode::Right => input.right(),
            KeyCode::Home => input.home(),
            KeyCode::End => input.end(),
            KeyCode::Char('a') if ctrl => input.home(),
            KeyCode::Char('e') if ctrl => input.end(),
            KeyCode::Char('b') if ctrl => input.left(),
            KeyCode::Char('f') if ctrl => input.right(),
            KeyCode::Char('k') if ctrl => input.kill_after(),
            KeyCode::Char('u') if ctrl => input.kill_before(),
            KeyCode::Char('w') if ctrl => input.delete_word(),
            KeyCode::Char('h') if ctrl => input.backspace(),
            KeyCode::Char('d') if ctrl => input.delete(),
            KeyCode::PageUp => self.scroll_transcript(10),
            KeyCode::PageDown => self.scroll_transcript(-10),
            KeyCode::Backspace => input.backspace(),
            KeyCode::Delete => input.delete(),
            KeyCode::Char(c) if !ctrl => input.insert(c),
            _ => {}
        }
        if self.chat.input.text() != before {
            self.typed();
        }
        Vec::new()
    }

    /// The text changed: the slash list opens again at its top, and Up
    /// starts from the newest question again.
    fn typed(&mut self) {
        self.chat.menu = 0;
        self.chat.menu_closed = false;
        self.chat.history.reset();
    }

    /// The slash commands that match what's typed, while the list is open.
    pub fn menu(&self) -> Vec<&'static Command> {
        let chat = &self.chat;
        if self.focus != Focus::Composer
            || chat.menu_closed
            || chat.picker.is_some()
            || chat.reason
            || chat.form.editing.is_some()
            || !chat.ready()
        {
            return Vec::new();
        }
        let Some(name) = chat.input.text().strip_prefix('/') else {
            return Vec::new();
        };
        if name.starts_with('/') || name.contains(char::is_whitespace) {
            return Vec::new();
        }
        commands::matching(name, |cmd| self.offers(cmd))
    }

    /// Whether the slash list offers `cmd` here and now. Typed in full, a
    /// command it doesn't offer still runs and says why it can't.
    fn offers(&self, cmd: Cmd) -> bool {
        let open = self.chat.open_summary();
        let can = |action: fn(&super::chat::Can) -> bool| open.is_some_and(|c| c.can(action));
        match cmd {
            Cmd::Retry => can(|c| c.retry),
            Cmd::Branch => can(|c| c.branch),
            Cmd::Rename => can(|c| c.rename),
            Cmd::Delete => can(|c| c.delete),
            Cmd::ShareChat => can(|c| c.share),
            Cmd::UnshareChat => open.is_some_and(|c| c.share.is_some()),
            Cmd::Copy | Cmd::Open => open.is_some(),
            Cmd::Detach => !self.chat.attachments.is_empty(),
            Cmd::Login => self.can_pair(),
            Cmd::Logout => self.daemon.server().is_some(),
            Cmd::Pause => self.daemon.paused() == Some(false),
            Cmd::ResumeSharing => self.daemon.paused() == Some(true),
            Cmd::Project => !self.chat.list.projects.is_empty(),
            _ => true,
        }
    }

    /// Keys for the open slash list, or `None` to edit as usual.
    fn menu_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        let menu = self.menu();
        if menu.is_empty() {
            return None;
        }
        let len = menu.len();
        let selected = self.chat.menu.min(len - 1);
        match key.code {
            KeyCode::Up => self.chat.menu = (selected + len - 1) % len,
            KeyCode::Down => self.chat.menu = (selected + 1) % len,
            KeyCode::Tab => self.complete(menu[selected]),
            KeyCode::Enter => return Some(self.run_menu(menu[selected])),
            KeyCode::Esc => self.chat.menu_closed = true,
            _ => return None,
        }
        Some(Vec::new())
    }

    /// Put a command's name in the composer, with room for its argument.
    fn complete(&mut self, command: &Command) {
        let space = if command.args.is_empty() { "" } else { " " };
        self.chat.input.set(format!("/{}{space}", command.name));
        self.chat.menu = 0;
    }

    /// Enter on a row of the slash list: run it, or complete it when it
    /// needs something typed after it.
    fn run_menu(&mut self, command: &'static Command) -> Vec<Effect> {
        if command.needs_args {
            self.complete(command);
            return Vec::new();
        }
        self.chat.input.clear();
        self.typed();
        let mut effects = self.remember(&format!("/{}", command.name));
        effects.extend(self.run_command(command.cmd, ""));
        effects
    }

    /// Enter: run a command, or ask.
    fn submit(&mut self) -> Vec<Effect> {
        let raw = self.chat.input.text().trim().to_string();
        if raw.is_empty() {
            return Vec::new();
        }
        let Some((name, args)) = commands::parse(&raw) else {
            // `//` asks something that starts with a slash.
            let text = raw.strip_prefix('/').unwrap_or(&raw).to_string();
            return self.send(text, raw);
        };
        let Some(command) = commands::find(name) else {
            self.notice(
                super::theme::Signal::Attention,
                &format!(
                    "There's no /{name} command. /help lists them; start with // to ask \
                     something that begins with a slash."
                ),
            );
            return Vec::new();
        };
        let args = args.to_string();
        self.chat.input.clear();
        self.typed();
        let mut effects = self.remember(&raw);
        effects.extend(self.run_command(command.cmd, &args));
        effects
    }

    /// Keep what was entered for Up, here and in the history file.
    fn remember(&mut self, entry: &str) -> Vec<Effect> {
        if self.chat.history.push(entry) {
            vec![Effect::Remember(entry.to_string())]
        } else {
            Vec::new()
        }
    }

    pub fn run_command(&mut self, cmd: Cmd, args: &str) -> Vec<Effect> {
        use super::theme::Signal;
        let needs_chats = matches!(
            cmd,
            Cmd::New
                | Cmd::Resume
                | Cmd::Search
                | Cmd::Model
                | Cmd::Project
                | Cmd::Attach
                | Cmd::Copy
                | Cmd::Retry
                | Cmd::Branch
                | Cmd::Rename
                | Cmd::Delete
                | Cmd::ShareChat
                | Cmd::UnshareChat
        );
        if needs_chats && !self.chat.ready() {
            self.notice(Signal::Attention, "Chats aren't available here right now.");
            return Vec::new();
        }
        match cmd {
            Cmd::New => {
                self.chat.project = None;
                self.chat.filter = None;
                return self.new_chat();
            }
            Cmd::Resume => self.open_picker(PickerKind::Chats, args),
            Cmd::Search => {
                self.focus = Focus::Chats;
                self.view = View::Chat;
                self.chat.search = Some(args.to_string());
                self.chat.selected = usize::from(!self.chat.visible().is_empty());
            }
            Cmd::Model => return self.pick_model(args),
            Cmd::Project => return self.pick_project(args),
            Cmd::Attach => return self.attach(args),
            Cmd::Detach => {
                self.chat.attachments.clear();
                self.notice(Signal::Idle, "No files attached.");
            }
            Cmd::Copy => match self.chat.last_answer() {
                Some(answer) => {
                    let answer = answer.to_string();
                    self.notice(Signal::Positive, "Copied the last answer.");
                    return vec![Effect::Copy(answer)];
                }
                None => self.notice(Signal::Idle, "There's no answer to copy yet."),
            },
            Cmd::Open => return self.open_in_browser(),
            Cmd::Retry
            | Cmd::Branch
            | Cmd::Rename
            | Cmd::Delete
            | Cmd::ShareChat
            | Cmd::UnshareChat => {
                return self.chat_action(cmd, args);
            }
            Cmd::Steps => self.chat.show_steps = !self.chat.show_steps,
            Cmd::Folders => self.settings(Page::Folders),
            Cmd::Settings => self.settings(self.attention_page()),
            Cmd::Share if args.is_empty() => {
                self.modal = Some(Modal::AddRoot {
                    input: self.default_folder(),
                });
            }
            Cmd::Share => return self.add_root(args.to_string(), None, false),
            Cmd::Unshare => return self.unshare(args),
            Cmd::Pause if self.daemon.paused() == Some(true) => {
                self.notice(
                    Signal::Idle,
                    "Already paused. /resume-sharing answers again.",
                );
            }
            Cmd::ResumeSharing if self.daemon.paused() == Some(false) => {
                self.notice(Signal::Idle, "Already answering Chat with Work.");
            }
            Cmd::Pause | Cmd::ResumeSharing => return self.toggle_pause(),
            Cmd::Log => {
                self.settings(Page::Activity);
                self.log_scroll = 0;
            }
            Cmd::Status => {
                let status = self.status_sentence();
                self.notice(Signal::Idle, &status);
            }
            Cmd::Login if self.can_pair() && self.pairing.is_none() => {
                self.pairing = Some(Pairing::Starting);
                return vec![Effect::Daemon(DaemonCommand::Pair)];
            }
            Cmd::Login if self.pairing.is_some() => {
                self.notice(Signal::Idle, "Pairing already: approve it in the browser.");
            }
            Cmd::Login => match self.daemon.server() {
                Some(server) => {
                    let host = host(server);
                    self.notice(
                        Signal::Idle,
                        &format!("Paired with {host} already. /logout first to pair again."),
                    );
                }
                None => self.notice(Signal::Idle, "Start the daemon first: press s."),
            },
            Cmd::Logout if self.daemon.server().is_none() => {
                self.notice(Signal::Idle, "This computer isn't paired.");
            }
            Cmd::Logout => self.modal = Some(Modal::ConfirmLogout),
            Cmd::Help => self.modal = Some(Modal::Help { scroll: 0 }),
            Cmd::Exit => {
                self.quit = true;
                return vec![Effect::Quit];
            }
        }
        Vec::new()
    }

    /// `/status`: how this computer is connected, in a sentence.
    fn status_sentence(&self) -> String {
        let roots = self.daemon.roots().len();
        let shared = match roots {
            0 => "nothing shared".to_string(),
            1 => "1 folder shared".to_string(),
            n => format!("{n} folders shared"),
        };
        match &self.daemon {
            Daemon::Unknown => "Still looking for the daemon.".into(),
            Daemon::NotRunning(offline) => {
                let paired = match &offline.server {
                    Some(server) => format!("paired with {}", host(server)),
                    None => "not paired".into(),
                };
                format!("The daemon isn't running ({paired}, {shared}).")
            }
            Daemon::Running(status) => {
                let link = match status.connection.connection.as_str() {
                    "connected" => "Connected to",
                    "connecting" => "Connecting to",
                    "offline" => "Can't reach",
                    "revoked" => "Revoked by",
                    _ => "Not paired with",
                };
                let server = status.server.as_deref().map(host).unwrap_or_default();
                let mut text = if server.is_empty() {
                    "Not paired".to_string()
                } else {
                    format!("{link} {server}")
                };
                if let Some(device) = &status.device_id {
                    text.push_str(&format!(" as device {device}"));
                }
                let answering = if status.paused { "paused" } else { "answering" };
                format!("{text} · {shared} · {answering} · cww {}", status.version)
            }
        }
    }

    fn open_picker(&mut self, kind: PickerKind, query: &str) {
        self.view = View::Chat;
        self.focus = Focus::Composer;
        self.chat.picker = Some(Picker {
            kind,
            query: query.to_string(),
            selected: 0,
        });
    }

    /// `/model`: the list of models, loaded the first time.
    fn pick_model(&mut self, query: &str) -> Vec<Effect> {
        use super::theme::Signal;
        match &self.chat.models {
            ModelList::Unavailable(why) => {
                let why = why.clone();
                self.notice(Signal::Idle, &why);
                return Vec::new();
            }
            ModelList::Ready(models) if !query.is_empty() => {
                let q = query.to_lowercase();
                let found: Vec<&Model> = models
                    .models
                    .iter()
                    .filter(|m| m.id == query || m.name.to_lowercase().contains(&q))
                    .collect();
                if let [model] = found[..] {
                    let model = model.clone();
                    self.choose_model(&model);
                    return Vec::new();
                }
            }
            _ => {}
        }
        self.open_picker(PickerKind::Models, query);
        if matches!(self.chat.models, ModelList::Unknown) {
            self.chat.models = ModelList::Loading;
            return vec![Effect::Chat(ChatCommand::Models)];
        }
        Vec::new()
    }

    fn choose_model(&mut self, model: &Model) {
        use super::theme::Signal;
        if !model.selectable {
            let why = model
                .reason
                .clone()
                .unwrap_or_else(|| format!("{} can't be used right now.", model.name));
            self.notice(Signal::Attention, &why);
            return;
        }
        self.chat.picker = None;
        self.chat.model = Some(ModelChoice {
            chat: self.chat.open,
            id: model.id.clone(),
            name: model.name.clone(),
        });
        let what = if self.chat.open.is_some() {
            "your next question here"
        } else {
            "the new chat"
        };
        self.notice(Signal::Positive, &format!("{} answers {what}.", model.name));
    }

    /// `/project`: the next new chat starts in a project.
    fn pick_project(&mut self, query: &str) -> Vec<Effect> {
        use super::theme::Signal;
        if self.chat.list.projects.is_empty() {
            self.notice(Signal::Idle, "You aren't in any projects.");
            return Vec::new();
        }
        let q = query.to_lowercase();
        if q == "none" {
            self.chat.project = None;
            self.notice(Signal::Idle, "New chats start outside projects.");
            return Vec::new();
        }
        if !q.is_empty() {
            let found: Vec<Project> = self
                .chat
                .list
                .projects
                .iter()
                .filter(|p| p.name.to_lowercase().contains(&q))
                .cloned()
                .collect();
            if let [project] = &found[..] {
                return self.choose_project(Some(project.clone()));
            }
        }
        self.open_picker(PickerKind::Projects, query);
        Vec::new()
    }

    fn choose_project(&mut self, project: Option<Project>) -> Vec<Effect> {
        use super::theme::Signal;
        self.chat.picker = None;
        let effects = if self.chat.open.is_some() {
            self.new_chat()
        } else {
            Vec::new()
        };
        match &project {
            Some(p) => self.notice(
                Signal::Positive,
                &format!("Your next question starts a chat in {}.", p.name),
            ),
            None => self.notice(Signal::Idle, "New chats start outside projects."),
        }
        self.chat.project = project;
        effects
    }

    /// Keys while a picker is open: typing filters it.
    fn picker_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.chat.pick_items().len();
        if key.code == KeyCode::Enter {
            let selected = self.chat.picker.as_ref().map_or(0, |p| p.selected);
            return self.pick(selected);
        }
        let Some(picker) = self.chat.picker.as_mut() else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Esc => self.chat.picker = None,
            KeyCode::Up if len > 0 => picker.selected = (picker.selected + len - 1) % len,
            KeyCode::Down if len > 0 => picker.selected = (picker.selected + 1) % len,
            KeyCode::Backspace => {
                picker.query.pop();
                picker.selected = 0;
            }
            KeyCode::Char('u') if ctrl => {
                picker.query.clear();
                picker.selected = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                picker.query.push(c);
                picker.selected = 0;
            }
            KeyCode::PageUp => self.scroll_transcript(10),
            KeyCode::PageDown => self.scroll_transcript(-10),
            _ => {}
        }
        Vec::new()
    }

    /// Pick row `index` of the open picker.
    fn pick(&mut self, index: usize) -> Vec<Effect> {
        let item = self.chat.pick_items().get(index).copied();
        let kind = self.chat.picker.as_ref().map(|p| p.kind);
        match item {
            Some(PickItem::Chat(chat)) => {
                let number = chat.number;
                self.chat.picker = None;
                self.select_chat(number);
                self.open_chat(number)
            }
            Some(PickItem::Model(model)) => {
                let model = model.clone();
                self.choose_model(&model);
                Vec::new()
            }
            Some(PickItem::Project(project)) => {
                let project = project.cloned();
                self.choose_project(project)
            }
            None => {
                if kind != Some(PickerKind::Models)
                    || !matches!(self.chat.models, ModelList::Loading)
                {
                    self.chat.picker = None;
                }
                Vec::new()
            }
        }
    }

    /// `/attach <path>`: upload a file for the next question.
    fn attach(&mut self, path: &str) -> Vec<Effect> {
        use super::theme::Signal;
        let path = path.trim().trim_matches(['"', '\'']).to_string();
        if path.is_empty() {
            self.notice(
                Signal::Idle,
                "Say which file: /attach ~/Documents/notes.pdf",
            );
            return Vec::new();
        }
        if self.chat.attachments.iter().any(|a| a.path == path) {
            self.notice(Signal::Idle, "That file is attached already.");
            return Vec::new();
        }
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        self.chat.attachments.push(Attached {
            path: path.clone(),
            name,
            uploaded: None,
        });
        vec![Effect::Chat(ChatCommand::Upload(path))]
    }

    /// `/unshare [folder]`: by ID, label or path, else the selected one.
    fn unshare(&mut self, which: &str) -> Vec<Effect> {
        if which.is_empty() {
            self.confirm_remove();
            return Vec::new();
        }
        let found = self.daemon.roots().iter().find(|r| {
            r.id == which || r.label.eq_ignore_ascii_case(which) || r.local_path == which
        });
        match found {
            Some(root) => {
                self.modal = Some(Modal::ConfirmRemove {
                    id: root.id.clone(),
                    label: root.label.clone(),
                    path: root.local_path.clone(),
                });
            }
            None => self.notice(
                super::theme::Signal::Attention,
                &format!("No shared folder is called {which}."),
            ),
        }
        Vec::new()
    }

    /// `/retry`, `/branch`, `/rename`, `/delete` and `/share-chat`, where
    /// the server allows them for the open chat.
    fn chat_action(&mut self, cmd: Cmd, args: &str) -> Vec<Effect> {
        use super::theme::Signal;
        let verb = match cmd {
            Cmd::Retry => "retry",
            Cmd::Branch => "branch",
            Cmd::Rename => "rename",
            Cmd::Delete => "delete",
            _ => "share",
        };
        let Some(chat) = self.chat.open_summary().cloned() else {
            self.notice(Signal::Idle, &format!("Open a chat to {verb} it."));
            return Vec::new();
        };
        let allowed = match (cmd, chat.can) {
            // Stopping works on any chat with a link, whatever else it allows.
            (Cmd::UnshareChat, _) => Some(chat.share.is_some()),
            (_, None) => None,
            (Cmd::Retry, Some(can)) => Some(can.retry),
            (Cmd::Branch, Some(can)) => Some(can.branch),
            (Cmd::Rename, Some(can)) => Some(can.rename),
            (Cmd::Delete, Some(can)) => Some(can.delete),
            (_, Some(can)) => Some(can.share),
        };
        match allowed {
            None => {
                self.notice(
                    Signal::Idle,
                    &format!("This Chat with Work server can't {verb} chats from here yet."),
                );
                return Vec::new();
            }
            Some(false) if cmd == Cmd::UnshareChat => {
                self.notice(Signal::Idle, "This chat has no public link.");
                return Vec::new();
            }
            Some(false) => {
                self.notice(Signal::Idle, &format!("You can't {verb} this chat."));
                return Vec::new();
            }
            Some(true) => {}
        }
        let number = chat.number;
        let action = match cmd {
            Cmd::Retry if self.chat.working() => {
                self.notice(Signal::Idle, "Wait for the answer to finish first.");
                return Vec::new();
            }
            Cmd::Retry => {
                self.notice(Signal::Idle, "Answering again…");
                ChatAction::Retry
            }
            Cmd::Branch => {
                self.notice(Signal::Idle, "Branching…");
                ChatAction::Branch
            }
            Cmd::Rename if args.trim().is_empty() => {
                // Edit the title where it's typed.
                self.chat.input.set(format!("/rename {}", chat.title));
                return Vec::new();
            }
            Cmd::Rename => ChatAction::Rename(args.trim().to_string()),
            Cmd::Delete => {
                self.modal = Some(Modal::ConfirmDelete {
                    chat: number,
                    title: chat.title.clone(),
                });
                return Vec::new();
            }
            Cmd::UnshareChat => {
                self.notice(Signal::Idle, "Stopping sharing…");
                ChatAction::Unshare
            }
            _ => {
                self.notice(Signal::Idle, "Sharing…");
                ChatAction::Share
            }
        };
        vec![Effect::Chat(ChatCommand::Act {
            chat: number,
            action,
        })]
    }

    /// Keys for the change waiting for approval, or `None` when there's
    /// none or the key isn't the prompt's.
    fn decision_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        // A slash command can still be typed and run.
        if self.chat.reason || !self.chat.input.is_empty() || key.code == KeyCode::Char('/') {
            return None;
        }
        let approval = self.chat.pending_decision()?.clone();
        let choices = ChatPane::choices(&approval);
        let len = choices.len();
        let selected = self.chat.decision.min(len - 1);
        let choice = match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.chat.decision = (selected + len - 1) % len;
                return Some(Vec::new());
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.chat.decision = (selected + 1) % len;
                return Some(Vec::new());
            }
            KeyCode::Enter => choices[selected],
            KeyCode::Char('y') => Choice::Approve,
            KeyCode::Char('a') if approval.allow_for_rest_of_chat => Choice::ApproveAll,
            KeyCode::Char('n') => Choice::Deny,
            KeyCode::Char(c @ '1'..='9') => *choices.get(c as usize - '1' as usize)?,
            KeyCode::Esc
            | KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::PageUp
            | KeyCode::PageDown => return None,
            // Nothing else is typed while the prompt waits.
            _ => return Some(Vec::new()),
        };
        Some(self.decide(choice))
    }

    /// Answer the change waiting for approval.
    pub fn decide(&mut self, choice: Choice) -> Vec<Effect> {
        let (Some(chat), Some(approval)) = (self.chat.open, self.chat.pending_decision()) else {
            return Vec::new();
        };
        if self.chat.deciding {
            return Vec::new();
        }
        let tool_call = approval.id;
        let command = match choice {
            Choice::Approve | Choice::ApproveAll => ChatCommand::Approve {
                chat,
                tool_call,
                for_rest_of_chat: choice == Choice::ApproveAll,
            },
            Choice::Deny => ChatCommand::Deny {
                chat,
                tool_call,
                reason: None,
            },
            Choice::DenyWithReason => {
                self.chat.reason = true;
                self.chat.input.clear();
                return Vec::new();
            }
        };
        self.chat.deciding = true;
        vec![Effect::Chat(command)]
    }

    /// Enter while saying what to do instead: deny with it.
    fn deny_with_reason(&mut self) -> Vec<Effect> {
        let (Some(chat), Some(approval)) = (self.chat.open, self.chat.pending_decision()) else {
            self.chat.reason = false;
            return Vec::new();
        };
        if self.chat.deciding {
            return Vec::new();
        }
        let tool_call = approval.id;
        let reason = self.chat.input.take().trim().to_string();
        self.chat.reason = false;
        self.chat.deciding = true;
        vec![Effect::Chat(ChatCommand::Deny {
            chat,
            tool_call,
            reason: (!reason.is_empty()).then_some(reason),
        })]
    }

    /// Keys for a question from a tool's server, or `None` when there's
    /// none or the key isn't the form's.
    fn question_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        if self.chat.reason
            || self.chat.form.editing.is_some()
            || !self.chat.input.is_empty()
            || key.code == KeyCode::Char('/')
        {
            return None;
        }
        let question = self.chat.pending_question()?.clone();
        let rows = ChatPane::question_rows(&question);
        let len = rows.len();
        let row = self.chat.form.row.min(len - 1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.chat.form.row = (row + len - 1) % len,
            KeyCode::Down | KeyCode::Char('j') => self.chat.form.row = (row + 1) % len,
            KeyCode::Left => self.step_field(&question, row, -1),
            KeyCode::Right | KeyCode::Char(' ') => self.step_field(&question, row, 1),
            KeyCode::Enter => return Some(self.activate_row(rows[row])),
            KeyCode::Esc
            | KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::PageUp
            | KeyCode::PageDown => return None,
            _ => {}
        }
        Some(Vec::new())
    }

    /// Enter on a row of the question's form.
    pub fn activate_row(&mut self, row: FormRow) -> Vec<Effect> {
        let (Some(chat), Some(question)) = (self.chat.open, self.chat.pending_question().cloned())
        else {
            return Vec::new();
        };
        if self.chat.deciding {
            return Vec::new();
        }
        self.focus = Focus::Composer;
        match row {
            FormRow::Field(i) => {
                let Some(field) = question.fields.get(i) else {
                    return Vec::new();
                };
                if field_has_choices(field) {
                    self.step_field(&question, i, 1);
                } else {
                    // Type it in the composer; Enter keeps it.
                    let value = self.chat.form.values.get(i).cloned().unwrap_or_default();
                    self.chat.form.editing = Some(i);
                    self.chat.input.set(value);
                }
                Vec::new()
            }
            FormRow::OpenPage => match question
                .url
                .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
            {
                Some(url) => vec![Effect::Chat(ChatCommand::OpenUrl(url))],
                None => Vec::new(),
            },
            FormRow::Send => {
                let input = if question.is_url() {
                    None
                } else {
                    Some(self.chat.form.input(&question))
                };
                self.chat.deciding = true;
                vec![Effect::Chat(ChatCommand::Answer {
                    chat,
                    tool_call: question.id,
                    input,
                })]
            }
            FormRow::Decline => {
                self.chat.deciding = true;
                vec![Effect::Chat(ChatCommand::Decline {
                    chat,
                    tool_call: question.id,
                })]
            }
        }
    }

    /// Left and Right go through a field's choices (yes and no for a
    /// yes-or-no field).
    fn step_field(&mut self, question: &super::chat::Question, row: usize, delta: isize) {
        let Some(FormRow::Field(i)) = ChatPane::question_rows(question).get(row).copied() else {
            return;
        };
        let field = &question.fields[i];
        let options = field_options(field);
        if options.is_empty() {
            return;
        }
        let form = &mut self.chat.form;
        if form.values.len() < question.fields.len() {
            form.values.resize(question.fields.len(), String::new());
        }
        let at = options.iter().position(|o| *o == form.values[i]);
        let next = match at {
            None if delta > 0 => 0,
            None => options.len() - 1,
            Some(at) => (at as isize + delta).rem_euclid(options.len() as isize) as usize,
        };
        form.values[i] = options[next].clone();
    }

    /// Enter after typing a field's value: keep it.
    fn save_field(&mut self) {
        let Some(i) = self.chat.form.editing.take() else {
            return;
        };
        let value = self.chat.input.take().trim().to_string();
        let len = self.chat.pending_question().map_or(0, |q| q.fields.len());
        let form = &mut self.chat.form;
        if form.values.len() < len {
            form.values.resize(len, String::new());
        }
        if let Some(slot) = form.values.get_mut(i) {
            *slot = value;
        }
        // On to the next row.
        form.row = i + 1;
    }

    fn send(&mut self, text: String, entered: String) -> Vec<Effect> {
        use super::theme::Signal;
        let files = !self.chat.attachments.is_empty();
        if (text.is_empty() && !files) || !self.chat.ready() || self.chat.working() {
            return Vec::new();
        }
        if let Some(reason) = self.chat.locked_reason().map(str::to_string) {
            self.notice(Signal::Attention, &reason);
            return Vec::new();
        }
        if let Some(waiting) = self.chat.attachments.iter().find(|a| a.uploaded.is_none()) {
            let name = waiting.name.clone();
            self.notice(
                Signal::Idle,
                &format!("Wait a moment: {name} is still uploading."),
            );
            return Vec::new();
        }
        let mut effects = self.remember(&entered);
        self.chat.input.clear();
        self.typed();
        self.chat.error = None;
        self.chat.sending = true;
        self.chat.pending_question = Some(text.clone());
        self.chat.scroll = 0;
        self.chat.pinned = true;
        let attachments = std::mem::take(&mut self.chat.attachments)
            .into_iter()
            .filter_map(|a| a.uploaded.map(|(id, _)| id))
            .collect();
        effects.push(Effect::Chat(ChatCommand::Send {
            chat: self.chat.open,
            text,
            model: self.chat.model_for_question(),
            project: if self.chat.open.is_none() {
                self.chat.project.as_ref().map(|p| p.id)
            } else {
                None
            },
            attachments,
        }));
        effects
    }

    fn paste(&mut self, text: &str) {
        match &mut self.modal {
            Some(Modal::AddRoot { input } | Modal::RenameRoot { input, .. }) => {
                input.push_str(text.replace(['\r', '\n'], " ").trim())
            }
            Some(_) => {}
            None if self.view == View::Chat && self.focus == Focus::Composer => {
                if let Some(picker) = self.chat.picker.as_mut() {
                    picker
                        .query
                        .push_str(text.replace(['\r', '\n'], " ").trim());
                    picker.selected = 0;
                } else {
                    // Pasted lines stay lines.
                    self.chat
                        .input
                        .insert_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
                    self.typed();
                }
            }
            None if self.view == View::Chat && self.chat.search.is_some() => {
                self.chat
                    .search
                    .get_or_insert_default()
                    .push_str(text.replace(['\r', '\n'], " ").trim());
            }
            None => {}
        }
    }

    /// A click or the wheel, on what the last frame drew there.
    fn mouse(&mut self, event: MouseEvent) -> Vec<Effect> {
        let up = match event.kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            MouseEventKind::Down(MouseButton::Left) => {
                return self.click(event.column, event.row);
            }
            _ => return Vec::new(),
        };
        let delta: isize = if up { -1 } else { 1 };
        if let Some(Modal::Help { scroll }) = self.modal {
            self.modal = Some(Modal::Help {
                scroll: scroll.saturating_add_signed(delta * 3),
            });
            return Vec::new();
        }
        if self.modal.is_some() {
            return Vec::new();
        }
        match self.hits.at(event.column, event.row).cloned() {
            Some(Hit::Transcript | Hit::Choice(_) | Hit::Ask(_) | Hit::Link(_)) => {
                self.scroll_transcript(-delta * 3);
            }
            Some(Hit::Log) => self.scroll_log(-delta * 3),
            Some(Hit::Chats | Hit::Chat(_) | Hit::NewChat | Hit::Project(_)) => {
                self.move_selection(delta);
            }
            Some(Hit::Root(_)) => self.move_root(delta),
            Some(Hit::Menu(_)) => {
                let len = self.menu().len();
                if len > 0 {
                    self.chat.menu = self.chat.menu.saturating_add_signed(delta).min(len - 1);
                }
            }
            Some(Hit::Pick(_)) => {
                let len = self.chat.pick_items().len();
                if let Some(picker) = self.chat.picker.as_mut()
                    && len > 0
                {
                    picker.selected = picker.selected.saturating_add_signed(delta).min(len - 1);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn click(&mut self, x: u16, y: u16) -> Vec<Effect> {
        if self.modal.is_some() {
            return Vec::new();
        }
        self.quit_armed = false;
        let Some(hit) = self.hits.at(x, y).cloned() else {
            return Vec::new();
        };
        self.notice = None;
        match hit {
            Hit::Page(page) => self.settings(page),
            Hit::Back => self.view = View::Chat,
            Hit::Settings => self.settings(self.attention_page()),
            Hit::Search if self.chat.ready() => {
                self.focus = Focus::Chats;
                if self.chat.search.is_none() {
                    self.chat.search = Some(String::new());
                }
            }
            Hit::NewChat if self.chat.ready() => {
                self.chat.search = None;
                return self.new_chat();
            }
            Hit::Chat(number) => {
                self.chat.search = None;
                self.select_chat(number);
                return self.open_chat(number);
            }
            Hit::Project(id) => {
                self.chat.search = None;
                return self.pick_filter(id);
            }
            Hit::Web(i) => {
                self.selected_link = i;
                return self.open_web(i);
            }
            Hit::AllProjects => return self.open_page(self.chat.list.links.projects.clone()),
            Hit::Root(i) => self.selected_root = i,
            Hit::Composer if self.chat.ready() => self.focus = Focus::Composer,
            Hit::Menu(i) => {
                if let Some(command) = self.menu().get(i).copied() {
                    self.chat.menu = i;
                    return self.run_menu(command);
                }
            }
            Hit::Pick(i) => {
                if let Some(picker) = self.chat.picker.as_mut() {
                    picker.selected = i;
                }
                return self.pick(i);
            }
            Hit::Ask(i) => {
                if let Some(question) = self.chat.pending_question() {
                    let rows = ChatPane::question_rows(question);
                    if let Some(row) = rows.get(i).copied() {
                        self.chat.form.row = i;
                        return self.activate_row(row);
                    }
                }
            }
            Hit::Choice(i) => {
                if let Some(approval) = self.chat.pending_decision() {
                    let choices = ChatPane::choices(approval);
                    if let Some(choice) = choices.get(i).copied() {
                        self.focus = Focus::Composer;
                        self.chat.decision = i;
                        return self.decide(choice);
                    }
                }
            }
            // A link the answer shows: only web pages open.
            Hit::Link(url) if url.starts_with("https://") || url.starts_with("http://") => {
                return vec![Effect::Chat(ChatCommand::OpenUrl(url))];
            }
            _ => {}
        }
        Vec::new()
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
        let len = self.chat.side_items().len();
        self.chat.selected = self.chat.selected.saturating_add_signed(delta).min(len - 1);
    }

    fn scroll_transcript(&mut self, delta: isize) {
        if std::mem::take(&mut self.chat.pinned) {
            self.chat.scroll = self.chat.pinned_scroll.get();
        }
        self.chat.scroll = self.chat.scroll.saturating_add_signed(delta).min(10_000);
    }

    fn open_selected(&mut self) -> Vec<Effect> {
        match self.chat.side_items().get(self.chat.selected).copied() {
            Some(SideItem::Chat(number) | SideItem::PinnedChat(number)) => self.open_chat(number),
            Some(SideItem::Project(id)) => self.pick_filter(id),
            _ => self.new_chat(),
        }
    }

    /// A project in the sidebar: show its chats and start a new chat in
    /// it, as the web's project page does; again, back to every chat.
    fn pick_filter(&mut self, id: u64) -> Vec<Effect> {
        if self.chat.filter == Some(id) {
            self.chat.keep_selection(|pane| pane.filter = None);
            if self.chat.open.is_none() {
                self.chat.project = None;
            }
            return Vec::new();
        }
        self.chat.keep_selection(|pane| pane.filter = Some(id));
        let effects = self.new_chat();
        // Back on the project's row, to pick one of its chats next.
        if let Some(at) = self
            .chat
            .side_items()
            .iter()
            .position(|i| *i == SideItem::Project(id))
        {
            self.chat.selected = at;
        }
        effects
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
        chat.pinned = false;
        chat.stopping = false;
        chat.loading = true;
        chat.stale = false;
        chat.following = Following::Starting;
        chat.picker = None;
        chat.reason = false;
        chat.deciding = false;
        chat.decision_for = None;
        chat.form = Form::default();
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
        chat.pinned = false;
        chat.loading = false;
        chat.stale = false;
        chat.stopping = false;
        chat.following = Following::No;
        chat.selected = 0;
        if let Some(project) = chat.filter_project().cloned() {
            chat.project = Some(project);
        }
        chat.picker = None;
        chat.reason = false;
        chat.deciding = false;
        chat.decision_for = None;
        chat.form = Form::default();
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
        if let Some(i) = self
            .chat
            .side_items()
            .iter()
            .position(|item| *item == SideItem::Chat(number))
        {
            self.chat.selected = i;
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

    /// A page of the web's settings, in the browser.
    fn open_web(&mut self, index: usize) -> Vec<Effect> {
        let url = self
            .chat
            .list
            .links
            .settings
            .get(index)
            .map(|l| l.url.clone());
        self.open_page(url)
    }

    /// A page of the paired server, in the browser; any other is ignored.
    fn open_page(&self, url: Option<String>) -> Vec<Effect> {
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
        if self.view == View::Settings(Page::Activity) {
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
            DaemonMsg::Deny(deny) => self.deny = Some(deny),
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
                self.chat.list_live = false;
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
        let first = self.chat.access == Access::Unknown;
        self.chat.access = Access::Loading;
        let mut effects = vec![Effect::Chat(ChatCommand::List)];
        if first {
            // A daemon (new, or back) keeps the list current from now on.
            effects.push(Effect::Chat(ChatCommand::FollowList(true)));
        }
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
                self.chat.access = Access::Ready;
                self.chat.set_list(list);
                // Allowed again (or for the first time): the server may have
                // refused to keep the list current until now.
                let follow_list = (!was_ready && !self.chat.list_live)
                    .then_some(Effect::Chat(ChatCommand::FollowList(true)));
                // Chats are back (allowed again, say): follow the open one
                // again, unless that's already underway. A list that works
                // after a refusal leaves it be: the refusal was about the
                // chat, and following again would only be refused again.
                let mut effects: Vec<Effect> = follow_list.into_iter().collect();
                if let Some(open) = self.chat.open
                    && !was_ready
                    && self.chat.following != Following::Starting
                {
                    self.chat.following = Following::Starting;
                    effects.push(Effect::Chat(ChatCommand::Follow(Some(open))));
                    effects.extend(self.refresh_chat(open));
                }
                return effects;
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
                        self.chat.upsert_chat(summary);
                        let mut effects = self.list_again();
                        if chat.is_none() && self.chat.open.is_none() {
                            // The first question made the chat: follow it,
                            // and the model picked for it goes with it.
                            if let Some(choice) =
                                self.chat.model.as_mut().filter(|c| c.chat.is_none())
                            {
                                choice.chat = Some(number);
                            }
                            self.chat.project = None;
                            self.chat.open = Some(number);
                            self.chat.following = Following::Starting;
                            self.chat.loading = true;
                            self.select_chat(number);
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
                            self.chat.input.set(question);
                        }
                        // A model that can't be used: back to the chat's own.
                        if failure.code == "model_unavailable" {
                            self.chat.model = None;
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
            ChatMsg::ListLive(live) => return self.list_live(live),
            // A daemon from before live lists, say: the list is read after
            // each change instead, as before.
            ChatMsg::ListFollowFailed(_) => self.chat.list_live = false,
            ChatMsg::FollowFailed { chat, failure } => {
                if self.chat.open == Some(chat) {
                    self.chat.following = Following::Refused;
                    if is_access_failure(&failure) {
                        self.chat_failure(failure);
                    }
                }
            }
            ChatMsg::Models(Ok(models)) => {
                self.chat.models = ModelList::Ready(models);
                if let Some(picker) = self.chat.picker.as_mut() {
                    picker.selected = 0;
                }
            }
            ChatMsg::Models(Err(failure)) => {
                let why = match failure.code.as_str() {
                    "unsupported" => Some(failure.message.clone()),
                    "daemon_outdated" => Some(
                        "The daemon running now is older than this cww and can't list models. \
                         Restart it with this version of cww."
                            .to_string(),
                    ),
                    _ => None,
                };
                if self.chat.picker.as_ref().map(|p| p.kind) == Some(PickerKind::Models) {
                    self.chat.picker = None;
                }
                match why {
                    // Chats go on with their models; only the choice is missing.
                    Some(why) => {
                        self.notice(Signal::Idle, &why);
                        self.chat.models = ModelList::Unavailable(why);
                    }
                    None => {
                        self.chat.models = ModelList::Unknown;
                        self.notice(Signal::Negative, &failure.message);
                    }
                }
            }
            ChatMsg::Decided { chat, result } => {
                self.chat.deciding = false;
                let decided = self.chat.decision_for;
                match result {
                    Ok(summary) => {
                        self.chat.form = Form::default();
                        self.chat.decision = 0;
                        self.settle(decided);
                        self.update_summary(summary);
                        return self.refresh_chat(chat);
                    }
                    // Decided already (a second press, or someone else first):
                    // settled, not an error. The read shows how.
                    Err(failure)
                        if failure.code == "already_decided" || failure.code == "not_found" =>
                    {
                        self.settle(decided);
                        return self.refresh_chat(chat);
                    }
                    Err(failure) => self.notice(Signal::Negative, &failure.message),
                }
            }
            ChatMsg::Uploaded { path, result } => {
                let at = self
                    .chat
                    .attachments
                    .iter()
                    .position(|a| a.path == path && a.uploaded.is_none());
                match (at, result) {
                    (Some(at), Ok(uploaded)) => {
                        let attached = &mut self.chat.attachments[at];
                        attached.name = uploaded.filename.clone();
                        attached.uploaded = Some((uploaded.signed_id, uploaded.byte_size));
                    }
                    (Some(at), Err(failure)) => {
                        self.chat.attachments.remove(at);
                        self.notice(Signal::Negative, &failure.message);
                    }
                    // Detached while it uploaded.
                    (None, _) => {}
                }
            }
            ChatMsg::Acted {
                chat,
                action,
                result,
            } => return self.acted(chat, action, result),
        }
        Vec::new()
    }

    /// Read the list again after a change made here, unless the server
    /// keeps it current anyway.
    fn list_again(&self) -> Vec<Effect> {
        if self.chat.list_live {
            Vec::new()
        } else {
            vec![Effect::Chat(ChatCommand::List)]
        }
    }

    /// A change to the chat list, as the server keeps it current.
    fn list_live(&mut self, live: ListLive) -> Vec<Effect> {
        use super::theme::Signal;
        match live {
            ListLive::Watching => {
                self.chat.list_live = true;
                // Catch up on what changed before the subscription.
                if !matches!(self.chat.access, Access::Unknown | Access::Loading) {
                    return vec![Effect::Chat(ChatCommand::List)];
                }
            }
            ListLive::Missed => return vec![Effect::Chat(ChatCommand::List)],
            ListLive::Refused | ListLive::Unsupported | ListLive::Offline => {
                self.chat.list_live = false;
            }
            // Changes only apply to a list that was read.
            _ if !self.chat.ready() => {}
            ListLive::Chat(chat) => {
                let reread = self.chat.open == Some(chat.number)
                    && self
                        .chat
                        .open_summary()
                        .is_some_and(|open| open.state != chat.state);
                let number = chat.number;
                self.chat.upsert_chat(chat);
                // The open chat's follower says what changed, unless it
                // can't follow it.
                if reread && self.chat.following != Following::Live {
                    return self.refresh_chat(number);
                }
            }
            ListLive::Removed(number) => {
                self.chat.remove_chat(number);
                if self.chat.open == Some(number) {
                    self.notice(
                        Signal::Attention,
                        "This chat was deleted, or you can't see it anymore.",
                    );
                    return self.new_chat();
                }
            }
            ListLive::Projects(projects) => self.chat.set_projects(projects),
            ListLive::Account(account) => self.chat.list.set_account(account),
        }
        Vec::new()
    }

    /// A chat action finished.
    fn acted(
        &mut self,
        chat: u64,
        action: ChatAction,
        result: Result<Acted, Failure>,
    ) -> Vec<Effect> {
        use super::theme::Signal;
        let acted = match result {
            Ok(acted) => acted,
            Err(failure) => {
                self.notice(Signal::Negative, &failure.message);
                return Vec::new();
            }
        };
        match (action, acted) {
            (ChatAction::Branch, Acted::Chat(branch)) => {
                let number = branch.number;
                self.chat.upsert_chat(branch);
                self.notice(Signal::Positive, "Branched into a new chat.");
                self.select_chat(number);
                let mut effects = self.open_chat(number);
                effects.extend(self.list_again());
                effects
            }
            (ChatAction::Delete, _) => {
                let title = self
                    .chat
                    .list
                    .chats
                    .iter()
                    .find(|c| c.number == chat)
                    .map(|c| c.title.clone())
                    .unwrap_or_else(|| format!("#{chat}"));
                self.chat.remove_chat(chat);
                self.notice(Signal::Positive, &format!("Deleted {title}."));
                if self.chat.open == Some(chat) {
                    return self.new_chat();
                }
                Vec::new()
            }
            (_, Acted::Shared(shared)) => {
                let url = shared.url.clone();
                self.update_summary(shared.chat);
                self.notice(
                    Signal::Positive,
                    &format!("Anyone with the link can read this chat for 30 days. Copied: {url}"),
                );
                vec![Effect::Copy(url)]
            }
            (action, Acted::Chat(summary)) => {
                let text = match action {
                    ChatAction::Retry => "",
                    ChatAction::Rename(_) => "Renamed.",
                    ChatAction::Unshare => "The link stopped working.",
                    _ => "",
                };
                if !text.is_empty() {
                    self.notice(Signal::Positive, text);
                } else if action == ChatAction::Retry {
                    self.notice = None;
                }
                self.update_summary(summary);
                self.refresh_chat(chat)
            }
            (_, Acted::Deleted) => Vec::new(),
        }
    }

    /// A chat as the server last answered with it, in the list and the
    /// open transcript.
    /// A decided change or answered question leaves its card at once, in
    /// place; the read that follows fills in what came of it.
    fn settle(&mut self, decided: Option<u64>) {
        let Some(id) = decided else { return };
        if let Some(transcript) = self.chat.transcript.as_mut() {
            transcript.approvals.retain(|a| a.id != id);
            transcript.questions.retain(|q| q.id != id);
        }
        self.chat.decision_for = None;
    }

    fn update_summary(&mut self, summary: ChatSummary) {
        if let Some(listed) = self
            .chat
            .list
            .chats
            .iter_mut()
            .find(|c| c.number == summary.number)
        {
            *listed = summary.clone();
        }
        if let Some(transcript) = self
            .chat
            .transcript
            .as_mut()
            .filter(|t| t.chat.number == summary.number)
        {
            transcript.chat = summary;
        }
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
        let waiting = transcript
            .next_decision()
            .map(|a| a.id)
            .or_else(|| transcript.next_question().map(|q| q.id));
        if chat.decision_for != waiting {
            chat.decision_for = waiting;
            chat.decision = 0;
            chat.reason = false;
            chat.form = Form {
                question: transcript.next_question().map(|q| q.id),
                values: transcript
                    .next_question()
                    .map(|q| q.fields.iter().map(default_value).collect())
                    .unwrap_or_default(),
                ..Form::default()
            };
        }
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
        self.focus = Focus::Chats;
    }
}

/// A field's default, as the form shows it.
fn default_value(field: &super::chat::Field) -> String {
    match &field.default {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Bool(b)) => if *b { "yes" } else { "no" }.into(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

/// `chatwithwork.com`, from `https://chatwithwork.com/`.
fn host(url: &str) -> String {
    url.split_once("://")
        .map_or(url, |(_, rest)| rest)
        .trim_end_matches('/')
        .to_string()
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
