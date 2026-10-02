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
//! The chat list stays current the same way, through one subscription to
//! `LocalAgent::ChatsChannel` however many clients watch it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};

use crate::auth::client::{Body, RefreshError};
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

/// The longest image the daemon fetches for a client.
pub const MAX_ASSET_BYTES: usize = 1024 * 1024;

/// An image path on the paired server, as the chat API gives them:
/// `/assets/` and then only letters, digits, `-`, `_`, `.` and `/`, never
/// `..` or `//`. Checked before anything is fetched, so nothing else on the
/// server (and nothing elsewhere) can be asked for.
pub fn asset_path(path: &str) -> Result<&str, Refusal> {
    let ok = path.len() <= 512
        && path
            .strip_prefix("/assets/")
            .is_some_and(|rest| !rest.is_empty() && !rest.ends_with('/'))
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
        && !path.contains("..")
        && !path.contains("//");
    if ok {
        Ok(path)
    } else {
        Err(Refusal::new(
            "invalid",
            format!("{path:?} isn't an image of Chat with Work's"),
        ))
    }
}

/// What a tunnel session is asked to do with the server's chat channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Follow {
    Watch(String),
    Unwatch(String),
    /// Keep the chat list current.
    WatchList,
    UnwatchList,
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
    /// Clients watching the chat list.
    list_watchers: usize,
    /// The server confirmed the list's subscription on this session.
    list_live: bool,
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

    /// One more client watching the chat list. The first one subscribes on
    /// the server, and so does one that comes while the server hasn't
    /// confirmed it (it refused, say, before chats were allowed): asking
    /// again is how a client tries again. Without a session, it hears
    /// `offline`.
    pub fn watch_list(&self) {
        let mut state = self.state.lock().expect("chat hub");
        state.list_watchers += 1;
        match &state.session {
            Some(_) if state.list_live => self.note_list("watching"),
            Some(session) => {
                let _ = session.send(Follow::WatchList);
            }
            None => self.note_list("offline"),
        }
    }

    pub fn unwatch_list(&self) {
        let mut state = self.state.lock().expect("chat hub");
        state.list_watchers = state.list_watchers.saturating_sub(1);
        if state.list_watchers == 0 {
            state.list_live = false;
            if let Some(session) = &state.session {
                let _ = session.send(Follow::UnwatchList);
            }
        }
    }

    /// The server confirmed the list's subscription, or refused it.
    pub fn list_confirmed(&self, live: bool) {
        self.state.lock().expect("chat hub").list_live = live;
        self.note_list(if live { "watching" } else { "refused" });
    }

    /// A tunnel session starts: it receives what to follow, starting with
    /// every chat followed now, and the list if anyone watches it.
    pub fn attach(&self) -> mpsc::UnboundedReceiver<Follow> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut state = self.state.lock().expect("chat hub");
        for chat in state.followers.keys() {
            let _ = tx.send(Follow::Watch(chat.clone()));
        }
        if state.list_watchers > 0 {
            let _ = tx.send(Follow::WatchList);
        }
        state.list_live = false;
        state.session = Some(tx);
        rx
    }

    /// The session ended: followers wait for the next one.
    pub fn detach(&self) {
        let mut state = self.state.lock().expect("chat hub");
        state.session = None;
        state.list_live = false;
        for chat in state.followers.keys() {
            self.note(chat, "offline");
        }
        if state.list_watchers > 0 {
            self.note_list("offline");
        }
    }

    /// A change to the chat list the server sent.
    pub fn publish_list(&self, update: Value) {
        let _ = self.events.send(ControlEvent::Chats { update });
    }

    /// A note about the list's subscription: `watching` once the server
    /// confirms it (list the chats then), `refused`, `offline` without a
    /// tunnel, `unsupported` on a connection that can't follow it.
    pub fn note_list(&self, what: &str) {
        self.publish_list(json!({ "event": what }));
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

/// The largest file a question can carry, as Chat with Work allows it.
pub const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

/// A question for [`ChatClient::send`]: in `chat`, or in a new chat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Question {
    pub chat: Option<String>,
    pub text: String,
    /// A project for a new chat.
    pub project: Option<u64>,
    /// The model to answer with, from `models`.
    pub model: Option<String>,
    /// Files uploaded with [`ChatClient::upload`], by `signed_id`.
    pub attachments: Vec<String>,
}

