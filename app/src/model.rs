//! What the daemon reports, as the app reads it (CONTROL.md), and what the
//! tray shows for it.
//!
//! Fields are lenient: a newer or older daemon that adds or drops a field
//! still parses.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Status {
    pub version: String,
    pub pid: u32,
    pub connection: ConnectionStatus,
    pub server: Option<String>,
    pub device_id: Option<String>,
    pub paused: bool,
    pub roots: Vec<RootStatus>,
    /// The latest tool call. Filled in by the app from the audit log and
    /// its events; the daemon's status doesn't carry it.
    pub last_access: Option<AuditEntry>,
    /// Counts audit events, so every new entry is a change the window sees.
    #[serde(skip)]
    pub activity: u64,
    pub pairing: Option<Pairing>,
    pub config_file: Option<PathBuf>,
    pub audit_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ConnectionStatus {
    /// `not_paired`, `connecting`, `connected`, `offline` or `revoked`.
    pub connection: String,
    pub since: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct RootStatus {
    pub id: String,
    pub label: String,
    pub available: bool,
    /// `pending`, `indexing`, `ready`, `error` or `disabled`.
    pub index: String,
    pub indexed_files: Option<u64>,
    pub local_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct AuditEntry {
    pub ts: String,
    pub event: String,
    pub chat_id: Option<String>,
    pub tool: Option<String>,
    pub path: Option<String>,
    pub query: Option<String>,
    /// `allowed`, `denied` or `error`.
    pub decision: Option<String>,
    pub code: Option<String>,
    pub reason: Option<String>,
    pub bytes: Option<u64>,
    pub results: Option<u64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Pairing {
    /// `waiting` or `failed`.
    pub state: String,
    pub server: String,
    pub device_name: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_at: String,
    pub fingerprint: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct DenyList {
    pub builtin: Vec<String>,
    pub extra: Vec<String>,
    pub removed: Vec<String>,
    pub own_dirs: Vec<PathBuf>,
    pub allow_hardlinks: bool,
    pub config_file: Option<PathBuf>,
}

impl Status {
    pub fn is_paired(&self) -> bool {
        self.server.is_some()
    }

    /// Roots whose first index pass hasn't finished.
    pub fn indexing(&self) -> usize {
        self.roots
            .iter()
            .filter(|r| r.available && matches!(r.index.as_str(), "pending" | "indexing"))
            .count()
    }

    /// The server's host, for "Connected to chatwithwork.com".
    pub fn server_host(&self) -> Option<String> {
        self.server.as_deref().map(host_of)
    }
}

pub fn host_of(origin: &str) -> String {
    origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .trim_end_matches('/')
        .to_string()
}

/// The overall state, which picks the tray icon and the first menu line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// The daemon isn't running.
    Stopped,
    NotPaired,
    Paused,
    Connecting,
    Online,
    Offline,
    Revoked,
}

impl Health {
    pub fn of(status: Option<&Status>) -> Self {
        let Some(status) = status else {
            return Self::Stopped;
        };
        if !status.is_paired() {
            return Self::NotPaired;
        }
        if status.paused {
            return Self::Paused;
        }
        match status.connection.connection.as_str() {
            "connected" => Self::Online,
            "connecting" => Self::Connecting,
            "revoked" => Self::Revoked,
            "not_paired" => Self::NotPaired,
            _ => Self::Offline,
        }
    }

    /// A few words, for tight spots such as the sidebar.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::NotPaired => "Not paired",
            Self::Offline => "Offline",
            Self::Revoked => "Disconnected",
            other => other.label(),
        }
    }

    /// Chat with Work can use this computer right now.
    pub fn is_serving(self) -> bool {
        self == Self::Online
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Not running",
            Self::NotPaired => "Not connected to Chat with Work",
            Self::Paused => "Paused",
            Self::Connecting => "Connecting…",
            Self::Online => "Online",
            Self::Offline => "Offline, retrying",
            Self::Revoked => "Disconnected from Chat with Work",
        }
    }
}

/// The platform's words for things, so menus read as native ones do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    pub fn settings_item(self) -> &'static str {
        match self {
            Self::MacOs => "Settings…",
            Self::Windows | Self::Linux => "Settings",
        }
    }

    pub fn quit_item(self) -> &'static str {
        match self {
            Self::MacOs => "Quit Chat with Work Local Agent",
            Self::Windows => "Exit",
            Self::Linux => "Quit",
        }
    }
}

/// Everything the tray shows. Built from the status, so it can be tested
/// without a tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayView {
    pub health: Health,
    /// First, disabled menu line.
    pub headline: String,
    /// Second, disabled menu line: indexing or the last access.
    pub detail: Option<String>,
    /// `Some(true)` offers Resume, `Some(false)` Pause, `None` neither.
    pub paused: Option<bool>,
    pub tooltip: String,
    /// Draw the icon faded: nothing is being served.
    pub dimmed: bool,
}

impl TrayView {
    pub fn new(status: Option<&Status>) -> Self {
        let health = Health::of(status);
        let mut headline = health.label().to_string();
        if let Some(status) = status
            && health == Health::Online
        {
            headline = match status.roots.len() {
                0 => "Online, nothing shared".into(),
                1 => "Online, sharing 1 folder".into(),
                n => format!("Online, sharing {n} folders"),
            };
        }
        let detail = status.and_then(|s| {
            let indexing = s.indexing();
            if indexing > 0 {
                return Some(if indexing == 1 {
                    "Indexing 1 folder…".to_string()
                } else {
                    format!("Indexing {indexing} folders…")
                });
            }
            s.last_access
                .as_ref()
                .map(|a| format!("Last access {}", describe_access(a)))
        });
        let paused = match status {
            Some(s) if s.is_paired() => Some(s.paused),
            _ => None,
        };
        Self {
            health,
            tooltip: format!("Chat with Work Local Agent: {}", health.label()),
            headline,
            detail,
            paused,
            dimmed: !health.is_serving(),
        }
    }
}

