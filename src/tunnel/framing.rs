//! WebSocket framing for MCP messages.
//!
//! Two framings are supported, chosen by the WebSocket subprotocol the server
//! selects from the two the daemon offers:
//!
//! - `actioncable-v1-json` (default): the Action Cable protocol that Rails
//!   speaks. The daemon subscribes to `LocalAgent::Channel`; the server sends
//!   each JSON-RPC message as the `message` of a channel broadcast, and the
//!   daemon sends each JSON-RPC message as the `data` string of a `message`
//!   command.
//! - `mcp`: one JSON-RPC message per text frame, nothing else. For servers or
//!   relays that don't use Action Cable.
//!
//! With Action Cable framing the daemon may also subscribe to
//! `LocalAgent::ChatChannel`, once per chat the terminal UI follows, and
//! receive that chat's updates. Those never reach the MCP server.

use serde_json::{Value, json};

pub const SUBPROTOCOL_ACTIONCABLE: &str = "actioncable-v1-json";
pub const SUBPROTOCOL_MCP: &str = "mcp";
/// Offered in this order; the server picks one.
pub const OFFERED_SUBPROTOCOLS: &str = "actioncable-v1-json, mcp";
/// The Action Cable channel identifier, byte for byte.
pub const CHANNEL_IDENTIFIER: &str = r#"{"channel":"LocalAgent::Channel"}"#;
/// The channel for one followed chat: `{"channel":…,"chat":"42"}`.
pub const CHAT_CHANNEL: &str = "LocalAgent::ChatChannel";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    ActionCable,
    Mcp,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// A JSON-RPC message for the MCP server.
    Mcp(Value),
    Welcome,
    Ping,
    Confirmed,
    Rejected,
    /// An update of a followed chat. Never handed to the MCP server.
    Chat {
        chat: String,
        update: Value,
    },
    ChatConfirmed(String),
    ChatRejected(String),
    Disconnect {
        reason: String,
        reconnect: bool,
    },
    /// Anything else, dropped.
    Ignored,
}

impl Framing {
    /// The framing for the subprotocol the server selected. A server that
    /// selects none gets Action Cable framing.
    pub fn for_protocol(selected: Option<&str>) -> Option<Self> {
        match selected {
            None | Some(SUBPROTOCOL_ACTIONCABLE) => Some(Self::ActionCable),
            Some(SUBPROTOCOL_MCP) => Some(Self::Mcp),
            Some(_) => None,
        }
    }

    /// Frames to send right after the socket opens.
    pub fn opening_frames(self) -> Vec<String> {
        match self {
            Self::ActionCable => vec![
                json!({ "command": "subscribe", "identifier": CHANNEL_IDENTIFIER }).to_string(),
            ],
            Self::Mcp => Vec::new(),
        }
    }

    /// The command that follows `chat`, when this framing can.
    pub fn follow_chat(self, chat: &str) -> Option<String> {
        self.chat_command("subscribe", chat)
    }

    pub fn unfollow_chat(self, chat: &str) -> Option<String> {
        self.chat_command("unsubscribe", chat)
    }

    fn chat_command(self, command: &str, chat: &str) -> Option<String> {
        match self {
            Self::ActionCable => {
                Some(json!({ "command": command, "identifier": chat_identifier(chat) }).to_string())
            }
            Self::Mcp => None,
        }
    }

    /// Wrap one serialized JSON-RPC message.
    pub fn encode(self, jsonrpc: &str) -> String {
        match self {
            Self::ActionCable => json!({
                "command": "message",
                "identifier": CHANNEL_IDENTIFIER,
                "data": jsonrpc,
            })
            .to_string(),
            Self::Mcp => jsonrpc.to_string(),
        }
    }

    pub fn decode(self, text: &str) -> Inbound {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return Inbound::Ignored;
        };
        match self {
            Self::Mcp => Inbound::Mcp(value),
            Self::ActionCable => decode_action_cable(value),
        }
    }
}

pub fn chat_identifier(chat: &str) -> String {
    json!({ "channel": CHAT_CHANNEL, "chat": chat }).to_string()
}

/// The chat a chat channel identifier names.
fn chat_of(identifier: Option<&str>) -> Option<String> {
    let value: Value = serde_json::from_str(identifier?).ok()?;
    if value["channel"] != CHAT_CHANNEL {
        return None;
    }
    value["chat"].as_str().map(str::to_string)
}

