//! Chats in the terminal, through the daemon.
//!
//! The TUI never holds a token: every call goes to the daemon over the
//! control socket (CONTROL.md, "Chats"), and the daemon asks Chat with Work
//! as this computer. The shapes here are the server's, as the daemon
//! relays them; unknown fields are ignored so a newer server still parses.
//! Every function blocks; the TUI calls them from worker threads.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::control::{Client, Closer, ControlRequest};

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Project {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ChatSummary {
    pub number: u64,
    pub title: String,
    /// `idle`, `processing` or `error`.
    pub state: String,
    pub project: Option<Project>,
    /// Started by this person, rather than someone in a shared project.
    pub mine: bool,
    /// RFC 3339.
    pub updated_at: String,
    pub url: String,
}

impl ChatSummary {
    pub fn processing(&self) -> bool {
        self.state == "processing"
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Named {
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ChatList {
    /// Newest first.
    pub chats: Vec<ChatSummary>,
    pub projects: Vec<Project>,
    pub account: Named,
    pub user: Named,
    /// Why a new question can't be asked right now, as the composer says it.
    pub locked_reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Source {
    pub title: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Step {
    /// "Searched Drive for “budget”".
    pub summary: String,
    pub pending: bool,
    /// Names of the files it found, for the owner's own steps.
    pub files: Vec<String>,
}

/// One thing in a conversation, as the web shows it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    User {
        id: u64,
        content: String,
        /// Who asked, when it wasn't this person (a project chat).
        #[serde(default)]
        author: Option<String>,
    },
    Assistant {
        id: u64,
        /// Markdown.
        content: String,
        #[serde(default)]
        sources: Vec<Source>,
    },
    /// The tool work between two things said, folded into one line.
    Activity {
        id: u64,
        /// "Searched Drive and Slack".
        title: String,
        /// "3 searches, read 2 files".
        #[serde(default)]
        details: Option<String>,
        /// What the running step reports doing.
        #[serde(default)]
        progress: Option<String>,
        #[serde(default)]
        services: Vec<String>,
        #[serde(default)]
        pending: bool,
        #[serde(default)]
        steps: Vec<Step>,
    },
    /// A failure or running out of credits. `tone` is `negative` or `attention`.
    Notice { tone: String, text: String },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Transcript {
    pub chat: ChatSummary,
    pub locked_reason: Option<String>,
    pub entries: Vec<Entry>,
}

/// Where asking for chat access leaves things.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AccessRequest {
    pub granted: bool,
    pub requested: bool,
    pub approve_url: Option<String>,
}

/// Why a chat call didn't work, with the code to act on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Failure {
    /// `daemon_stopped`, `not_paired`, `revoked`, `unreachable`,
    /// `unsupported`, `chat_access_required`, `locked`, `chat_busy`,
    /// `not_found`, `rate_limited`, `invalid`, `forbidden`, ...
    pub code: String,
    pub message: String,
    pub approve_url: Option<String>,
    /// The owner was already asked and hasn't answered.
    pub requested: bool,
}

impl Failure {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            ..Self::default()
        }
    }

    fn from_response(response: &Value) -> Self {
        Self {
            code: response["code"].as_str().unwrap_or("error").into(),
            message: response["error"]
                .as_str()
                .unwrap_or("The daemon returned an error")
                .into(),
            approve_url: response["approve_url"].as_str().map(str::to_string),
            requested: response["requested"].as_bool().unwrap_or(false),
        }
    }
}

/// Something that happened in a followed chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Live {
    /// Answer text as it's written.
    Chunk { message_id: u64, text: String },
    /// What the running tool step is doing.
    Progress(String),
    /// Anything else: read the chat again.
    Changed,
    /// The server follows the chat for us (again): catch up.
    Watching,
    /// The server won't let this computer follow it.
    Refused,
    /// The daemon's connection can't follow chats at all (it speaks bare
    /// MCP): nothing to retry until it reconnects.
    Unsupported,
    /// No connection to the server right now; the daemon keeps trying.
    Offline,
}

impl Live {
    /// `None` only for a chunk it can't read; anything unknown means "read
    /// the chat again".
    fn from_update(update: &Value) -> Option<Self> {
        Some(match update["type"].as_str().unwrap_or_default() {
            "chunk" => Self::Chunk {
                message_id: update["message_id"].as_u64()?,
                text: update["text"].as_str()?.to_string(),
            },
            "progress" => Self::Progress(update["text"].as_str()?.to_string()),
            "watching" => Self::Watching,
            "refused" => Self::Refused,
            "unsupported" => Self::Unsupported,
            "offline" => Self::Offline,
            _ => Self::Changed,
        })
    }
}

/// The daemon, as the chat pane talks to it.
#[derive(Debug, Clone)]
pub struct Chats {
    socket: PathBuf,
}

impl Chats {
    pub fn new(socket: &Path) -> Self {
        Self {
            socket: socket.to_path_buf(),
        }
    }

    pub fn list(&self) -> Result<ChatList, Failure> {
        parse(self.call(&ControlRequest::Chats)?)
    }

