//! What the chat page knows, and how each answer from the daemon changes
//! it: the same rules as the terminal UI's chat pane (`cww::tui::app`), for
//! the same control API. It does no I/O; the commands it returns are run
//! by `runner.rs`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use cww::tui::chat::{
    AccessRequest, Ask, ChatList, ChatSummary, Entry, Failure, Field, ListLive, Live, Models,
    Question, Shared, Transcript, Uploaded,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Read the list of chats.
    List,
    /// Read one chat.
    Open(u64),
    /// Follow one chat live, or stop following.
    Follow(Option<u64>),
    /// Keep the list of chats current, starting over, or stop.
    FollowList(bool),
    /// Ask in a chat, or in a new one.
    Send(Ask),
    Cancel(u64),
    /// Read the models the composer's picker offers.
    Models,
    /// Upload a file picked for the next question.
    Upload {
        key: u64,
        path: PathBuf,
    },
    /// Settle a step the answer stopped at.
    Decide {
        chat: u64,
        decision: Decision,
    },
    /// Retry, branch, rename, delete, share or stop sharing a chat.
    Act {
        chat: u64,
        action: ChatAction,
    },
    /// Ask the owner to allow chats, then check back until they answer.
    RequestAccess,
    /// Check back until the owner answers a request made earlier.
    WaitForAccess,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
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
    Models(Result<Models, Failure>),
    Uploaded {
        key: u64,
        result: Result<Uploaded, Failure>,
    },
    Decided {
        chat: u64,
        result: Result<ChatSummary, Failure>,
    },
    Acted {
        chat: u64,
        action: ChatAction,
        result: Result<Acted, Failure>,
    },
    Live {
        chat: u64,
        live: Live,
    },
    /// Following a chat stopped with an error.
    FollowFailed {
        chat: u64,
        failure: Failure,
    },
    /// A change to the list, as the server keeps it current.
    ListLive(ListLive),
    /// Keeping the list current stopped: the daemon went away, or can't
    /// (an older one refuses the `chats` topic).
    ListFollowEnded(Option<Failure>),
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

/// A decision on a step the answer stopped at, as the web's cards make it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Approve {
        tool_call: u64,
        for_rest_of_chat: bool,
    },
    Deny {
        tool_call: u64,
        reason: Option<String>,
    },
    /// The form's values, or `None` for a page that was visited.
    Answer {
        tool_call: u64,
        input: Option<Value>,
    },
    Decline {
        tool_call: u64,
    },
}

impl Decision {
    pub fn tool_call(&self) -> u64 {
        match self {
            Decision::Approve { tool_call, .. }
            | Decision::Deny { tool_call, .. }
            | Decision::Answer { tool_call, .. }
            | Decision::Decline { tool_call } => *tool_call,
        }
    }
}

/// What can be done to a chat, where its `can` allows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAction {
    /// Answer `message` (a question or an answer) again.
    Retry(u64),
    /// A new chat with the conversation up to `message`.
    Branch(u64),
    Rename(String),
    Delete,
    Share,
    Unshare,
}

/// What a chat action answered with.
#[derive(Debug, Clone, PartialEq)]
pub enum Acted {
    Chat(ChatSummary),
    Shared(Shared),
    Deleted,
}

/// The models to pick from, as far as they're known.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ModelList {
    #[default]
    Unknown,
    Loading,
    Ready(Models),
    /// The server or the daemon can't offer the choice: chats keep theirs.
    Unavailable,
}

/// A model picked in the composer, for one chat (or the next new one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub chat: Option<u64>,
    pub id: String,
}

/// A file picked for the next question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    /// This page's own key for it.
    pub key: u64,
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub content_type: String,
    /// Its `signed_id` once it's uploaded.
    pub signed_id: Option<String>,
    /// Its file type's icon, as the server names it once it's uploaded.
    pub icon: Option<cww::tui::chat::Asset>,
}

/// A dialog over the page, as the web opens one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialog {
    /// "Retry this question?" or "Retry this answer?".
    Retry { chat: u64, message: u64, user: bool },
    /// The chat's share dialog: private, or its public link.
    Share { chat: u64, copied: bool },
    /// "Are you sure?" before deleting a chat.
    Delete { chat: u64 },
    /// "That file can't be attached".
    Unsupported { name: String },
}

/// One field of a question's form, as it's filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Text(String),
    Check(bool),
    /// For an `array` field: which of its choices are ticked.
    Picks(Vec<bool>),
}

/// What's filled in on an approval card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovalInput {
    /// "Deny with a reason" is open.
    pub reason_open: bool,
    pub reason: String,
    pub for_rest_of_chat: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatState {
    pub access: Access,
    pub list: ChatList,
    /// What's typed in the sidebar's search.
    pub search: String,
    /// The menu under the person's name is open.
    pub user_menu: bool,
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
    /// About the open chat: it's gone, or couldn't be read.
    pub error: Option<String>,
    /// A failure to show over the composer: a question that didn't go.
    pub notice: Option<String>,
    pub models: ModelList,
    /// The model picked in the composer, if another than the chat's.
    pub model: Option<ModelChoice>,
    /// Files picked for the next question.
    pub attachments: Vec<Attached>,
    next_key: u64,
    pub dialog: Option<Dialog>,
    /// The chat whose row menu is open in the sidebar.
    pub menu: Option<u64>,
    /// The chat being renamed in the sidebar, and its title so far.
    pub renaming: Option<(u64, String)>,
    /// A decision on this tool call is on its way.
    pub deciding: Option<u64>,
    /// A chat action under way, by chat.
    pub acting: Option<(u64, ChatAction)>,
    /// Text to put on the clipboard: a public link just made.
    pub clipboard: Option<String>,
    /// The daemon is running and paired, so chats can be asked for.
    paired: bool,
    /// The list is kept current by the server: no need to read it again
    /// after a change made here, or now and then while a chat runs.
    pub list_live: bool,
    /// A follower for the list was started and hasn't ended.
    list_followed: bool,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            access: Access::Unknown,
            list: ChatList::default(),
            search: String::new(),
            user_menu: false,
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
            notice: None,
            models: ModelList::Unknown,
            model: None,
            attachments: Vec::new(),
            next_key: 1,
            dialog: None,
            menu: None,
            renaming: None,
            deciding: None,
            acting: None,
            clipboard: None,
            paired: false,
            list_live: false,
            list_followed: false,
        }
    }
}

