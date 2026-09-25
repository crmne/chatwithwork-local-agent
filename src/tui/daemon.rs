//! The TUI's side of the control channel (CONTROL.md).
//!
//! Two threads, both blocked until something happens:
//!
//! - The watcher subscribes to `status` and `audit` events and forwards them.
//!   When no daemon is listening, it reads `config.toml` and the audit file
//!   instead, and looks for the daemon again every few seconds (or at once,
//!   when asked).
//! - The worker runs commands (pause, share, stop sharing). Without a daemon
//!   it edits `config.toml`, as `cww roots add` and `cww pause` do.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::app::{
    DaemonCommand, DaemonMsg, DaemonStatus, Msg, Offline, PairingMsg, RootState, Suggestion,
};
use crate::audit::{self, AuditEntry};
use crate::auth::{LoginOptions, start_login};
use crate::config::Config;
use crate::control::{self, Client, ControlRequest, Topic};
use crate::paths::Paths;
use crate::roots::{NewRoot, add_root, remove_root};

/// Audit entries loaded as history.
const HISTORY: usize = 200;

/// How often to look for a daemon that isn't running.
const RETRY: Duration = Duration::from_secs(5);

pub struct Link {
    commands: Sender<DaemonCommand>,
    wake: Sender<()>,
}

impl Link {
    pub fn spawn(paths: Paths, tx: Sender<Msg>) -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (wake, wake_rx) = mpsc::channel();
        {
            let (paths, tx) = (paths.clone(), tx.clone());
            thread::spawn(move || watch(&paths, &tx, &wake_rx));
        }
        {
            let wake = wake.clone();
            thread::spawn(move || work(&paths, &tx, &command_rx, &wake));
        }
        Self { commands, wake }
    }

    pub fn send(&self, command: DaemonCommand) {
        let _ = match command {
            DaemonCommand::Retry => self.wake.send(()),
            other => self.commands.send(other).map_err(|_| mpsc::SendError(())),
        };
    }
}

fn send(tx: &Sender<Msg>, msg: DaemonMsg) -> bool {
    tx.send(Msg::Daemon(msg)).is_ok()
}