fn decode_action_cable(mut value: Value) -> Inbound {
    let identifier = value.get("identifier").and_then(Value::as_str);
    let chat = chat_of(identifier);
    match value.get("type").and_then(Value::as_str) {
        Some("welcome") => return Inbound::Welcome,
        Some("ping") => return Inbound::Ping,
        Some("confirm_subscription") => {
            return chat.map_or(Inbound::Confirmed, Inbound::ChatConfirmed);
        }
        // Only the device's own channel being refused ends the session.
        Some("reject_subscription") => {
            return match chat {
                Some(chat) => Inbound::ChatRejected(chat),
                None if identifier.is_none_or(|i| i == CHANNEL_IDENTIFIER) => Inbound::Rejected,
                None => Inbound::Ignored,
            };
        }
        Some("disconnect") => {
            return Inbound::Disconnect {
                reason: value["reason"].as_str().unwrap_or_default().to_string(),
                reconnect: value["reconnect"].as_bool().unwrap_or(true),
            };
        }
        Some(_) => return Inbound::Ignored,
        None => {}
    }
    if let Some(chat) = chat {
        return match value.get_mut("message").map(Value::take) {
            Some(update @ Value::Object(_)) => Inbound::Chat { chat, update },
            _ => Inbound::Ignored,
        };
    }
    if identifier != Some(CHANNEL_IDENTIFIER) {
        return Inbound::Ignored;
    }
    match value.get_mut("message").map(Value::take) {
        // A string message holds serialized JSON; an object is the message.
        Some(Value::String(s)) => serde_json::from_str(&s).map_or(Inbound::Ignored, Inbound::Mcp),
        Some(message @ Value::Object(_)) => Inbound::Mcp(message),
        _ => Inbound::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_cable_round_trip() {
        let f = Framing::ActionCable;
        assert_eq!(
            f.opening_frames(),
            vec![r#"{"command":"subscribe","identifier":"{\"channel\":\"LocalAgent::Channel\"}"}"#]
        );
        let out: Value =
            serde_json::from_str(&f.encode(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#)).unwrap();
        assert_eq!(out["command"], "message");
        assert_eq!(out["identifier"], CHANNEL_IDENTIFIER);
        assert_eq!(out["data"], r#"{"jsonrpc":"2.0","id":1,"result":{}}"#);

        let inbound = json!({
            "identifier": CHANNEL_IDENTIFIER,
            "message": { "jsonrpc": "2.0", "id": 7, "method": "tools/list" }
        });
        assert_eq!(
            f.decode(&inbound.to_string()),
            Inbound::Mcp(json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list" }))
        );
        let as_string = json!({
            "identifier": CHANNEL_IDENTIFIER,
            "message": r#"{"jsonrpc":"2.0","id":8,"method":"tools/list"}"#
        });
        assert!(matches!(f.decode(&as_string.to_string()), Inbound::Mcp(_)));
        assert_eq!(f.decode(r#"{"type":"welcome"}"#), Inbound::Welcome);
        assert_eq!(f.decode(r#"{"type":"ping","message":1}"#), Inbound::Ping);
        assert_eq!(
            f.decode(r#"{"type":"disconnect","reason":"unauthorized","reconnect":false}"#),
            Inbound::Disconnect {
                reason: "unauthorized".into(),
                reconnect: false
            }
        );
        let other_channel = json!({ "identifier": "{\"channel\":\"Other\"}", "message": {} });
        assert_eq!(f.decode(&other_channel.to_string()), Inbound::Ignored);
    }

    #[test]
    fn chat_updates_stay_apart_from_mcp() {
        let f = Framing::ActionCable;
        let identifier = chat_identifier("42");
        let follow: Value = serde_json::from_str(&f.follow_chat("42").unwrap()).unwrap();
        assert_eq!(follow["command"], "subscribe");
        assert_eq!(follow["identifier"], identifier.as_str());
        assert!(Framing::Mcp.follow_chat("42").is_none());

        let update = json!({ "identifier": identifier, "message": { "type": "chunk", "message_id": 5, "text": "Hi" } });
        assert_eq!(
            f.decode(&update.to_string()),
            Inbound::Chat {
                chat: "42".into(),
                update: json!({ "type": "chunk", "message_id": 5, "text": "Hi" })
            }
        );
        let confirmed = json!({ "identifier": identifier, "type": "confirm_subscription" });
        assert_eq!(
            f.decode(&confirmed.to_string()),
            Inbound::ChatConfirmed("42".into())
        );
        // A refused chat doesn't end the session; a refused device does.
        let rejected = json!({ "identifier": identifier, "type": "reject_subscription" });
        assert_eq!(
            f.decode(&rejected.to_string()),
            Inbound::ChatRejected("42".into())
        );
        let device = json!({ "identifier": CHANNEL_IDENTIFIER, "type": "reject_subscription" });
        assert_eq!(f.decode(&device.to_string()), Inbound::Rejected);
        // A chat update shaped like a JSON-RPC request is still not one.
        let sneaky = json!({ "identifier": identifier, "message": { "jsonrpc": "2.0", "id": 1, "method": "tools/call" } });
        assert!(matches!(
            f.decode(&sneaky.to_string()),
            Inbound::Chat { .. }
        ));
    }

    #[test]
    fn plain_mcp_framing() {
        let f = Framing::for_protocol(Some("mcp")).unwrap();
        assert_eq!(f.encode("{}"), "{}");
        assert!(f.opening_frames().is_empty());
        assert_eq!(f.decode("not json"), Inbound::Ignored);
        assert!(Framing::for_protocol(Some("graphql-ws")).is_none());
        assert_eq!(Framing::for_protocol(None), Some(Framing::ActionCable));
    }
}