impl ChatState {
    pub fn ready(&self) -> bool {
        self.access == Access::Ready
    }

    /// The chats the search leaves, newest first.
    pub fn visible(&self) -> Vec<&ChatSummary> {
        let query = self.search.trim().trim_start_matches('#').to_lowercase();
        self.list
            .chats
            .iter()
            .filter(|chat| {
                query.is_empty()
                    || chat.title.to_lowercase().contains(&query)
                    || chat.number.to_string() == query
                    || chat
                        .project
                        .as_ref()
                        .is_some_and(|p| p.name.to_lowercase().contains(&query))
            })
            .collect()
    }

    /// A project the list knows, by id.
    pub fn project(&self, id: u64) -> Option<&cww::tui::chat::Project> {
        self.list.projects.iter().find(|p| p.id == id)
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

    /// The daemon's state changed: chats can be asked for while it runs
    /// and is paired.
    pub fn set_paired(&mut self, paired: bool) -> Vec<Command> {
        if paired == self.paired {
            return Vec::new();
        }
        self.paired = paired;
        if paired {
            if matches!(self.access, Access::Unknown | Access::Unavailable(_)) {
                self.access = Access::Loading;
                let mut commands = vec![Command::List];
                if let Some(open) = self.open {
                    self.following = Following::Starting;
                    commands.push(Command::Follow(Some(open)));
                    commands.extend(self.refresh(open));
                }
                return commands;
            }
            Vec::new()
        } else {
            self.access = Access::Unknown;
            let followed = self.following != Following::No;
            self.following = Following::No;
            self.sending = false;
            // Reads under way fail with the daemon; read again once it's back.
            self.loading = false;
            self.stale = false;
            let mut commands = Vec::new();
            if followed {
                commands.push(Command::Follow(None));
            }
            self.list_live = false;
            if std::mem::take(&mut self.list_followed) {
                commands.push(Command::FollowList(false));
            }
            commands
        }
    }

    /// Ask again after a failure: chats were allowed, the server is back.
    pub fn retry(&mut self) -> Vec<Command> {
        if !self.paired {
            return Vec::new();
        }
        self.access = Access::Loading;
        vec![Command::List]
    }

    /// Show a chat, read it, and follow it live.
    pub fn open_chat(&mut self, number: u64) -> Vec<Command> {
        if self.open == Some(number) && self.transcript.is_some() {
            return Vec::new();
        }
        self.open = Some(number);
        self.transcript = None;
        self.streamed.clear();
        self.progress = None;
        self.pending_question = None;
        self.error = None;
        self.notice = None;
        self.stopping = false;
        self.loading = true;
        self.stale = false;
        self.following = Following::Starting;
        self.leave_chat();
        vec![Command::Open(number), Command::Follow(Some(number))]
    }

    /// An empty conversation: the first question starts the chat.
    pub fn new_chat(&mut self) -> Vec<Command> {
        let followed = self.open.is_some();
        self.open = None;
        self.transcript = None;
        self.streamed.clear();
        self.progress = None;
        self.pending_question = None;
        self.error = None;
        self.notice = None;
        self.loading = false;
        self.stale = false;
        self.stopping = false;
        self.following = Following::No;
        self.leave_chat();
        if followed {
            vec![Command::Follow(None)]
        } else {
            Vec::new()
        }
    }

    /// What belongs to the chat that was open goes with it, as a page
    /// change does on the web.
    fn leave_chat(&mut self) {
        self.attachments.clear();
        self.dialog = None;
        self.deciding = None;
    }

    /// Whether what's typed can be sent now, or why not.
    pub fn can_send(&self) -> Result<(), &'static str> {
        if !self.ready() {
            Err("Chats aren't available")
        } else if self.working() {
            Err("Wait for the answer, or stop it")
        } else if self.locked_reason().is_some() {
            Err("Locked")
        } else if self.uploading() {
            Err("Wait for the files to upload")
        } else if self.input.trim().is_empty() && self.attachments.is_empty() {
            Err("Type a question")
        } else {
            Ok(())
        }
    }

    /// Send what's typed, with the model picked and the files attached.
    pub fn send(&mut self) -> Vec<Command> {
        if self.can_send().is_err() {
            return Vec::new();
        }
        let text = self.input.trim().to_string();
        self.input.clear();
        self.error = None;
        self.notice = None;
        self.sending = true;
        self.pending_question = Some(text.clone());
        let attachments = std::mem::take(&mut self.attachments)
            .into_iter()
            .filter_map(|a| a.signed_id)
            .collect();
        vec![Command::Send(Ask {
            chat: self.open,
            text,
            // A chat in a project starts on the project's page on the web.
            project: None,
            model: self.model_for_question(),
            attachments,
        })]
    }

    /// Files are still on their way up.
    pub fn uploading(&self) -> bool {
        self.attachments.iter().any(|a| a.signed_id.is_none())
    }

    /// The model picked for the next question here, if any.
    fn model_for_question(&self) -> Option<String> {
        self.model
            .as_ref()
            .filter(|choice| choice.chat == self.open)
            .map(|choice| choice.id.clone())
    }

    /// The model the composer's picker shows: the one picked here, else the
    /// chat's own, else (for a new chat) the default. Its ID and name.
    pub fn current_model(&self) -> Option<(String, String)> {
        let models = match &self.models {
            ModelList::Ready(models) => Some(models),
            _ => None,
        };
        let named = |id: &str| {
            models
                .and_then(|m| m.get(id))
                .map(|m| (m.id.clone(), m.name.clone()))
        };
        if let Some(choice) = self.model.as_ref().filter(|c| c.chat == self.open) {
            return named(&choice.id);
        }
        match self.open {
            Some(_) => self
                .open_summary()
                .and_then(|c| c.model.as_ref())
                .map(|m| named(&m.id).unwrap_or((m.id.clone(), m.name.clone()))),
            None => models
                .and_then(|m| m.default_model_id.as_deref())
                .and_then(named),
        }
    }

