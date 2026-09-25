//! The local configuration file (`~/.config/cww/config.toml`).
//!
//! Everything that decides what the server may see lives here, on the user's
//! machine. The server can't change any of it.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::{Paths, write_private_file};

pub const DEFAULT_SERVER: &str = "https://chatwithwork.com";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The paired server. Absent until `cww login` succeeds.
    pub server: Option<ServerConfig>,
    /// Where secrets are kept: `keyring` (OS keychain) or `file` (0600 file).
    pub secret_store: Option<String>,
    /// Pause answering tool calls. Survives restarts.
    pub paused: bool,
    /// Folders shared with the server. Nothing outside them is readable.
    pub roots: Vec<Root>,
    pub deny: DenyConfig,
    pub limits: Limits,
    pub index: IndexConfig,
    pub sandbox: SandboxConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxConfig {
    /// Confine the daemon with Landlock (Linux) or Seatbelt (macOS), so the
    /// kernel refuses files outside the shared folders even if cww had a bug.
    pub enabled: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Origin of the Chat with Work server, e.g. `https://chatwithwork.com`.
    pub url: String,
    /// Device ID issued by the server at pairing.
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Root {
    /// Short, stable ID used in tool paths (`<id>:relative/path`).
    pub id: String,
    /// Human label shown to the server and the model.
    pub label: String,
    /// Absolute, canonical path. Never sent to the server.
    pub path: PathBuf,
    /// Follow symlinks that stay inside the root. Off by default. Links that
    /// leave the root are refused either way.
    #[serde(default)]
    pub follow_symlinks: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DenyConfig {
    /// Extra deny patterns, same syntax as the built-in list.
    pub extra: Vec<String>,
    /// Built-in patterns to drop. Only editable here, never from the server.
    pub remove: Vec<String>,
    /// Serve files with more than one hard link. Off by default, because a
    /// hard link inside a root can point at a file that lives outside it.
    pub allow_hardlinks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// Tool calls accepted per rolling minute, across all chats.
    pub calls_per_minute: u32,
    /// Maximum characters one `read` call returns.
    pub read_chars_per_call: usize,
    /// Characters `read` may return per chat in a rolling hour.
    pub read_chars_per_chat_hour: usize,
    /// Characters `read` may return across all chats in a rolling hour.
    pub read_chars_per_hour: usize,
    /// Maximum search hits per call.
    pub search_hits: usize,
    /// Maximum characters per search snippet.
    pub snippet_chars: usize,
    /// Maximum entries per `list` page.
    pub list_page: usize,
    /// Largest file `read` will open, in bytes.
    pub max_file_bytes: u64,
    /// Largest file the indexer will extract, in bytes.
    pub max_index_file_bytes: u64,
    /// Largest WebSocket message accepted or sent, in bytes.
    pub max_message_bytes: usize,
    /// Time budget for the live grep fallback, in milliseconds.
    pub grep_timeout_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            calls_per_minute: 120,
            read_chars_per_call: 32_000,
            read_chars_per_chat_hour: 256_000,
            read_chars_per_hour: 2_000_000,
            search_hits: 20,
            snippet_chars: 300,
            list_page: 200,
            max_file_bytes: 64 * 1024 * 1024,
            max_index_file_bytes: 32 * 1024 * 1024,
            max_message_bytes: 256 * 1024,
            grep_timeout_ms: 5_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct IndexConfig {
    /// Build and use the full-text index. Search falls back to live grep
    /// without it.
    pub enabled: bool,
    /// Watch roots for changes. Without it, roots are rescanned periodically.
    pub watch: bool,
    /// Full rescan interval in seconds, to catch events the watcher missed.
    pub rescan_secs: u64,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            watch: true,
            rescan_secs: 30 * 60,
        }
    }
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        let file = paths.config_file();
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", file.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
        }
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing config")?;
        let header = "# Chat with Work local agent (cww) configuration.\n\
                      # The server can't change this file. See README.md for every option.\n\n";
        write_private_file(&paths.config_file(), format!("{header}{text}").as_bytes())
    }

    pub fn root(&self, id: &str) -> Option<&Root> {
        self.roots.iter().find(|r| r.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());
        assert_eq!(Config::load(&paths).unwrap(), Config::default());

        let mut config = Config::default();
        config.roots.push(Root {
            id: "abc123".into(),
            label: "Docs".into(),
            path: "/tmp/docs".into(),
            follow_symlinks: false,
        });
        config.deny.extra.push("*.secret".into());
        config.save(&paths).unwrap();
        assert_eq!(Config::load(&paths).unwrap(), config);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("allow_everything = true").is_err());
    }
}