/// "14:32, read Documents:plans/q3.md".
pub fn describe_access(entry: &AuditEntry) -> String {
    let when = crate::time::short(&entry.ts);
    let what = describe_tool(entry);
    format!("{when}, {what}")
}

/// What a tool call did, in a few words.
pub fn describe_tool(entry: &AuditEntry) -> String {
    let tool = entry.tool.as_deref().unwrap_or("request");
    let target = match tool {
        "search" => entry
            .query
            .as_deref()
            .map(|q| format!("“{}”", ellipsize(q, 40))),
        "roots" => None,
        _ => entry.path.as_deref().map(|p| ellipsize(p, 48)),
    };
    let verb = match tool {
        "search" => "searched",
        "read" => "read",
        "list" => "listed",
        "roots" => "listed the shared folders",
        other => other,
    };
    match target {
        Some(target) => format!("{verb} {target}"),
        None => verb.to_string(),
    }
}

/// Shorten `text` to `max` characters, keeping the end of paths visible.
pub fn ellipsize(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    let tail: String = text.chars().skip(count - keep).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status(value: serde_json::Value) -> Status {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn parses_what_the_daemon_sends() {
        let s = status(json!({
            "ok": true,
            "version": "0.1.0",
            "pid": 4242,
            "connection": { "connection": "connected", "since": "2026-09-25T08:00:00Z" },
            "server": "https://chatwithwork.com",
            "device_id": "42",
            "paused": false,
            "roots": [{ "id": "documents", "label": "Documents", "available": true,
                        "index": "ready", "indexed_files": 12, "local_path": "/home/u/Documents" }],
            "last_access": { "ts": "2026-09-25T09:12:03Z", "event": "tool", "tool": "read",
                             "path": "documents:plans/q3.md", "decision": "allowed" },
            "pairing": null,
            "something_new": [1, 2, 3],
        }));
        assert!(s.is_paired());
        assert_eq!(s.server_host().as_deref(), Some("chatwithwork.com"));
        assert_eq!(s.roots[0].indexed_files, Some(12));
        assert_eq!(Health::of(Some(&s)), Health::Online);
    }

    #[test]
    fn health_follows_the_daemon() {
        assert_eq!(Health::of(None), Health::Stopped);
        let mut s = status(json!({ "connection": { "connection": "not_paired" } }));
        assert_eq!(Health::of(Some(&s)), Health::NotPaired);
        s.server = Some("https://chatwithwork.com".into());
        s.connection.connection = "offline".into();
        assert_eq!(Health::of(Some(&s)), Health::Offline);
        s.connection.connection = "revoked".into();
        assert_eq!(Health::of(Some(&s)), Health::Revoked);
        s.paused = true;
        assert_eq!(Health::of(Some(&s)), Health::Paused, "paused wins");
    }

    #[test]
    fn the_tray_says_what_is_going_on() {
        let stopped = TrayView::new(None);
        assert_eq!(stopped.headline, "Not running");
        assert_eq!(stopped.paused, None, "nothing to pause");
        assert!(stopped.dimmed);

        let mut s = status(json!({
            "connection": { "connection": "connected" },
            "server": "https://chatwithwork.com",
            "roots": [
                { "id": "a", "label": "A", "available": true, "index": "indexing" },
                { "id": "b", "label": "B", "available": true, "index": "ready" },
            ],
        }));
        let view = TrayView::new(Some(&s));
        assert_eq!(view.headline, "Online, sharing 2 folders");
        assert_eq!(view.detail.as_deref(), Some("Indexing 1 folder…"));
        assert_eq!(view.paused, Some(false));
        assert!(!view.dimmed);

        s.roots[0].index = "ready".into();
        s.last_access = Some(AuditEntry {
            ts: "2026-09-25T09:12:03Z".into(),
            event: "tool".into(),
            tool: Some("search".into()),
            query: Some("falcon budget".into()),
            ..AuditEntry::default()
        });
        let view = TrayView::new(Some(&s));
        let detail = view.detail.unwrap();
        assert!(detail.starts_with("Last access "), "{detail}");
        assert!(detail.ends_with("searched “falcon budget”"), "{detail}");

        s.paused = true;
        let view = TrayView::new(Some(&s));
        assert_eq!(view.headline, "Paused");
        assert_eq!(view.paused, Some(true));
        assert!(view.dimmed);
    }

    #[test]
    fn tool_calls_read_as_sentences() {
        let entry = |tool: &str, path: Option<&str>| AuditEntry {
            tool: Some(tool.into()),
            path: path.map(Into::into),
            ..AuditEntry::default()
        };
        assert_eq!(
            describe_tool(&entry("read", Some("docs:a.md"))),
            "read docs:a.md"
        );
        assert_eq!(describe_tool(&entry("list", Some("docs:"))), "listed docs:");
        assert_eq!(
            describe_tool(&entry("roots", None)),
            "listed the shared folders"
        );
        let long = format!("docs:{}/end.md", "x".repeat(80));
        let described = describe_tool(&entry("read", Some(&long)));
        assert!(
            described.ends_with("/end.md") && described.contains('…'),
            "{described}"
        );
    }

    #[test]
    fn menus_use_each_platforms_words() {
        assert_eq!(Platform::MacOs.settings_item(), "Settings…");
        assert_eq!(Platform::Windows.quit_item(), "Exit");
        assert_eq!(
            Platform::MacOs.quit_item(),
            "Quit Chat with Work Local Agent"
        );
    }
}