    /// Pick a model for the next question here. One that can't be used now
    /// says why instead.
    pub fn pick_model(&mut self, id: &str) {
        let ModelList::Ready(models) = &self.models else {
            return;
        };
        let Some(model) = models.get(id) else { return };
        if !model.selectable {
            self.notice = Some(
                model
                    .reason
                    .clone()
                    .unwrap_or_else(|| format!("{} can't be used right now.", model.name)),
            );
            return;
        }
        self.notice = None;
        self.model = Some(ModelChoice {
            chat: self.open,
            id: model.id.clone(),
        });
    }

    /// Attach files for the next question: each is checked as the web's
    /// composer checks it, then uploaded.
    pub fn attach(&mut self, paths: Vec<PathBuf>) -> Vec<Command> {
        let mut commands = Vec::new();
        if !self.ready() || self.locked_reason().is_some() {
            return commands;
        }
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            if self.attachments.iter().any(|a| a.path == path) {
                continue;
            }
            if !accepts(&name) {
                self.dialog = Some(Dialog::Unsupported { name });
                continue;
            }
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if size > cww::tui::chat::MAX_UPLOAD_BYTES {
                self.notice = Some(format!(
                    "{name} is larger than {} MB",
                    cww::tui::chat::MAX_UPLOAD_BYTES / (1024 * 1024)
                ));
                continue;
            }
            let key = self.next_key;
            self.next_key += 1;
            self.attachments.push(Attached {
                key,
                path: path.clone(),
                content_type: cww::tui::chat::content_type(&name).into(),
                name,
                size,
                signed_id: None,
                icon: None,
            });
            commands.push(Command::Upload { key, path });
        }
        commands
    }

    /// Take a file back off the next question.
    pub fn detach(&mut self, key: u64) {
        self.attachments.retain(|a| a.key != key);
    }

    /// Stop the answer being written.
    pub fn stop(&mut self) -> Vec<Command> {
        match self.open {
            Some(chat) if self.working() && !self.stopping => {
                self.stopping = true;
                vec![Command::Cancel(chat)]
            }
            _ => Vec::new(),
        }
    }

    /// Settle a step the open chat stopped at.
    pub fn decide(&mut self, decision: Decision) -> Vec<Command> {
        let Some(chat) = self.open else {
            return Vec::new();
        };
        if self.deciding.is_some() {
            return Vec::new();
        }
        self.deciding = Some(decision.tool_call());
        self.notice = None;
        vec![Command::Decide { chat, decision }]
    }

    /// Retry, branch, rename, delete, share or stop sharing `chat`.
    pub fn act(&mut self, chat: u64, action: ChatAction) -> Vec<Command> {
        if self.acting.is_some() {
            return Vec::new();
        }
        self.notice = None;
        self.acting = Some((chat, action.clone()));
        vec![Command::Act { chat, action }]
    }

    /// A chat as the server last answered with it, in the list and the
    /// open transcript.
    /// A decided change or answered question leaves its card at once, in
    /// place; the read that follows fills in what came of it.
    fn settle(&mut self, decided: Option<u64>) {
        let Some(id) = decided else { return };
        if let Some(transcript) = self.transcript.as_mut() {
            transcript.approvals.retain(|a| a.id != id);
            transcript.questions.retain(|q| q.id != id);
        }
    }

    fn update_summary(&mut self, summary: ChatSummary) {
        if let Some(listed) = self
            .list
            .chats
            .iter_mut()
            .find(|c| c.number == summary.number)
        {
            *listed = summary.clone();
        }
        if let Some(transcript) = self
            .transcript
            .as_mut()
            .filter(|t| t.chat.number == summary.number)
        {
            transcript.chat = summary;
        }
    }

    /// A chat's summary, as last read or listed.
    pub fn summary(&self, number: u64) -> Option<&ChatSummary> {
        self.transcript
            .as_ref()
            .map(|t| &t.chat)
            .filter(|c| c.number == number)
            .or_else(|| self.list.chats.iter().find(|c| c.number == number))
    }

    /// Ask the owner to let this computer use their chats.
    pub fn request_access(&mut self) -> Vec<Command> {
        if let Access::NeedsApproval {
            requested: false, ..
        } = self.access
        {
            self.access = Access::NeedsApproval {
                requested: true,
                url: None,
            };
            vec![Command::RequestAccess]
        } else {
            Vec::new()
        }
    }

    /// Read the open chat again, or once more after the read under way.
    fn refresh(&mut self, number: u64) -> Vec<Command> {
        if self.open != Some(number) {
            Vec::new()
        } else if self.loading {
            self.stale = true;
            Vec::new()
        } else {
            self.loading = true;
            vec![Command::Open(number)]
        }
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Command> {
        match msg {
            Msg::Listed(Ok(list)) => {
                let was_ready = self.ready();
                self.access = Access::Ready;
                self.list = list;
                let mut commands = Vec::new();
                if self.models == ModelList::Unknown {
                    self.models = ModelList::Loading;
                    commands.push(Command::Models);
                }
                // Chats are allowed (again): keep the list current from
                // now on, unless that's under way.
                if !was_ready && !self.list_followed {
                    self.list_followed = true;
                    commands.push(Command::FollowList(true));
                }
                // Chats are back (allowed again, say): follow the open one
                // again, unless that's already under way.
                if let Some(open) = self.open
                    && !was_ready
                    && self.following != Following::Starting
                {
                    self.following = Following::Starting;
                    commands.push(Command::Follow(Some(open)));
                    commands.extend(self.refresh(open));
                }
                return commands;
            }
            Msg::Listed(Err(failure)) => {
                self.failure(failure);
                // Asked already, maybe from another computer or the
                // terminal: check back until the owner answers.
                if matches!(
                    self.access,
                    Access::NeedsApproval {
                        requested: true,
                        ..
                    }
                ) {
                    return vec![Command::WaitForAccess];
                }
            }
            Msg::Shown { chat, result } => {
                if self.open != Some(chat) {
                    return Vec::new();
                }
                self.loading = false;
                match result {
                    Ok(transcript) => self.take_transcript(transcript),
                    Err(failure) if failure.code == "not_found" => {
                        self.error = Some("This chat is gone, or you can't see it anymore.".into());
                    }
                    Err(failure) if is_access_failure(&failure) => self.failure(failure),
                    Err(failure) => self.error = Some(failure.message),
                }
                if self.stale {
                    self.stale = false;
                    return self.refresh(chat);
                }
            }
            Msg::Sent { chat, result } => {
                self.sending = false;
                match result {
                    Ok(summary) => {
                        let number = summary.number;
                        self.upsert_chat(summary);
                        let mut commands = self.list_again();
                        if chat.is_none() && self.open.is_none() {
                            // The first question made the chat: follow it,
                            // and the model picked for it goes with it.
                            if let Some(choice) = self.model.as_mut().filter(|c| c.chat.is_none()) {
                                choice.chat = Some(number);
                            }
                            self.open = Some(number);
                            self.following = Following::Starting;
                            self.loading = true;
                            commands.push(Command::Follow(Some(number)));
                            commands.push(Command::Open(number));
                        } else {
                            commands.extend(self.refresh(number));
                        }
                        return commands;
                    }
                    Err(failure) => {
                        // Give the question back, to try again.
                        if let Some(question) = self.pending_question.take()
                            && self.input.is_empty()
                        {
                            self.input = question;
                        }
                        // A model that can't be used: back to the chat's own.
                        if failure.code == "model_unavailable" {
                            self.model = None;
                        }
                        if is_access_failure(&failure) {
                            self.failure(failure);
                        } else {
                            self.notice = Some(failure.message);
                        }
                    }
                }
            }
            Msg::Models(Ok(models)) => self.models = ModelList::Ready(models),
            // Chats go on with their models; only the choice is missing.
            Msg::Models(Err(_)) => self.models = ModelList::Unavailable,
            Msg::Uploaded { key, result } => {
                let at = self.attachments.iter().position(|a| a.key == key);
                match (at, result) {
                    (Some(at), Ok(uploaded)) => {
                        let attached = &mut self.attachments[at];
                        attached.name = uploaded.filename;
                        attached.size = uploaded.byte_size;
                        attached.content_type = uploaded.content_type;
                        attached.signed_id = Some(uploaded.signed_id);
                        attached.icon = uploaded.icon;
                    }
                    (Some(at), Err(failure)) => {
                        self.attachments.remove(at);
                        self.notice = Some(failure.message);
                    }
                    // Taken off while it uploaded.
                    (None, _) => {}
                }
            }
            Msg::Decided { chat, result } => {
                let decided = self.deciding.take();
                match result {
                    Ok(summary) => {
                        self.settle(decided);
                        self.update_summary(summary);
                        return self.refresh(chat);
                    }
                    // Decided already (a second press, or someone else first):
                    // settled, not an error. The read shows how.
                    Err(failure)
                        if failure.code == "already_decided" || failure.code == "not_found" =>
                    {
                        self.settle(decided);
                        return self.refresh(chat);
                    }
                    Err(failure) => self.notice = Some(failure.message),
                }
            }
            Msg::Acted {
                chat,
                action,
                result,
            } => {
                self.acting = None;
                return self.acted(chat, action, result);
            }
            Msg::Cancelled(Ok(())) => {}
            Msg::Cancelled(Err(failure)) => {
                self.stopping = false;
                self.notice = Some(failure.message);
            }
            Msg::Access(Ok(request)) if request.granted => {
                self.access = Access::Loading;
                return vec![Command::List];
            }
            Msg::Access(Ok(request)) => {
                self.access = Access::NeedsApproval {
                    requested: true,
                    url: request.approve_url,
                };
            }
            Msg::Access(Err(failure)) => self.failure(failure),
            Msg::Live { chat, live } => {
                if self.open != Some(chat) {
                    return Vec::new();
                }
                match live {
                    Live::Chunk { message_id, text } => {
                        self.following = Following::Live;
                        self.progress = None;
                        self.streamed.entry(message_id).or_default().push_str(&text);
                    }
                    Live::Progress(text) => self.progress = Some(text),
                    Live::Changed | Live::Watching => {
                        self.following = Following::Live;
                        return self.refresh(chat);
                    }
                    Live::Offline => self.following = Following::Offline,
                    // Maybe chats were taken back: the list says.
                    Live::Refused => {
                        self.following = Following::Refused;
                        if self.ready() {
                            return vec![Command::List];
                        }
                    }
                    Live::Unsupported => self.following = Following::Unsupported,
                }
            }
            Msg::FollowFailed { chat, failure } => {
                if self.open == Some(chat) {
                    self.following = Following::Refused;
                    if is_access_failure(&failure) {
                        self.failure(failure);
                    }
                }
            }
            Msg::ListLive(live) => return self.list_changed(live),
            Msg::ListFollowEnded(_) => {
                self.list_followed = false;
                self.list_live = false;
            }
        }
        Vec::new()
    }

    /// Read the list again after a change made here, unless the server
    /// keeps it current anyway.
    fn list_again(&self) -> Vec<Command> {
        if self.list_live {
            Vec::new()
        } else {
            vec![Command::List]
        }
    }

    /// A chat as the server last sent it: a new one goes in, another is
    /// replaced, and the list stays newest first.
    pub fn upsert_chat(&mut self, chat: ChatSummary) {
        let chats = &mut self.list.chats;
        chats.retain(|c| c.number != chat.number);
        let at = chats
            .iter()
            .position(|c| newer(&chat.updated_at, &c.updated_at))
            .unwrap_or(chats.len());
        chats.insert(at, chat.clone());
        if let Some(transcript) = self
            .transcript
            .as_mut()
            .filter(|t| t.chat.number == chat.number)
        {
            transcript.chat = chat;
        }
    }

    /// A chat that's gone from the list: deleted, or out of sight now.
    /// Open, it closes, saying so.
    fn remove_chat(&mut self, number: u64) -> Vec<Command> {
        self.list.chats.retain(|c| c.number != number);
        if self.menu == Some(number) {
            self.menu = None;
        }
        if self.renaming.as_ref().is_some_and(|(n, _)| *n == number) {
            self.renaming = None;
        }
        if self.open != Some(number) {
            return Vec::new();
        }
        let commands = self.new_chat();
        self.notice = Some(GONE.into());
        commands
    }

    /// A change to the list, as the server keeps it current.
    fn list_changed(&mut self, live: ListLive) -> Vec<Command> {
        match live {
            ListLive::Watching => {
                self.list_live = true;
                // Catch up on what changed before the subscription.
                if self.ready() {
                    return vec![Command::List];
                }
            }
            ListLive::Missed => {
                if self.ready() {
                    return vec![Command::List];
                }
            }
            ListLive::Refused | ListLive::Unsupported | ListLive::Offline => {
                self.list_live = false;
            }
            // Changes only apply to a list that was read.
            _ if !self.ready() => {}
            ListLive::Chat(chat) => {
                let number = chat.number;
                let moved = self.open == Some(number)
                    && self
                        .open_summary()
                        .is_some_and(|open| open.state != chat.state);
                self.upsert_chat(chat);
                // The open chat's follower says what changed, unless it
                // can't follow it.
                if moved && self.following != Following::Live {
                    return self.refresh(number);
                }
            }
            ListLive::Removed(number) => return self.remove_chat(number),
            ListLive::Projects(projects) => {
                self.list.projects = projects;
            }
            ListLive::Account(account) => self.list.set_account(account),
        }
        Vec::new()
    }

    /// A chat action finished.
    fn acted(
        &mut self,
        chat: u64,
        action: ChatAction,
        result: Result<Acted, Failure>,
    ) -> Vec<Command> {
        let acted = match result {
            Ok(acted) => acted,
            Err(failure) => {
                self.notice = Some(failure.message);
                return Vec::new();
            }
        };
        match (action, acted) {
            (ChatAction::Branch(_), Acted::Chat(branch)) => {
                let number = branch.number;
                self.upsert_chat(branch);
                let mut commands = self.open_chat(number);
                commands.extend(self.list_again());
                commands
            }
            (ChatAction::Delete, _) => {
                self.list.chats.retain(|c| c.number != chat);
                if self.dialog == Some(Dialog::Delete { chat }) {
                    self.dialog = None;
                }
                if self.open == Some(chat) {
                    return self.new_chat();
                }
                Vec::new()
            }
            (_, Acted::Shared(shared)) => {
                // The link goes on the clipboard as it's made.
                self.clipboard = Some(shared.url.clone());
                self.update_summary(shared.chat);
                if let Some(Dialog::Share { chat: c, copied }) = &mut self.dialog
                    && *c == chat
                {
                    *copied = true;
                }
                Vec::new()
            }
            (action, Acted::Chat(summary)) => {
                if let ChatAction::Rename(_) = action {
                    self.renaming = None;
                }
                if let ChatAction::Retry(_) = action {
                    self.streamed.clear();
                }
                self.update_summary(summary);
                self.refresh(chat)
            }
            (_, Acted::Deleted) => Vec::new(),
        }
    }

    /// A chat read back: it replaces what streamed in, once it has it.
    fn take_transcript(&mut self, transcript: Transcript) {
        for entry in &transcript.entries {
            if let Entry::Assistant { id, content, .. } = entry
                && !content.trim().is_empty()
            {
                self.streamed.remove(id);
            }
        }
        if !transcript.chat.processing() {
            self.streamed.clear();
            self.progress = None;
            self.stopping = false;
        }
        let asked = transcript.entries.iter().rev().find_map(|e| match e {
            Entry::User { content, .. } => Some(content.trim()),
            _ => None,
        });
        if self.pending_question.as_deref().map(str::trim) == asked {
            self.pending_question = None;
        }
        if let Some(listed) = self
            .list
            .chats
            .iter_mut()
            .find(|c| c.number == transcript.chat.number)
        {
            *listed = transcript.chat.clone();
        }
        self.error = None;
        self.transcript = Some(transcript);
    }

    fn failure(&mut self, failure: Failure) {
        self.access = match failure.code.as_str() {
            "chat_access_required" => Access::NeedsApproval {
                requested: failure.requested,
                url: failure.approve_url,
            },
            "daemon_stopped" | "not_paired" => Access::Unknown,
            _ => Access::Unavailable(failure),
        };
    }
}