fn watch(paths: &Paths, tx: &Sender<Msg>, wake: &Receiver<()>) {
    let socket = paths.socket_path();
    // What the offline view was last built from, to skip rereading.
    let mut shown: Option<Stamp> = None;
    loop {
        match Client::connect(&socket) {
            Ok(Some(client)) => {
                shown = None;
                if !follow(client, paths, tx) || !send(tx, DaemonMsg::Lost) {
                    return;
                }
                // Look again soon: the daemon may be restarting.
                if wake.recv_timeout(Duration::from_millis(300))
                    == Err(RecvTimeoutError::Disconnected)
                {
                    return;
                }
                continue;
            }
            Ok(None) => {
                let stamp = Stamp::of(paths);
                if shown.as_ref() != Some(&stamp) && !send(tx, offline(paths, None)) {
                    return;
                }
                shown = Some(stamp);
            }
            Err(e) => {
                if !send(tx, offline(paths, Some(format!("{e:#}")))) {
                    return;
                }
                shown = None;
            }
        }
        match wake.recv_timeout(RETRY) {
            Ok(()) => shown = None,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Forward events until the daemon closes the subscription. False when the
/// UI has gone away.
fn follow(client: Client, paths: &Paths, tx: &Sender<Msg>) -> bool {
    let socket = &paths.socket_path();
    let events = match client.subscribe(&[Topic::Audit, Topic::Status]) {
        Ok(events) => events,
        Err(e) => return send(tx, offline(paths, Some(format!("{e:#}")))),
    };
    // Subscribed first, so nothing falls between the history and the events.
    if !catch_up(socket, tx) {
        return false;
    }
    for event in events {
        let Ok(event) = event else { break };
        let msg = match event["event"].as_str() {
            Some("status") => serde_json::from_value::<DaemonStatus>(event["status"].clone())
                .ok()
                .map(|s| DaemonMsg::Status(Box::new(s))),
            Some("audit") => serde_json::from_value::<AuditEntry>(event["entry"].clone())
                .ok()
                .map(|e| DaemonMsg::Audit(Box::new(e))),
            Some("lagged") => {
                if !catch_up(socket, tx) {
                    return false;
                }
                None
            }
            _ => None,
        };
        if let Some(msg) = msg
            && !send(tx, msg)
        {
            return false;
        }
    }
    true
}

/// Status, audit history and suggestions, on a second connection.
fn catch_up(socket: &Path, tx: &Sender<Msg>) -> bool {
    let Ok(Some(mut client)) = Client::connect(socket) else {
        return true;
    };
    let mut msgs = Vec::new();
    if let Ok(v) = client.call(&ControlRequest::Status)
        && let Ok(status) = serde_json::from_value::<DaemonStatus>(v)
    {
        msgs.push(DaemonMsg::Status(Box::new(status)));
    }
    if let Ok(v) = client.call(&ControlRequest::AuditTail {
        lines: Some(HISTORY),
    }) && let Ok(entries) = serde_json::from_value::<Vec<AuditEntry>>(v["entries"].clone())
    {
        msgs.push(DaemonMsg::AuditHistory(entries));
    }
    if let Ok(v) = client.call(&ControlRequest::SuggestedRoots)
        && let Ok(s) = serde_json::from_value::<Vec<Suggestion>>(v["suggestions"].clone())
    {
        msgs.push(DaemonMsg::Suggestions(s));
    }
    msgs.into_iter().all(|m| send(tx, m))
}

/// Modification times of the files the offline view is built from.
#[derive(PartialEq, Eq)]
struct Stamp(Option<SystemTime>, Option<SystemTime>);

impl Stamp {
    fn of(paths: &Paths) -> Self {
        let mtime = |p: PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        Self(mtime(paths.config_file()), mtime(paths.audit_file()))
    }
}

/// What the files say while the daemon isn't running.
fn offline(paths: &Paths, error: Option<String>) -> DaemonMsg {
    let (config, config_error) = match Config::load(paths) {
        Ok(config) => (config, None),
        Err(e) => (Config::default(), Some(format!("{e:#}"))),
    };
    let roots = config
        .roots
        .iter()
        .map(|r| RootState {
            id: r.id.clone(),
            label: r.label.clone(),
            available: r.path.is_dir(),
            index: String::new(),
            indexed_files: 0,
            local_path: r.path.display().to_string(),
        })
        .collect();
    let suggestion = crate::paths::documents_dir().map(|path| Suggestion {
        exists: path.is_dir(),
        shared: config.roots.iter().any(|r| r.path == path),
        path: path.display().to_string(),
        label: "Documents".into(),
    });
    DaemonMsg::NotRunning {
        offline: Box::new(Offline {
            paired: config.server.is_some(),
            server: config.server.as_ref().map(|s| s.url.clone()),
            device_id: config.server.as_ref().map(|s| s.device_id.clone()),
            paused: config.paused,
            roots,
            error: error.or(config_error),
        }),
        audit: audit::tail(&paths.audit_file(), HISTORY).unwrap_or_default(),
        suggestion,
    }
}

fn work(paths: &Paths, tx: &Sender<Msg>, commands: &Receiver<DaemonCommand>, wake: &Sender<()>) {
    // The pairing in progress, if any: set to cancel it.
    let mut pairing: Option<Arc<AtomicBool>> = None;
    for command in commands {
        match command {
            DaemonCommand::Pair => {
                if let Some(cancel) = pairing.take() {
                    cancel.store(true, Ordering::SeqCst);
                }
                let cancel = Arc::new(AtomicBool::new(false));
                pairing = Some(Arc::clone(&cancel));
                let (paths, tx, wake) = (paths.clone(), tx.clone(), wake.clone());
                // Waiting for approval takes minutes; don't hold up the rest.
                thread::spawn(move || pair(&paths, &tx, &cancel, &wake));
                continue;
            }
            DaemonCommand::CancelPairing => {
                if let Some(cancel) = pairing.take() {
                    cancel.store(true, Ordering::SeqCst);
                }
                continue;
            }
            _ => {}
        }
        let result = run(paths, &command);
        let offline = matches!(result, Ok((_, true)));
        let result = result.map(|(text, _)| text).map_err(|e| format!("{e:#}"));
        if !send(tx, DaemonMsg::Done { command, result }) {
            return;
        }
        if offline {
            // Rebuild the offline view from the edited file.
            let _ = wake.send(());
        }
    }
}

/// Run one command. The flag is true when it edited `config.toml` because
/// no daemon was running.
fn run(paths: &Paths, command: &DaemonCommand) -> Result<(String, bool)> {
    let socket = paths.socket_path();
    match command {
        DaemonCommand::Pause | DaemonCommand::Resume => {
            let paused = *command == DaemonCommand::Pause;
            let text = if paused {
                "Paused: requests from Chat with Work are refused."
            } else {
                "Resumed: requests from Chat with Work are answered again."
            };
            let request = if paused {
                ControlRequest::Pause
            } else {
                ControlRequest::Resume
            };
            if control::request(&socket, request)?.is_some() {
                return Ok((text.into(), false));
            }
            let mut config = Config::load(paths)?;
            config.paused = paused;
            config.save(paths)?;
            Ok((format!("{text} Saved for when the daemon starts."), true))
        }
        DaemonCommand::AddRoot {
            path,
            label,
            i_know,
        } => {
            let path = absolute(path)?;
            let request = ControlRequest::RootsAdd {
                path: path.clone(),
                label: label.clone(),
                follow_symlinks: false,
                i_know: *i_know,
            };
            if let Some(v) = control::request(&socket, request)? {
                let root = &v["root"];
                return Ok((
                    format!(
                        "Sharing {} as {}.",
                        root["path"].as_str().unwrap_or_default(),
                        root["id"].as_str().unwrap_or_default()
                    ),
                    false,
                ));
            }
            let mut config = Config::load(paths)?;
            let root = add_root(
                &mut config,
                paths,
                NewRoot {
                    path: &path,
                    label: label.clone(),
                    i_know: *i_know,
                    follow_symlinks: false,
                },
            )?;
            config.save(paths)?;
            Ok((
                format!(
                    "Sharing {} as {}. Saved for when the daemon starts.",
                    root.path.display(),
                    root.id
                ),
                true,
            ))
        }
        DaemonCommand::RemoveRoot { id } => {
            let request = ControlRequest::RootsRemove { root: id.clone() };
            if let Some(v) = control::request(&socket, request)? {
                let root = &v["root"];
                return Ok((
                    format!(
                        "Stopped sharing {}.",
                        root["path"].as_str().unwrap_or(id.as_str())
                    ),
                    false,
                ));
            }
            let mut config = Config::load(paths)?;
            let root = remove_root(&mut config, id)?;
            config.save(paths)?;
            Ok((format!("Stopped sharing {}.", root.path.display()), true))
        }
        DaemonCommand::InstallService => {
            let exe = std::env::current_exe().context("finding the cww binary")?;
            let output = std::process::Command::new(exe)
                .args(["daemon", "install"])
                .stdin(std::process::Stdio::null())
                .output()
                .context("running cww daemon install")?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let reason = stderr.trim().lines().last().unwrap_or("it failed");
                anyhow::bail!(
                    "Couldn't start the daemon: {}",
                    reason.trim_start_matches("error: ")
                );
            }
            Ok((
                "Started the daemon. It starts again whenever you log in.".into(),
                true,
            ))
        }
        DaemonCommand::Retry | DaemonCommand::Pair | DaemonCommand::CancelPairing => {
            Ok((String::new(), false))
        }
    }
}

/// Pair this computer: get a code, open the approval page, wait.
fn pair(paths: &Paths, tx: &Sender<Msg>, cancel: &AtomicBool, wake: &Sender<()>) {
    let finish = |result: Result<String, String>| {
        send(tx, DaemonMsg::Pairing(PairingMsg::Finished(result)));
        let _ = wake.send(());
    };
    // Pair again with the server this computer was paired with, if any.
    let server = Config::load(paths)
        .ok()
        .and_then(|c| c.server.map(|s| s.url))
        .or_else(|| std::env::var("CWW_SERVER").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| crate::config::DEFAULT_SERVER.to_string());
    let pending = match start_login(paths, &server, LoginOptions::default()) {
        Ok(pending) => pending,
        Err(e) => return finish(Err(format!("{e:#}"))),
    };
    let shown = pending
        .verification_uri_complete()
        .unwrap_or(pending.verification_uri())
        .to_string();
    let opened = pending.browser_url().is_some_and(open_in_browser);
    let code = PairingMsg::Code {
        code: pending.user_code().to_string(),
        url: shown,
        name: pending.device_name().to_string(),
        fingerprint: pending.fingerprint(),
        opened,
    };
    if !send(tx, DaemonMsg::Pairing(code)) {
        return;
    }
    let result = pending.wait(cancel).map(|server| server.url);
    if result.is_ok() {
        // A running daemon picks up the pairing and connects.
        let _ = control::request(&paths.socket_path(), ControlRequest::Reload);
    }
    finish(result.map_err(|e| format!("{e:#}")));
}

/// Open `url` in the default browser. Only plain http(s) URLs with
/// unremarkable characters are handed to the system.
fn open_in_browser(url: &str) -> bool {
    let plain = (url.starts_with("https://") || url.starts_with("http://"))
        && url
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ":/?=._~%-".contains(c));
    if !plain {
        return false;
    }
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// The daemon wants an absolute path; people type `~/Work` or `Work`.
fn absolute(input: &str) -> Result<PathBuf> {
    let expanded = match input.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            crate::paths::home_dir()?.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(input),
    };
    std::path::absolute(&expanded).with_context(|| format!("resolving {input}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_view_comes_from_the_config_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());
        let shared = tmp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();

        let (_, edited_file) = run(
            &paths,
            &DaemonCommand::AddRoot {
                path: shared.display().to_string(),
                label: Some("Shared".into()),
                i_know: false,
            },
        )
        .unwrap();
        assert!(edited_file, "no daemon, so the file was edited");
        run(&paths, &DaemonCommand::Pause).unwrap();

        let DaemonMsg::NotRunning { offline, .. } = offline(&paths, None) else {
            panic!("expected the offline view");
        };
        assert!(offline.paused);
        assert!(!offline.paired);
        assert_eq!(offline.roots.len(), 1);
        assert_eq!(offline.roots[0].label, "Shared");
        assert!(offline.roots[0].available);

        let id = offline.roots[0].id.clone();
        run(&paths, &DaemonCommand::RemoveRoot { id }).unwrap();
        assert!(Config::load(&paths).unwrap().roots.is_empty());
    }

    #[test]
    fn expands_home_and_relative_paths() {
        let home = crate::paths::home_dir().unwrap();
        assert_eq!(absolute("~").unwrap(), home);
        assert_eq!(absolute("~/Work").unwrap(), home.join("Work"));
        assert!(absolute("Work").unwrap().is_absolute());
    }
}
