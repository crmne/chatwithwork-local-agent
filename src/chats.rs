//! Chats for the terminal UI, relayed by the daemon.
//!
//! The TUI never holds a token. It asks the daemon over the control socket,
//! and the daemon calls Chat with Work's chat API as its device, with the
//! same DPoP-bound access token its tunnel uses ([`ChatClient`]). The server
//! only answers while the owner lets this computer use their chats; that
//! permission is asked for when pairing, or on first use, and the owner can
//! take it back at any time without touching the shared folders.
//!
//! Answers stream over the tunnel's WebSocket rather than a new connection:
//! the daemon subscribes to `LocalAgent::ChatChannel` for each chat a TUI
//! follows ([`ChatHub`]) and passes its updates to the TUI's subscription.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};

use crate::auth::client::RefreshError;
use crate::auth::tokens::Tokens;
use crate::control::{ControlEvent, Refusal};

/// A chat number, as the server numbers chats in an organization. Checked
/// before it goes into a path or a channel identifier.
pub fn chat_number(chat: &str) -> Result<&str, Refusal> {
    if !chat.is_empty() && chat.len() <= 18 && chat.bytes().all(|b| b.is_ascii_digit()) {
        Ok(chat)
    } else {
        Err(Refusal::new(
            "bad_request",
            format!("bad request: {chat:?} is not a chat number"),
        ))
    }
}

/// What a tunnel session is asked to do with the server's chat channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Follow {
    Watch(String),
    Unwatch(String),
}

/// The chats TUIs follow, counted, and the tunnel session that follows
/// them on the server. A new session is told to follow every chat again,
/// so a reconnect loses nothing but what happened while it was down.
pub struct ChatHub {
    state: Mutex<HubState>,
    events: broadcast::Sender<ControlEvent>,
}

#[derive(Default)]
struct HubState {
    followers: HashMap<String, usize>,
    session: Option<mpsc::UnboundedSender<Follow>>,
}

impl ChatHub {
    pub fn new(events: broadcast::Sender<ControlEvent>) -> Self {
        Self {
            state: Mutex::new(HubState::default()),
            events,
        }
    }

    /// One more follower for `chat`. The first one subscribes on the server;
    /// without a session, the TUI hears `offline` and waits.
    pub fn watch(&self, chat: &str) {
        let mut state = self.state.lock().expect("chat hub");
        let count = state.followers.entry(chat.to_string()).or_default();
        *count += 1;
        if *count == 1 {
            match &state.session {
                Some(session) => {
                    let _ = session.send(Follow::Watch(chat.to_string()));
                }
                None => self.note(chat, "offline"),
            }
        } else {
            // Already followed: the new subscriber needs to know it's live.
            self.note(
                chat,
                if state.session.is_some() {
                    "watching"
                } else {
                    "offline"
                },
            );
        }
    }

    pub fn unwatch(&self, chat: &str) {
        let mut state = self.state.lock().expect("chat hub");
        if let Some(count) = state.followers.get_mut(chat) {
            *count -= 1;
            if *count == 0 {
                state.followers.remove(chat);
                if let Some(session) = &state.session {
                    let _ = session.send(Follow::Unwatch(chat.to_string()));
                }
            }
        }
    }

    /// A tunnel session starts: it receives what to follow, starting with
    /// every chat followed now.
    pub fn attach(&self) -> mpsc::UnboundedReceiver<Follow> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut state = self.state.lock().expect("chat hub");
        for chat in state.followers.keys() {
            let _ = tx.send(Follow::Watch(chat.clone()));
        }
        state.session = Some(tx);
        rx
    }

    /// The session ended: followers wait for the next one.
    pub fn detach(&self) {
        let mut state = self.state.lock().expect("chat hub");
        state.session = None;
        for chat in state.followers.keys() {
            self.note(chat, "offline");
        }
    }

    /// An update the server sent for `chat`.
    pub fn publish(&self, chat: &str, update: Value) {
        let _ = self.events.send(ControlEvent::Chat {
            chat: chat.to_string(),
            update,
        });
    }

    /// A note about the subscription itself: `watching` once the server
    /// confirms it, `refused` when it won't, `offline` without a tunnel.
    pub fn note(&self, chat: &str, what: &str) {
        self.publish(chat, json!({ "type": what }));
    }

    pub fn followed(&self) -> Vec<String> {
        let mut chats: Vec<String> = self
            .state
            .lock()
            .expect("chat hub")
            .followers
            .keys()
            .cloned()
            .collect();
        chats.sort();
        chats
    }
}

/// Chat with Work's chat API, called as this device. Every call blocks.
pub struct ChatClient {
    tokens: Arc<Tokens>,
}

impl ChatClient {
    pub fn new(tokens: Arc<Tokens>) -> Self {
        Self { tokens }
    }

    pub fn list(&self) -> Result<Value> {
        self.call("GET", "local_agent/chats", None)
    }

    pub fn show(&self, chat: &str) -> Result<Value> {
        let chat = chat_number(chat)?;
        self.call("GET", &format!("local_agent/chats/{chat}"), None)
    }