/// What the page says when the open chat goes from the list.
pub const GONE: &str = "This chat was deleted, or you can't see it anymore.";

/// Whether `a` is later than `b`, both RFC 3339; as text when either
/// doesn't parse.
fn newer(a: &str, b: &str) -> bool {
    match (a.parse::<jiff::Timestamp>(), b.parse::<jiff::Timestamp>()) {
        (Ok(a), Ok(b)) => a > b,
        _ => a > b,
    }
}

/// Failures that are about using chats here at all, rather than one call.
fn is_access_failure(failure: &Failure) -> bool {
    matches!(
        failure.code.as_str(),
        "chat_access_required" | "daemon_stopped" | "not_paired" | "revoked" | "unsupported"
    )
}

/// The values a question's form sends, by field name, typed as the fields
/// ask; empty optional fields are left out.
pub fn form_input(question: &Question, values: Option<&Vec<FieldValue>>) -> Value {
    let values = values.cloned().unwrap_or_else(|| default_form(question));
    let mut input = serde_json::Map::new();
    for (field, value) in question.fields.iter().zip(values) {
        let json = match value {
            FieldValue::Check(on) => Value::Bool(on),
            FieldValue::Picks(picks) => field
                .choices
                .iter()
                .flatten()
                .zip(picks)
                .filter(|(_, on)| *on)
                .map(|(c, _)| Value::String(c.clone()))
                .collect(),
            FieldValue::Text(text) => {
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                // Numbers go as typed: the server reads them as a form does.
                Value::String(text.to_string())
            }
        };
        input.insert(field.name.clone(), json);
    }
    Value::Object(input)
}