/// A file to upload, as a client read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
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
        self.call("GET", "local_agent/chats", Body::Empty)
    }

    pub fn show(&self, chat: &str) -> Result<Value> {
        let chat = chat_number(chat)?;
        self.call("GET", &format!("local_agent/chats/{chat}"), Body::Empty)
    }

    /// The models a question can be asked with, and the owner's default.
    pub fn models(&self) -> Result<Value> {
        newer(
            self.call("GET", "local_agent/models", Body::Empty),
            "doesn't let you pick a model from here yet. Chats use your default model",
        )
    }

    pub fn send(&self, question: &Question) -> Result<Value> {
        let mut body = json!({ "content": question.text });
        if let Some(model) = &question.model {
            body["model_id"] = json!(model);
        }
        if !question.attachments.is_empty() {
            body["attachments"] = json!(question.attachments);
        }
        match &question.chat {
            Some(chat) => {
                let chat = chat_number(chat)?;
                self.call(
                    "POST",
                    &format!("local_agent/chats/{chat}/messages"),
                    Body::Json(&body),
                )
            }
            None => {
                if let Some(project) = question.project {
                    body["project_id"] = json!(project);
                }
                self.call("POST", "local_agent/chats", Body::Json(&body))
            }
        }
    }

    pub fn cancel(&self, chat: &str) -> Result<Value> {
        let chat = chat_number(chat)?;
        self.call(
            "POST",
            &format!("local_agent/chats/{chat}/cancellation"),
            Body::Empty,
        )
    }

    /// Approve a change the answer stopped at, as the web's Approve does.
    pub fn approve(&self, chat: &str, tool_call: &str, for_rest_of_chat: bool) -> Result<Value> {
        let (chat, tool_call) = (chat_number(chat)?, tool_call_id(tool_call)?);
        self.call(
            "POST",
            &format!("local_agent/chats/{chat}/tool_calls/{tool_call}/approval"),
            Body::Json(&json!({ "for_rest_of_chat": for_rest_of_chat })),
        )
    }

    /// Deny a change the answer stopped at, with what to do instead.
    pub fn deny(&self, chat: &str, tool_call: &str, reason: Option<&str>) -> Result<Value> {
        let (chat, tool_call) = (chat_number(chat)?, tool_call_id(tool_call)?);
        let mut body = json!({});
        if let Some(reason) = reason.map(str::trim).filter(|r| !r.is_empty()) {
            body["reason"] = json!(reason);
        }
        self.call(
            "POST",
            &format!("local_agent/chats/{chat}/tool_calls/{tool_call}/denial"),
            Body::Json(&body),
        )
    }

    /// Upload a file for a question: `{signed_id, filename, byte_size,
    /// content_type}`.
    pub fn upload(&self, upload: &Upload) -> Result<Value> {
        let boundary = boundary()?;
        let mut data = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
            upload.filename.replace(['"', '\\'], "_"),
            upload.content_type
        )
        .into_bytes();
        data.extend_from_slice(&upload.data);
        data.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        newer(
            self.call(
                "POST",
                "local_agent/uploads",
                Body::Bytes {
                    content_type: format!("multipart/form-data; boundary={boundary}"),
                    data,
                },
            ),
            "doesn't take attachments from here yet",
        )
    }

    /// Answer `message` again, or the latest question.
    pub fn retry(&self, chat: &str, message: Option<&str>) -> Result<Value> {
        let chat = chat_number(chat)?;
        let body = message_body(message)?;
        newer(
            self.call(
                "POST",
                &format!("local_agent/chats/{chat}/retry"),
                Body::Json(&body),
            ),
            "can't retry answers from here yet",
        )
    }

    /// A new chat with the conversation up to `message`, or all of it.
    pub fn branch(&self, chat: &str, message: Option<&str>) -> Result<Value> {
        let chat = chat_number(chat)?;
        let body = message_body(message)?;
        newer(
            self.call(
                "POST",
                &format!("local_agent/chats/{chat}/branches"),
                Body::Json(&body),
            ),
            "can't branch chats from here yet",
        )
    }

    pub fn rename(&self, chat: &str, title: &str) -> Result<Value> {
        let chat = chat_number(chat)?;
        newer(
            self.call(
                "PATCH",
                &format!("local_agent/chats/{chat}"),
                Body::Json(&json!({ "title": title })),
            ),
            "can't rename chats from here yet",
        )
    }

    pub fn delete(&self, chat: &str) -> Result<Value> {
        let chat = chat_number(chat)?;
        newer(
            self.call("DELETE", &format!("local_agent/chats/{chat}"), Body::Empty),
            "can't delete chats from here yet",
        )
    }

    /// Make the chat's public link: `{url, expires_at, chat}`.
    pub fn share(&self, chat: &str) -> Result<Value> {
        let chat = chat_number(chat)?;
        newer(
            self.call(
                "POST",
                &format!("local_agent/chats/{chat}/share"),
                Body::Empty,
            ),
            "can't share chats from here yet",
        )
    }

    /// Stop sharing the chat: its link stops working.
    pub fn unshare(&self, chat: &str) -> Result<Value> {
        let chat = chat_number(chat)?;
        newer(
            self.call(
                "DELETE",
                &format!("local_agent/chats/{chat}/share"),
                Body::Empty,
            ),
            "can't share chats from here yet",
        )
    }

    /// Answer a question the tool's server asked: the form's values, or
    /// nothing once the person has been to the page it asked them to open.
    pub fn answer(&self, chat: &str, tool_call: &str, input: Option<&Value>) -> Result<Value> {
        let (chat, tool_call) = (chat_number(chat)?, tool_call_id(tool_call)?);
        let body = match input {
            Some(input) => json!({ "input": input }),
            None => json!({}),
        };
        newer(
            self.call(
                "POST",
                &format!("local_agent/chats/{chat}/tool_calls/{tool_call}/input"),
                Body::Json(&body),
            ),
            "can't answer questions from here yet",
        )
    }

    /// Decline a question the tool's server asked.
    pub fn decline(&self, chat: &str, tool_call: &str) -> Result<Value> {
        let (chat, tool_call) = (chat_number(chat)?, tool_call_id(tool_call)?);
        newer(
            self.call(
                "DELETE",
                &format!("local_agent/chats/{chat}/tool_calls/{tool_call}/input"),
                Body::Empty,
            ),
            "can't answer questions from here yet",
        )
    }

    pub fn request_access(&self) -> Result<Value> {
        self.call("POST", "local_agent/chat_access_request", Body::Empty)
    }

    /// An image under `/assets/` on the paired server, without the token
    /// (the server serves them to anyone): `{content_type, data}`, the
    /// bytes in base64. Only images, at most [`MAX_ASSET_BYTES`], and never
    /// through a redirect.
    pub fn asset(&self, path: &str) -> Result<Value> {
        use base64::Engine;
        let path = asset_path(path)?;
        let (status, content_type, bytes) =
            self.tokens
                .client()
                .get_asset(path, MAX_ASSET_BYTES)
                .map_err(|e| Refusal::new("unreachable", format!("Can't fetch {path}: {e:#}")))?;
        let content_type = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match status {
            200 if content_type.starts_with("image/") => Ok(json!({
                "path": path,
                "content_type": content_type,
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            })),
            200 => Err(Refusal::new(
                "unsupported",
                format!("{path} isn't an image ({content_type})"),
            )
            .into()),
            404 => Err(Refusal::new("not_found", format!("Chat with Work has no {path}")).into()),
            _ => Err(Refusal::new(
                "unreachable",
                format!("Chat with Work answered {status} for {path}"),
            )
            .into()),
        }
    }

    /// One call, refreshing the token and trying again once when the server
    /// says it's expired, or lacks chats that were allowed since it was
    /// issued. Failures come back as [`Refusal`]s with the server's code.
    fn call(&self, method: &str, path: &str, body: Body) -> Result<Value> {
        let mut retried = false;
        loop {
            let token = self.tokens.access_token().map_err(refresh_refusal)?;
            let (status, response) = self
                .tokens
                .client()
                .send(self.tokens.key(), method, path, &token, &body)
                .map_err(|e| {
                    Refusal::new("unreachable", format!("Can't reach Chat with Work: {e:#}"))
                })?;
            let code = response["error"].as_str().unwrap_or_default();
            match status {
                200..=299 if response.is_object() || status == 202 || status == 204 => {
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

/// A call to an endpoint an older server doesn't have: its "no chat API"
/// refusal says what this server can't do instead.
fn newer(result: Result<Value>, cannot: &str) -> Result<Value> {
    result.map_err(|e| match e.downcast::<Refusal>() {
        Ok(refusal) if refusal.code == "unsupported" => Refusal::new(
            "unsupported",
            format!("This Chat with Work server {cannot}."),
        )
        .into(),
        Ok(refusal) => refusal.into(),
        Err(e) => e,
    })
}

/// `{"message_id": N}` for a message, or `{}` for the latest.
fn message_body(message: Option<&str>) -> Result<Value, Refusal> {
    Ok(match message {
        Some(message) => {
            json!({ "message_id": message_id(message)?.parse::<u64>().unwrap_or_default() })
        }
        None => json!({}),
    })
}

/// A tool call's ID, as the server numbers them. Checked before it goes
/// into a path.
pub fn tool_call_id(id: &str) -> Result<&str, Refusal> {
    digits(id, "tool call")
}

pub fn message_id(id: &str) -> Result<&str, Refusal> {
    digits(id, "message")
}

fn digits<'a>(id: &'a str, what: &str) -> Result<&'a str, Refusal> {
    if !id.is_empty() && id.len() <= 18 && id.bytes().all(|b| b.is_ascii_digit()) {
        Ok(id)
    } else {
        Err(Refusal::new(
            "bad_request",
            format!("bad request: {id:?} is not a {what} ID"),
        ))
    }
}

/// A multipart boundary no file is likely to contain.
fn boundary() -> Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("no randomness for an upload"))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("cww-{hex}"))
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
    fn tool_call_and_message_ids_are_digits_only() {
        assert_eq!(tool_call_id("12").unwrap(), "12");
        assert_eq!(message_id("3").unwrap(), "3");
        for bad in ["", "1/2", "../approval", "12?x"] {
            assert!(tool_call_id(bad).is_err(), "{bad:?}");
            assert!(message_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_older_server_says_what_it_cant_do() {
        let refused = newer(
            Err(server_refusal(404, &Value::Null).into()),
            "can't retry answers from here yet",
        )
        .unwrap_err();
        let refusal = refused.downcast_ref::<Refusal>().unwrap();
        assert_eq!(refusal.code, "unsupported");
        assert_eq!(
            refusal.message,
            "This Chat with Work server can't retry answers from here yet."
        );
        let kept = newer(
            Err(Refusal::new("not_found", "No such chat").into()),
            "can't retry answers from here yet",
        )
        .unwrap_err();
        assert_eq!(kept.downcast_ref::<Refusal>().unwrap().code, "not_found");
    }

    #[test]
    fn only_image_paths_on_the_server_are_fetched() {
        for good in [
            "/assets/providers/slack-0c9450af.svg",
            "/assets/mimetypes/application-pdf-a0ad3afb.svg",
        ] {
            assert_eq!(asset_path(good).unwrap(), good);
        }
        for bad in [
            "",
            "/assets/",
            "/assets/a/",
            "assets/x.svg",
            "/local_agent/chats",
            "/assets/../local_agent/chats",
            "/assets//evil.example/x.svg",
            "https://evil.example/assets/x.svg",
            "//evil.example/assets/x.svg",
            "/assets/x.svg?y=1",
            "/assets/x.svg#y",
            "/assets/x y.svg",
            "/assets/x%2e%2e/y.svg",
            "/assets/x\\y.svg",
        ] {
            assert!(asset_path(bad).is_err(), "{bad:?}");
        }
        assert!(asset_path(&format!("/assets/{}", "a".repeat(600))).is_err());
    }

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

    #[test]
    fn the_hub_watches_the_list_once_and_asks_again_until_confirmed() {
        let (events, mut heard) = broadcast::channel(16);
        let hub = ChatHub::new(events);
        let note = |what: &str| ControlEvent::Chats {
            update: json!({ "event": what }),
        };

        hub.watch_list();
        assert_eq!(heard.try_recv().ok(), Some(note("offline")));
        let mut session = hub.attach();
        assert_eq!(session.try_recv().ok(), Some(Follow::WatchList));

        // Refused (no chat access yet): the next watcher asks again.
        hub.list_confirmed(false);
        assert_eq!(heard.try_recv().ok(), Some(note("refused")));
        hub.watch_list();
        assert_eq!(session.try_recv().ok(), Some(Follow::WatchList));
        hub.list_confirmed(true);
        assert_eq!(heard.try_recv().ok(), Some(note("watching")));
        // Once live, a newcomer only hears that it is.
        hub.watch_list();
        assert!(session.try_recv().is_err());
        assert_eq!(heard.try_recv().ok(), Some(note("watching")));

        for _ in 0..2 {
            hub.unwatch_list();
        }
        assert!(session.try_recv().is_err(), "one watcher left");
        hub.unwatch_list();
        assert_eq!(session.try_recv().ok(), Some(Follow::UnwatchList));

        // A reconnect subscribes again only while someone watches.
        hub.watch_list();
        let _ = session.try_recv();
        hub.detach();
        assert_eq!(heard.try_recv().ok(), Some(note("offline")));
        let mut again = hub.attach();
        assert_eq!(again.try_recv().ok(), Some(Follow::WatchList));
    }
}