    pub fn show(&self, chat: u64) -> Result<Transcript, Failure> {
        parse(self.call(&ControlRequest::Chat {
            chat: chat.to_string(),
        })?)
    }

    /// Ask in `chat`, or start a new chat. Answers with the chat.
    pub fn send(&self, chat: Option<u64>, text: &str) -> Result<ChatSummary, Failure> {
        let response = self.call(&ControlRequest::ChatSend {
            chat: chat.map(|c| c.to_string()),
            text: text.to_string(),
            project: None,
        })?;
        parse(response["chat"].clone())
    }

    pub fn cancel(&self, chat: u64) -> Result<(), Failure> {
        self.call(&ControlRequest::ChatCancel {
            chat: chat.to_string(),
        })
        .map(|_| ())
    }

    pub fn request_access(&self) -> Result<AccessRequest, Failure> {
        parse(self.call(&ControlRequest::ChatAccess)?)
    }

    /// Follow `chat` until `on_live` answers false or the daemon goes
    /// away. `on_live` also hears `None` now and then, when nothing
    /// happened, so it can stop; `on_start` gets a way to stop it sooner
    /// from another thread.
    pub fn follow(
        &self,
        chat: u64,
        on_start: impl FnOnce(Option<Closer>),
        mut on_live: impl FnMut(Option<Live>) -> bool,
    ) -> Result<(), Failure> {
        let client = self.connect()?;
        let events = client
            .follow_chat(&chat.to_string())
            .map_err(|e| Failure::new("daemon_stopped", format!("{e:#}")))?
            .map_err(|response| Failure::from_response(&response))?;
        on_start(events.closer());
        for event in events {
            let Ok(event) = event else { break };
            // Anything else, like a heartbeat, is nothing new but a chance
            // to stop.
            let live = match event["event"].as_str() {
                Some("chat") => Live::from_update(&event["update"]),
                _ => None,
            };
            if !on_live(live) {
                break;
            }
        }
        Ok(())
    }

    fn connect(&self) -> Result<Client, Failure> {
        match Client::connect(&self.socket) {
            Ok(Some(client)) => Ok(client),
            Ok(None) => Err(Failure::new("daemon_stopped", "The daemon isn't running.")),
            Err(e) => Err(Failure::new("daemon_stopped", format!("{e:#}"))),
        }
    }

    fn call(&self, request: &ControlRequest) -> Result<Value, Failure> {
        let response = self
            .connect()?
            .call_raw(request)
            .map_err(|e| Failure::new("daemon_stopped", format!("{e:#}")))?;
        if response["ok"] == Value::Bool(true) {
            Ok(response)
        } else {
            Err(Failure::from_response(&response))
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Failure> {
    serde_json::from_value(value).map_err(|e| {
        Failure::new(
            "unsupported",
            format!("Chat with Work sent something this cww doesn't understand ({e}). Update cww."),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_the_servers_transcript() {
        let transcript: Transcript = serde_json::from_value(json!({
            "chat": { "number": 7, "title": "Budget", "state": "processing", "project": null,
                      "mine": true, "updated_at": "2026-09-25T08:14:03Z", "url": "https://x/1/chats/7" },
            "locked_reason": null,
            "entries": [
                { "kind": "user", "id": 1, "content": "Q3?" },
                { "kind": "activity", "id": 2, "title": "Searching Drive", "services": ["Drive"], "pending": true,
                  "steps": [{ "summary": "Searching Drive for “q3”", "pending": true }] },
                { "kind": "assistant", "id": 3, "content": "**€40k**", "sources": [{ "title": "Q3.pdf", "url": null }] },
                { "kind": "notice", "tone": "negative", "text": "Model is rate limited." },
                { "kind": "hologram", "id": 9 }
            ]
        }))
        .unwrap();
        assert!(transcript.chat.processing());
        assert_eq!(transcript.entries.len(), 5);
        assert!(
            matches!(&transcript.entries[1], Entry::Activity { pending: true, steps, .. } if steps.len() == 1)
        );
        assert_eq!(transcript.entries[4], Entry::Unknown);
    }

    #[test]
    fn reads_failures_and_live_updates() {
        let failure = Failure::from_response(&json!({
            "ok": false, "error": "Allow it in Settings", "code": "chat_access_required",
            "requested": true, "approve_url": "https://x/settings"
        }));
        assert_eq!(failure.code, "chat_access_required");
        assert!(failure.requested);
        assert_eq!(
            Live::from_update(&json!({ "type": "chunk", "message_id": 5, "text": "Hi" })),
            Some(Live::Chunk {
                message_id: 5,
                text: "Hi".into()
            })
        );
        assert_eq!(
            Live::from_update(&json!({ "type": "anything" })),
            Some(Live::Changed)
        );
        assert_eq!(Live::from_update(&json!({})), Some(Live::Changed));
        assert_eq!(Live::from_update(&json!({ "type": "chunk" })), None);
        assert_eq!(
            Live::from_update(&json!({ "type": "unsupported" })),
            Some(Live::Unsupported)
        );
    }
}