/// A form's fields as the web's form starts them: the server's defaults.
pub fn default_form(question: &Question) -> Vec<FieldValue> {
    question.fields.iter().map(default_value).collect()
}

fn default_value(field: &Field) -> FieldValue {
    match field.kind.as_str() {
        "boolean" => FieldValue::Check(field.default == Some(Value::Bool(true))),
        "array" if field.choices.is_some() => {
            let defaults: Vec<String> = match &field.default {
                Some(Value::Array(values)) => values
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                Some(Value::String(one)) => vec![one.clone()],
                _ => Vec::new(),
            };
            FieldValue::Picks(
                field
                    .choices
                    .iter()
                    .flatten()
                    .map(|c| defaults.contains(c))
                    .collect(),
            )
        }
        _ => FieldValue::Text(match &field.default {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }),
    }
}

/// Whether the composer takes a file with this name, as the web's
/// `attachments` controller checks it (`Message::AttachmentPolicy`): never
/// SVG or Flash, otherwise documents, images, audio, video, text and code.
pub fn accepts(name: &str) -> bool {
    const BLOCKED: [&str; 3] = [".svg", ".svgz", ".swf"];
    const SUPPORTED: &[&str] = &[
        ".aac",
        ".aif",
        ".aiff",
        ".avi",
        ".bash",
        ".bmp",
        ".c",
        ".cc",
        ".cjs",
        ".cpp",
        ".css",
        ".csv",
        ".cxx",
        ".doc",
        ".docx",
        ".dot",
        ".flac",
        ".gif",
        ".go",
        ".h",
        ".heic",
        ".heif",
        ".hpp",
        ".htm",
        ".html",
        ".java",
        ".jpeg",
        ".jpg",
        ".js",
        ".json",
        ".jsx",
        ".key",
        ".m4a",
        ".markdown",
        ".md",
        ".mkv",
        ".mov",
        ".mp3",
        ".mp4",
        ".mpeg",
        ".mpg",
        ".numbers",
        ".odp",
        ".ods",
        ".odt",
        ".ogg",
        ".pages",
        ".pdf",
        ".php",
        ".pl",
        ".png",
        ".pot",
        ".pps",
        ".ppt",
        ".pptx",
        ".py",
        ".rb",
        ".rs",
        ".rtf",
        ".sh",
        ".sql",
        ".tcl",
        ".tex",
        ".tif",
        ".tiff",
        ".toml",
        ".ts",
        ".tsx",
        ".txt",
        ".wav",
        ".webm",
        ".webp",
        ".xls",
        ".xlsx",
        ".xml",
        ".yaml",
        ".yml",
        ".zsh",
    ];
    let lower = name.to_lowercase();
    let extension = lower.rfind('.').map_or("", |i| &lower[i..]);
    if extension.is_empty() || BLOCKED.contains(&extension) {
        return false;
    }
    SUPPORTED.contains(&extension)
        || cww::tui::chat::content_type(name) != "application/octet-stream"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(number: u64, state: &str) -> ChatSummary {
        ChatSummary {
            number,
            title: format!("Chat {number}"),
            state: state.into(),
            ..ChatSummary::default()
        }
    }

    fn ready() -> ChatState {
        let mut state = ChatState::default();
        assert_eq!(state.set_paired(true), vec![Command::List]);
        let commands = state.update(Msg::Listed(Ok(ChatList {
            chats: vec![summary(2, "idle"), summary(1, "idle")],
            ..ChatList::default()
        })));
        assert_eq!(
            commands,
            vec![Command::Models, Command::FollowList(true)],
            "the picker's models, and the list kept current, once"
        );
        assert!(state.ready());
        state
    }

    fn at(minutes_ago: i64) -> String {
        (jiff::Timestamp::now() - jiff::SignedDuration::from_mins(minutes_ago)).to_string()
    }

    #[test]
    fn the_list_changes_in_place_once_the_server_keeps_it_current() {
        let mut state = ready();
        state.list.chats = vec![
            ChatSummary {
                updated_at: at(5),
                ..summary(2, "idle")
            },
            ChatSummary {
                updated_at: at(60),
                ..summary(1, "idle")
            },
        ];
        // Watching: catch up once, then no more reading after changes here.
        assert_eq!(
            state.update(Msg::ListLive(ListLive::Watching)),
            vec![Command::List]
        );
        assert!(state.list_live);
        // A chat started elsewhere goes in by time; a changed one moves.
        state.update(Msg::ListLive(ListLive::Chat(ChatSummary {
            updated_at: at(0),
            ..summary(3, "processing")
        })));
        state.update(Msg::ListLive(ListLive::Chat(ChatSummary {
            title: "Renamed".into(),
            updated_at: at(30),
            ..summary(1, "idle")
        })));
        let order: Vec<u64> = state.list.chats.iter().map(|c| c.number).collect();
        assert_eq!(order, [3, 2, 1]);
        assert_eq!(state.list.chats[2].title, "Renamed");
        // Asking needs no read of the list while it's live.
        state.input = "Hi".into();
        state.send();
        let commands = state.update(Msg::Sent {
            chat: None,
            result: Ok(ChatSummary {
                updated_at: at(0),
                ..summary(4, "processing")
            }),
        });
        assert!(!commands.contains(&Command::List), "{commands:?}");
        // The open chat deleted elsewhere closes, saying so.
        assert_eq!(state.open, Some(4));
        assert_eq!(
            state.update(Msg::ListLive(ListLive::Removed(4))),
            vec![Command::Follow(None)]
        );
        assert_eq!(state.open, None);
        assert_eq!(state.notice.as_deref(), Some(GONE));
        // Missed changes: read it all again.
        assert_eq!(
            state.update(Msg::ListLive(ListLive::Missed)),
            vec![Command::List]
        );
        // Offline, or a daemon that can't: back to reading it after changes.
        state.update(Msg::ListLive(ListLive::Offline));
        assert!(!state.list_live);
        state.update(Msg::ListFollowEnded(Some(Failure::new(
            "daemon_outdated",
            "old",
        ))));
        assert_eq!(state.set_paired(false), vec![]);
    }

    #[test]
    fn the_list_follower_stops_with_the_daemon_and_starts_again() {
        let mut state = ready();
        assert_eq!(state.set_paired(false), vec![Command::FollowList(false)]);
        assert_eq!(state.set_paired(true), vec![Command::List]);
        let commands = state.update(Msg::Listed(Ok(ChatList::default())));
        assert_eq!(commands, vec![Command::FollowList(true)]);
        // Read again while following: not twice.
        state.update(Msg::ListLive(ListLive::Refused));
        assert!(
            state
                .update(Msg::Listed(Ok(ChatList::default())))
                .is_empty()
        );
    }

    #[test]
    fn a_first_question_starts_and_follows_a_chat() {
        let mut state = ready();
        state.input = "  Q3?  ".into();
        assert_eq!(
            state.send(),
            vec![Command::Send(Ask {
                text: "Q3?".into(),
                ..Ask::default()
            })]
        );
        assert!(state.working() && state.input.is_empty());
        // Nothing more goes while it's on its way.
        state.input = "again".into();
        assert!(state.send().is_empty());
        let commands = state.update(Msg::Sent {
            chat: None,
            result: Ok(ChatSummary {
                updated_at: at(0),
                ..summary(3, "processing")
            }),
        });
        assert_eq!(
            commands,
            vec![Command::List, Command::Follow(Some(3)), Command::Open(3)],
            "the list isn't live yet: read it again"
        );
        assert_eq!(state.open, Some(3));
        assert_eq!(state.list.chats[0].number, 3);
    }

    #[test]
    fn streamed_text_stays_until_the_chat_has_it() {
        let mut state = ready();
        state.open_chat(2);
        let chunk = |text: &str| Msg::Live {
            chat: 2,
            live: Live::Chunk {
                message_id: 9,
                text: text.into(),
            },
        };
        state.update(chunk("The budget "));
        state.update(chunk("is €40k."));
        assert_eq!(state.streamed[&9], "The budget is €40k.");
        // A read without the message's text yet leaves the stream.
        state.update(Msg::Shown {
            chat: 2,
            result: Ok(Transcript {
                chat: summary(2, "processing"),
                entries: vec![Entry::Assistant {
                    id: 9,
                    content: String::new(),
                    sources: vec![],
                }],
                ..Transcript::default()
            }),
        });
        assert_eq!(state.streamed[&9], "The budget is €40k.");
        // A changed chat is read again, once the read under way is back.
        assert_eq!(
            state.update(Msg::Live {
                chat: 2,
                live: Live::Changed
            }),
            vec![Command::Open(2)]
        );
        state.update(Msg::Shown {
            chat: 2,
            result: Ok(Transcript {
                chat: summary(2, "idle"),
                ..Transcript::default()
            }),
        });
        assert!(state.streamed.is_empty() && !state.working());
    }

    #[test]
    fn a_failed_question_comes_back_to_the_composer() {
        let mut state = ready();
        state.open_chat(1);
        state.update(Msg::Shown {
            chat: 1,
            result: Ok(Transcript {
                chat: summary(1, "idle"),
                ..Transcript::default()
            }),
        });
        state.input = "And Q4?".into();
        state.send();
        state.update(Msg::Sent {
            chat: Some(1),
            result: Err(Failure::new("chat_busy", "An answer is being written.")),
        });
        assert_eq!(state.input, "And Q4?");
        assert_eq!(state.notice.as_deref(), Some("An answer is being written."));
    }

    #[test]
    fn chats_need_the_owners_approval() {
        let mut state = ChatState::default();
        state.set_paired(true);
        let mut failure = Failure::new("chat_access_required", "Allow it in Settings");
        failure.approve_url = Some("https://chatwithwork.com/settings".into());
        assert!(state.update(Msg::Listed(Err(failure))).is_empty());
        assert!(matches!(
            state.access,
            Access::NeedsApproval {
                requested: false,
                ..
            }
        ));
        assert_eq!(state.request_access(), vec![Command::RequestAccess]);
        assert!(state.request_access().is_empty(), "asked once");
        state.update(Msg::Access(Ok(AccessRequest {
            granted: true,
            ..AccessRequest::default()
        })));
        assert_eq!(state.access, Access::Loading);
    }

    #[test]
    fn stopping_asks_once() {
        let mut state = ready();
        state.open_chat(2);
        state.update(Msg::Shown {
            chat: 2,
            result: Ok(Transcript {
                chat: summary(2, "processing"),
                ..Transcript::default()
            }),
        });
        assert_eq!(state.stop(), vec![Command::Cancel(2)]);
        assert!(state.stop().is_empty());
    }

    #[test]
    fn the_daemon_going_away_stops_following() {
        let mut state = ready();
        state.open_chat(2);
        assert_eq!(
            state.set_paired(false),
            vec![Command::Follow(None), Command::FollowList(false)]
        );
        assert_eq!(state.access, Access::Unknown);
        // Back again: list, follow and read the open chat.
        let commands = state.set_paired(true);
        assert_eq!(
            commands,
            vec![Command::List, Command::Follow(Some(2)), Command::Open(2)]
        );
    }

    #[test]
    fn search_filters_by_title_number_and_project() {
        let mut state = ready();
        state.search = "#1".into();
        assert_eq!(state.visible().len(), 1);
        state.search = "chat".into();
        assert_eq!(state.visible().len(), 2);
    }

    fn models() -> Models {
        serde_json::from_value(serde_json::json!({
            "default_model_id": 1,
            "models": [
                { "id": 1, "name": "Flash", "provider": "vertexai" },
                { "id": 2, "name": "Sol", "provider": "azure", "selectable": false,
                  "reason": "You're out of credits." },
                { "id": 3, "name": "Large", "provider": "hetzner" },
            ]
        }))
        .unwrap()
    }

    #[test]
    fn a_model_picked_goes_with_the_question_and_stays_with_the_chat() {
        let mut state = ready();
        state.update(Msg::Models(Ok(models())));
        assert_eq!(state.current_model(), Some(("1".into(), "Flash".into())));
        // One that can't be used says why, and isn't picked.
        state.pick_model("2");
        assert_eq!(state.notice.as_deref(), Some("You're out of credits."));
        assert_eq!(state.current_model().unwrap().0, "1");
        state.pick_model("3");
        state.input = "Hi".into();
        let [Command::Send(ask)] = &state.send()[..] else {
            panic!("one send")
        };
        assert_eq!(ask.model.as_deref(), Some("3"));
        state.update(Msg::Sent {
            chat: None,
            result: Ok(summary(3, "processing")),
        });
        assert_eq!(state.current_model().unwrap().1, "Large");
        // Another chat shows its own.
        state.open_chat(2);
        assert_eq!(state.current_model(), None);
    }

    #[test]
    fn files_upload_before_the_question_goes_with_them() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("Q3 plan.pdf");
        std::fs::write(&pdf, b"%PDF").unwrap();
        let svg = dir.path().join("logo.svg");
        std::fs::write(&svg, b"<svg/>").unwrap();
        let mut state = ready();
        let commands = state.attach(vec![pdf.clone(), svg]);
        assert_eq!(commands, vec![Command::Upload { key: 1, path: pdf }]);
        assert_eq!(
            state.dialog,
            Some(Dialog::Unsupported {
                name: "logo.svg".into()
            })
        );
        assert_eq!(state.can_send(), Err("Wait for the files to upload"));
        state.update(Msg::Uploaded {
            key: 1,
            result: Ok(Uploaded {
                signed_id: "sid".into(),
                filename: "Q3 plan.pdf".into(),
                byte_size: 4,
                content_type: "application/pdf".into(),
                icon: None,
            }),
        });
        // Files alone can be sent.
        assert_eq!(state.can_send(), Ok(()));
        let [Command::Send(ask)] = &state.send()[..] else {
            panic!("one send")
        };
        assert_eq!(ask.attachments, ["sid"]);
        assert!(state.attachments.is_empty());
    }

    #[test]
    fn a_refused_upload_comes_off_with_the_servers_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.txt");
        std::fs::write(&path, b"x").unwrap();
        let mut state = ready();
        state.attach(vec![path]);
        state.update(Msg::Uploaded {
            key: 1,
            result: Err(Failure::new(
                "attachment_refused",
                "setup.txt is not a supported file type",
            )),
        });
        assert!(state.attachments.is_empty());
        assert_eq!(
            state.notice.as_deref(),
            Some("setup.txt is not a supported file type")
        );
    }

    #[test]
    fn the_composer_takes_what_the_web_takes() {
        assert!(accepts("Q3.PDF"));
        assert!(accepts("main.rs"));
        assert!(accepts("notes.log"));
        assert!(!accepts("logo.svg"));
        assert!(!accepts("setup.exe"));
        assert!(!accepts("README"));
    }

    #[test]
    fn a_decision_goes_once_and_reads_the_chat_again() {
        let mut state = ready();
        state.open_chat(2);
        state.update(Msg::Shown {
            chat: 2,
            result: Ok(Transcript {
                chat: summary(2, "idle"),
                ..Transcript::default()
            }),
        });
        let approve = Decision::Approve {
            tool_call: 7,
            for_rest_of_chat: true,
        };
        assert_eq!(
            state.decide(approve.clone()),
            vec![Command::Decide {
                chat: 2,
                decision: approve.clone()
            }]
        );
        assert!(state.decide(approve).is_empty(), "one at a time");
        let commands = state.update(Msg::Decided {
            chat: 2,
            result: Ok(summary(2, "processing")),
        });
        assert_eq!(commands, vec![Command::Open(2)]);
        assert!(state.working() && state.deciding.is_none());
    }

    #[test]
    fn a_decided_change_leaves_in_place_and_a_race_is_no_error() {
        let mut state = ready();
        state.open_chat(2);
        let card = |id| cww::tui::chat::Approval {
            id,
            decidable: true,
            ..Default::default()
        };
        state.update(Msg::Shown {
            chat: 2,
            result: Ok(Transcript {
                chat: summary(2, "idle"),
                approvals: vec![card(7), card(8)],
                ..Transcript::default()
            }),
        });
        let approve = Decision::Approve {
            tool_call: 7,
            for_rest_of_chat: false,
        };
        state.decide(approve);
        // Another change still waits: only the decided card goes, at once.
        state.update(Msg::Decided {
            chat: 2,
            result: Ok(summary(2, "idle")),
        });
        let left: Vec<u64> = state
            .transcript
            .as_ref()
            .unwrap()
            .approvals
            .iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(left, [8]);

        // A second press that lost the race: settled, not an error.
        state.loading = false;
        state.decide(Decision::Deny {
            tool_call: 8,
            reason: None,
        });
        let commands = state.update(Msg::Decided {
            chat: 2,
            result: Err(Failure::new(
                "already_decided",
                "Someone already decided this.",
            )),
        });
        assert_eq!(commands, vec![Command::Open(2)]);
        assert!(state.notice.is_none());
        assert!(state.transcript.as_ref().unwrap().approvals.is_empty());
    }

    #[test]
    fn forms_send_the_fields_as_typed() {
        let question: Question = serde_json::from_value(serde_json::json!({
            "id": 5, "service": "Notion", "decidable": true, "kind": "form",
            "fields": [
                { "name": "env", "type": "string", "required": true, "choices": ["staging", "production"] },
                { "name": "days", "type": "integer", "default": 7 },
                { "name": "drafts", "type": "boolean" },
                { "name": "teams", "type": "array", "choices": ["Web", "Ops"], "default": ["Ops"] },
                { "name": "note", "type": "string" },
            ]
        }))
        .unwrap();
        let mut form = default_form(&question);
        form[0] = FieldValue::Text("production".into());
        assert_eq!(
            form_input(&question, Some(&form)),
            serde_json::json!({ "env": "production", "days": "7", "drafts": false, "teams": ["Ops"] })
        );
    }

    #[test]
    fn branching_opens_the_new_chat_and_deleting_the_open_one_starts_afresh() {
        let mut state = ready();
        state.open_chat(2);
        let commands = state.act(2, ChatAction::Branch(5));
        assert_eq!(
            commands,
            vec![Command::Act {
                chat: 2,
                action: ChatAction::Branch(5)
            }]
        );
        let commands = state.update(Msg::Acted {
            chat: 2,
            action: ChatAction::Branch(5),
            result: Ok(Acted::Chat(summary(9, "idle"))),
        });
        assert_eq!(state.open, Some(9));
        assert!(commands.contains(&Command::Open(9)));
        state.act(9, ChatAction::Delete);
        state.update(Msg::Acted {
            chat: 9,
            action: ChatAction::Delete,
            result: Ok(Acted::Deleted),
        });
        assert_eq!(state.open, None);
        assert!(state.list.chats.iter().all(|c| c.number != 9));
    }
}
