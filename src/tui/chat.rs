//! Where the chat pane gets its chats from.
//!
//! Chat with Work has no chat API for the terminal yet (CHAT_API.md proposes
//! one), so the only backend is [`NotConnected`], which says so honestly.
//! The trait is what an HTTP backend will implement. Every method blocks;
//! the app calls them from worker threads.

use anyhow::{Result, bail};

pub type ChatId = String;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// Why chat can't be used here, as a sentence.
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSummary {
    pub id: ChatId,
    pub title: String,
    /// RFC 3339.
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: String,
    pub role: Role,
    pub text: String,
    /// Tool work done while writing this answer.
    pub tools: Vec<ToolStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStep {
    /// The tool call's ID, to update a step as it finishes.
    pub id: String,
    /// The service that answered, e.g. `Drive`, `Slack`, or this computer.
    pub service: String,
    /// What it did, e.g. `search "budget"`.
    pub summary: String,
    pub state: ToolState,
}

/// One step of a streamed reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    /// The server accepted the message. For a new chat, this is its ID.
    Started {
        chat_id: ChatId,
        message_id: String,
    },
    TextDelta(String),
    Tool(ToolStep),
    Done,
    Error(String),
}

pub type ChatStream = Box<dyn Iterator<Item = Result<ChatEvent>> + Send>;

pub trait ChatBackend: Send + Sync {
    fn availability(&self) -> Availability;

    /// Most recent first.
    fn list_chats(&self) -> Result<Vec<ChatSummary>>;

    /// Oldest first.
    fn messages(&self, chat: &str) -> Result<Vec<ChatMessage>>;

    /// Post `text` to `chat` (a new chat when `None`) and stream the reply.
    fn send(&self, chat: Option<&str>, text: &str) -> Result<ChatStream>;

    /// Stop the reply being written in `chat`.
    fn cancel(&self, chat: &str) -> Result<()>;
}

/// No chat API to talk to. The pane says so and points at the browser.
pub struct NotConnected;

pub const NO_CHAT_API: &str = "Chat with Work has no chat API for it.";

impl ChatBackend for NotConnected {
    fn availability(&self) -> Availability {
        Availability::Unavailable {
            reason: NO_CHAT_API.into(),
        }
    }

    fn list_chats(&self) -> Result<Vec<ChatSummary>> {
        bail!("{NO_CHAT_API}")
    }

    fn messages(&self, _chat: &str) -> Result<Vec<ChatMessage>> {
        bail!("{NO_CHAT_API}")
    }

    fn send(&self, _chat: Option<&str>, _text: &str) -> Result<ChatStream> {
        bail!("{NO_CHAT_API}")
    }

    fn cancel(&self, _chat: &str) -> Result<()> {
        bail!("{NO_CHAT_API}")
    }
}