    pub fn send(&self, chat: Option<&str>, text: &str, project: Option<u64>) -> Result<Value> {
        match chat {
            Some(chat) => {
                let chat = chat_number(chat)?;
                self.call(
                    "POST",
                    &format!("local_agent/chats/{chat}/messages"),
                    Some(&json!({ "content": text })),
                )
            }
            None => {
                let mut body = json!({ "content": text });
                if let Some(project) = project {
                    body["project_id"] = json!(project);
                }
                self.call("POST", "local_agent/chats", Some(&body))
            }
        }
    }

    pub fn cancel(&self, chat: &str) -> Result<Value> {
        let chat = chat_number(chat)?;
        self.call(
            "POST",
            &format!("local_agent/chats/{chat}/cancellation"),
            None,
        )
    }

    pub fn request_access(&self) -> Result<Value> {
        self.call("POST", "local_agent/chat_access_request", None)
    }

    /// One call, refreshing the token and trying again once when the server
    /// says it's expired, or lacks chats that were allowed since it was
    /// issued. Failures come back as [`Refusal`]s with the server's code.
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut retried = false;
        loop {
            let token = self.tokens.access_token().map_err(refresh_refusal)?;
            let (status, response) = self
                .tokens
                .client()
                .send_json(self.tokens.key(), method, path, &token, body)
                .map_err(|e| {
                    Refusal::new("unreachable", format!("Can't reach Chat with Work: {e:#}"))
                })?;
            let code = response["error"].as_str().unwrap_or_default();
            match status {
                200..=299 if response.is_object() || status == 202 => {
                    return Ok(if response.is_object() {
                        response
                    } else {
                        json!({})
                    });
                }
                401 if !retried => self.tokens.invalidate(),
                403 if code == "insufficient_scope" && !retried => {
                    self.tokens.refresh().map_err(refresh_refusal)?;
                }
                _ => return Err(server_refusal(status, &response).into()),
            }
            retried = true;
        }
    }
}

fn refresh_refusal(e: RefreshError) -> Refusal {
    match e {
        RefreshError::Revoked(reason) => Refusal::new(
            "revoked",
            format!(
                "Chat with Work revoked this computer ({reason}). Pair it again with cww login."
            ),
        ),
        RefreshError::Other(e) => {
            Refusal::new("unreachable", format!("Can't reach Chat with Work: {e:#}"))
        }
    }
}

/// The server's error as a refusal. An answer that isn't the chat API's
/// JSON means the server doesn't offer chats to the terminal (yet).
fn server_refusal(status: u16, response: &Value) -> Refusal {
    let Some(code) = response["error"].as_str() else {
        return Refusal::new(
            "unsupported",
            format!(
                "This Chat with Work server doesn't offer chats to the terminal (it answered {status})."
            ),
        );
    };
    let message = response["error_description"]
        .as_str()
        .unwrap_or(code)
        .to_string();
    let mut details = response.clone();
    if let Some(object) = details.as_object_mut() {
        object.remove("error");
        object.remove("error_description");
    }
    details["status"] = json!(status);
    Refusal::new(code, message).with_details(details)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_numbers_are_digits_only() {
        assert_eq!(chat_number("42").unwrap(), "42");
        for bad in ["", "4 2", "../1", "42?x=1", "1234567890123456789"] {
            assert!(chat_number(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn server_errors_keep_their_code_and_details() {
        let refusal = server_refusal(
            403,
            &json!({ "error": "chat_access_required", "error_description": "Allow it", "requested": true, "approve_url": "https://x/settings" }),
        );
        assert_eq!(refusal.code, "chat_access_required");
        assert_eq!(refusal.message, "Allow it");
        assert_eq!(refusal.details["approve_url"], "https://x/settings");
        assert_eq!(refusal.details["requested"], true);
        assert!(refusal.details.get("error").is_none());

        assert_eq!(server_refusal(404, &Value::Null).code, "unsupported");
    }

    #[test]
    fn the_hub_follows_each_chat_once_and_again_after_a_reconnect() {
        let (events, mut heard) = broadcast::channel(16);
        let hub = ChatHub::new(events);

        hub.watch("7");
        assert_eq!(
            heard.try_recv().ok(),
            Some(ControlEvent::Chat {
                chat: "7".into(),
                update: json!({ "type": "offline" })
            })
        );

        let mut session = hub.attach();
        assert_eq!(session.try_recv().ok(), Some(Follow::Watch("7".into())));
        hub.watch("7");
        hub.watch("8");
        assert_eq!(session.try_recv().ok(), Some(Follow::Watch("8".into())));
        hub.unwatch("7");
        assert!(session.try_recv().is_err(), "7 still has a follower");
        hub.unwatch("7");
        assert_eq!(session.try_recv().ok(), Some(Follow::Unwatch("7".into())));

        hub.detach();
        let mut again = hub.attach();
        assert_eq!(again.try_recv().ok(), Some(Follow::Watch("8".into())));
        assert_eq!(hub.followed(), vec!["8".to_string()]);
    }
}
