//! The daemon's control channel (CONTROL.md), through the client in the
//! `cww` crate: one JSON request per line, and `subscribe` for a line per
//! change. The crate also does the Windows pipe's ownership checks.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cww::control::{self as api, ControlRequest, Topic};
use serde_json::Value;

use crate::model::{AuditEntry, DenyList, Status};

/// How long to wait for the daemon's socket before trying again anyway,
/// in case a file-system notification was missed.
const RECONNECT_FALLBACK: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub enum ControlError {
    /// No daemon is listening.
    NotRunning,
    /// The daemon refused the request, or the connection failed; the
    /// message is for people.
    Refused(String),
    Other(anyhow::Error),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning => f.write_str("The Local Agent isn't running."),
            Self::Refused(message) => f.write_str(&sentence(message)),
            Self::Other(e) => write!(f, "Couldn't read the Local Agent's answer: {e:#}"),
        }
    }
}

impl std::error::Error for ControlError {}

impl From<serde_json::Error> for ControlError {
    fn from(e: serde_json::Error) -> Self {
        Self::Other(e.into())
    }
}

/// The daemon's messages start lowercase, as CLI errors do. The window
/// shows them as sentences.
fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    let mut out: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

pub type Result<T> = std::result::Result<T, ControlError>;

fn connect(socket: &Path) -> Result<api::Client> {
    match api::Client::connect(socket) {
        Ok(Some(client)) => Ok(client),
        Ok(None) => Err(ControlError::NotRunning),
        Err(e) => Err(ControlError::Refused(format!("{e:#}"))),
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    /// Send one request and wait for its answer.
    pub fn request(&self, request: &ControlRequest) -> Result<Value> {
        connect(&self.socket)?
            .call(request)
            .map_err(|e| ControlError::Refused(format!("{e:#}")))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn status(&self) -> Result<Status> {
        Ok(serde_json::from_value(
            self.request(&ControlRequest::Status)?,
        )?)
    }

    pub fn set_paused(&self, paused: bool) -> Result<()> {
        let request = if paused {
            ControlRequest::Pause
        } else {
            ControlRequest::Resume
        };
        self.request(&request).map(drop)
    }

    pub fn add_root(&self, path: &Path, label: Option<&str>) -> Result<Value> {
        self.request(&ControlRequest::RootsAdd {
            path: path.to_path_buf(),
            label: label.map(str::to_string),
            follow_symlinks: false,
            i_know: false,
        })
    }

    pub fn remove_root(&self, id: &str) -> Result<()> {
        self.request(&ControlRequest::RootsRemove { root: id.into() })
            .map(drop)
    }

    pub fn label_root(&self, id: &str, label: &str) -> Result<()> {
        self.request(&ControlRequest::RootsLabel {
            root: id.into(),
            label: label.into(),
        })
        .map(drop)
    }

    pub fn deny(&self) -> Result<DenyList> {
        Ok(serde_json::from_value(
            self.request(&ControlRequest::Deny)?,
        )?)
    }

    pub fn log(&self, lines: usize) -> Result<Vec<AuditEntry>> {
        let mut value = self.request(&ControlRequest::AuditTail { lines: Some(lines) })?;
        Ok(serde_json::from_value(value["entries"].take())?)
    }

    /// Follow the daemon for as long as the process runs. `on_change` gets
    /// the status whenever it changes, and `None` while no daemon is
    /// running. The thread blocks between events, and waits for the socket
    /// to appear instead of polling for it.
    pub fn spawn_watch(
        &self,
        on_change: impl Fn(Option<Status>) + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        let socket = self.socket.clone();
        let client = self.clone();
        std::thread::Builder::new()
            .name("cww-watch".into())
            .spawn(move || {
                let mut last: Option<Option<Status>> = None;
                let mut publish = |status: Option<Status>| {
                    if last.as_ref() != Some(&status) {
                        on_change(status.clone());
                        last = Some(status);
                    }
                };
                let mut idle_rounds = 0u32;
                loop {
                    match watch_once(&client, &socket, &mut |s| publish(Some(s))) {
                        Ok(()) => idle_rounds = 0,
                        Err(ControlError::NotRunning) => idle_rounds += 1,
                        Err(e) => {
                            idle_rounds += 1;
                            log::warn!("watching the Local Agent: {e}");
                        }
                    }
                    publish(None);
                    wait_for_socket(&socket, RECONNECT_FALLBACK, idle_rounds);
                }
            })
            .expect("spawning the watch thread")
    }
}

/// Follow status and audit events until the daemon goes away. The status
/// the app sees carries the last tool call, and changes with every audit
/// entry so the activity log can refresh.
fn watch_once(client: &Client, socket: &Path, on_status: &mut dyn FnMut(Status)) -> Result<()> {
    let events = connect(socket)?
        .subscribe(&[Topic::Status, Topic::Audit])
        .map_err(|e| ControlError::Refused(format!("{e:#}")))?;
    let mut last_access = client
        .log(200)
        .ok()
        .and_then(|log| log.into_iter().rev().find(|e| e.event == "tool"));
    let mut activity = 0u64;
    let mut status: Option<Status> = None;
    for event in events {
        let mut event = event.map_err(ControlError::Other)?;
        match event["event"].as_str() {
            Some("status") => {
                status = Some(serde_json::from_value(event["status"].take())?);
            }
            Some("audit") => {
                let entry: AuditEntry = serde_json::from_value(event["entry"].take())?;
                activity += 1;
                if entry.event == "tool" {
                    last_access = Some(entry);
                }
            }
            _ => continue,
        }
        if let Some(status) = &mut status {
            status.last_access = last_access.clone();
            status.activity = activity;
            on_status(status.clone());
        }
    }
    Ok(())
}

/// Block until the socket might be connectable again: something changed in
/// its directory, or `fallback` passed.
fn wait_for_socket(socket: &Path, fallback: Duration, rounds: u32) {
    #[cfg(unix)]
    {
        use notify::{RecursiveMode, Watcher};
        use std::sync::mpsc;

        // Watch the closest directory that exists; a daemon that has never
        // run hasn't created the socket's directory yet.
        let closest = || socket.ancestors().skip(1).find(|d| d.is_dir());
        let Some(dir) = closest() else {
            std::thread::sleep(fallback);
            return;
        };
        let (tx, rx) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if event.is_ok_and(|e| !e.kind.is_access()) {
                let _ = tx.send(());
            }
        });
        let mut watcher = match watcher {
            Ok(w) => w,
            Err(e) => {
                log::info!("can't watch for the Local Agent ({e}); checking every minute");
                std::thread::sleep(fallback);
                return;
            }
        };
        if watcher.watch(dir, RecursiveMode::NonRecursive).is_err() {
            std::thread::sleep(fallback);
            return;
        }
        // It may have started while the watch was being set up, or made a
        // deeper directory, whose changes this watch won't see.
        if connect(socket).is_ok() || closest() != Some(dir) {
            return;
        }
        if rx.recv_timeout(fallback).is_ok() {
            // Let the daemon finish binding and chmod-ing the socket.
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    // A named pipe can't be watched for, so look again after 1, 2, 4 ...
    // seconds, then every 30 seconds while the daemon stays down.
    #[cfg(windows)]
    {
        let _ = (socket, fallback);
        let seconds = 1u64 << rounds.saturating_sub(1).min(5);
        std::thread::sleep(Duration::from_secs(seconds.min(30)));
    }
    #[cfg(unix)]
    let _ = rounds;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_errors_read_as_sentences() {
        assert_eq!(
            ControlError::Refused("/x is already shared as docs".into()).to_string(),
            "/x is already shared as docs."
        );
        assert_eq!(
            ControlError::Refused("refusing to share /: it is the whole filesystem".into())
                .to_string(),
            "Refusing to share /: it is the whole filesystem."
        );
        assert_eq!(sentence("done."), "Done.");
    }

    /// The app's reading of every request, against the real daemon.
    #[test]
    fn works_with_the_real_daemon() {
        use std::sync::mpsc;

        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let paths = cww::paths::Paths::under(&base.join("cww"));
        let docs = base.join("Docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("notes.md"), "Falcon launch notes\n").unwrap();
        cww::config::Config {
            secret_store: Some("file".into()),
            ..cww::config::Config::default()
        }
        .save(&paths)
        .unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let shutdown = tokio_util::sync::CancellationToken::new();
        let daemon = runtime.spawn(cww::daemon::run(paths.clone(), shutdown.clone(), false));
        let client = Client::new(paths.socket_path());
        let (tx, rx) = mpsc::channel();
        client.spawn_watch(move |status| {
            let _ = tx.send(status);
        });
        let next = |what: &str, check: &dyn Fn(&Option<Status>) -> bool| -> Option<Status> {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match rx.recv_timeout(left) {
                    Ok(status) if check(&status) => return status,
                    Ok(_) => {}
                    Err(e) => panic!("no status with {what}: {e}"),
                }
            }
        };

        let first = next("the daemon running", &|s| s.is_some()).unwrap();
        assert_eq!(first.connection.connection, "not_paired");
        assert!(!first.is_paired());
        assert_eq!(first.config_file, Some(paths.config_file()));

        let added = client.add_root(&docs, Some("Docs")).unwrap();
        assert_eq!(added["root"]["id"], "docs");
        let ready = next("an indexed folder", &|s| {
            s.as_ref()
                .is_some_and(|s| s.roots.first().is_some_and(|r| r.index == "ready"))
        })
        .unwrap();
        // The daemon reports Windows paths without the \\?\ prefix.
        let shown = docs.to_string_lossy().replace(r"\\?\", "");
        assert_eq!(
            ready.roots[0].local_path.as_deref(),
            Some(std::path::Path::new(&shown))
        );
        assert_eq!(ready.roots[0].indexed_files, Some(1));

        client.label_root("docs", "My docs").unwrap();
        next("the new label", &|s| {
            s.as_ref().is_some_and(|s| s.roots[0].label == "My docs")
        });
        match client.add_root(&docs, None) {
            Err(ControlError::Refused(message)) => {
                assert!(message.contains("already shared"), "{message}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }

        let deny = client.deny().unwrap();
        assert!(deny.builtin.iter().any(|p| p == ".ssh"));
        client.set_paused(true).unwrap();
        next("paused", &|s| s.as_ref().is_some_and(|s| s.paused));
        let log = client.log(5).unwrap();
        assert!(log.iter().any(|e| e.event == "paused"), "{log:?}");
        assert!(client.status().unwrap().paused);

        client.remove_root("docs").unwrap();
        next("no folders", &|s| {
            s.as_ref().is_some_and(|s| s.roots.is_empty())
        });

        shutdown.cancel();
        runtime.block_on(daemon).unwrap().unwrap();
        assert_eq!(next("the daemon gone", &|s| s.is_none()), None);
    }

    #[test]
    fn a_missing_socket_means_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        let client = Client::new(crate::paths::Paths::under(tmp.path()).socket_path());
        assert!(matches!(client.status(), Err(ControlError::NotRunning)));
    }
}
