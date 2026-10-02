//! What the chat page knows, and how each answer from the daemon changes
//! it: the same rules as the terminal UI's chat pane (`cww::tui::app`), for
//! the same control API. It does no I/O; the commands it returns are run
//! by `runner.rs`.

use std::collections::BTreeMap;

use cww::tui::chat::{AccessRequest, ChatList, ChatSummary, Entry, Failure, Live, Transcript};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Read the list of chats.
    List,
    /// Read one chat.
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
pub struct ChatState {
    pub access: Access,
    pub list: ChatList,
    /// What's typed in the sidebar's search.
    pub search: String,
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
    /// The daemon is running and paired, so chats can be asked for.
    paired: bool,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            access: Access::Unknown,
            list: ChatList::default(),
            search: String::new(),
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
            paired: false,
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
        if query.is_empty() {
            return self.list.chats.iter().collect();
        }
        self.list
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
            .collect()
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
            if followed {
                vec![Command::Follow(None)]
            } else {
                Vec::new()
            }
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
        if followed {
            vec![Command::Follow(None)]
        } else {
            Vec::new()
        }
    }

    /// Whether what's typed can be sent now, or why not.
    pub fn can_send(&self) -> Result<(), &'static str> {
        if !self.ready() {
            Err("Chats aren't available")
        } else if self.working() {
            Err("Wait for the answer, or stop it")
        } else if self.locked_reason().is_some() {
            Err("Locked")
        } else if self.input.trim().is_empty() {
            Err("Type a question")
        } else {
            Ok(())
        }
    }

    /// Send what's typed.
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
        vec![Command::Send {
            chat: self.open,
            text,
        }]
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
                // Chats are back (allowed again, say): follow the open one
                // again, unless that's already under way.
                if let Some(open) = self.open
                    && !was_ready
                    && self.following != Following::Starting
                {
                    self.following = Following::Starting;
                    let mut commands = vec![Command::Follow(Some(open))];
                    commands.extend(self.refresh(open));
                    return commands;
                }
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
                        self.list.chats.retain(|c| c.number != number);
                        self.list.chats.insert(0, summary);
                        let mut commands = vec![Command::List];
                        if chat.is_none() && self.open.is_none() {
                            // The first question made the chat: follow it.
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
                        if is_access_failure(&failure) {
                            self.failure(failure);
                        } else {
                            self.notice = Some(failure.message);
                        }
                    }
                }
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
        }
        Vec::new()
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

/// Failures that are about using chats here at all, rather than one call.
fn is_access_failure(failure: &Failure) -> bool {
    matches!(
        failure.code.as_str(),
        "chat_access_required" | "daemon_stopped" | "not_paired" | "revoked" | "unsupported"
    )
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
        state.update(Msg::Listed(Ok(ChatList {
            chats: vec![summary(2, "idle"), summary(1, "idle")],
            ..ChatList::default()
        })));
        assert!(state.ready());
        state
    }

    #[test]
    fn a_first_question_starts_and_follows_a_chat() {
        let mut state = ready();
        state.input = "  Q3?  ".into();
        assert_eq!(
            state.send(),
            vec![Command::Send {
                chat: None,
                text: "Q3?".into()
            }]
        );
        assert!(state.working() && state.input.is_empty());
        // Nothing more goes while it's on its way.
        state.input = "again".into();
        assert!(state.send().is_empty());
        let commands = state.update(Msg::Sent {
            chat: None,
            result: Ok(summary(3, "processing")),
        });
        assert_eq!(
            commands,
            vec![Command::List, Command::Follow(Some(3)), Command::Open(3)]
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
        assert_eq!(state.set_paired(false), vec![Command::Follow(None)]);
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
}
