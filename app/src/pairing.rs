//! Pairing, the way `cww login` does it. The app runs `cww login --json`
//! (CONTROL.md, "Pairing"), so the device key is created by the same binary
//! that reads it, and there is one pairing flow for the CLI, the terminal UI
//! and the app. Logging out runs `cww logout`.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde::Deserialize;

/// What to show while the user approves the computer.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Code {
    pub user_code: String,
    pub verification_uri: String,
    /// The approval page, if it is on the server being paired with.
    pub browser_url: Option<String>,
    pub device_name: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PairEvent {
    Code(Code),
    Paired,
    Failed(String),
}

pub type Report = Box<dyn Fn(PairEvent) + Send + 'static>;

/// Stops a pairing that is waiting.
pub type Cancel = Box<dyn FnOnce() + Send + 'static>;

pub trait Account: Send + Sync {
    /// Start pairing; `report` gets the code, then the outcome.
    fn pair(&self, server: Option<String>, report: Report) -> Cancel;
    /// Forget the pairing on this computer.
    fn logout(&self) -> Result<(), String>;
}

/// Pairs by running the `cww` command.
pub struct Cli;

fn cww() -> Result<std::path::PathBuf, String> {
    crate::agent::cww_binary().ok_or_else(|| {
        "Couldn't find the cww command. Install the Local Agent, then try again.".to_string()
    })
}

fn quiet(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up.
        command.creation_flags(0x0800_0000);
    }
    command
}

impl Account for Cli {
    fn pair(&self, server: Option<String>, report: Report) -> Cancel {
        let child: Arc<Mutex<Option<Child>>> = Arc::default();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (running, stop) = (Arc::clone(&child), Arc::clone(&cancelled));
        std::thread::spawn(move || {
            let cww = match cww() {
                Ok(cww) => cww,
                Err(e) => return report(PairEvent::Failed(e)),
            };
            let mut command = Command::new(cww);
            command.args(["login", "--json"]);
            if let Some(server) = &server {
                command.args(["--server", server]);
            }
            let spawned = quiet(&mut command)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let mut process = match spawned {
                Ok(p) => p,
                Err(e) => return report(PairEvent::Failed(format!("Couldn't run cww: {e}"))),
            };
            let stdout = process.stdout.take().expect("piped stdout");
            *running.lock().expect("pairing lock") = Some(process);
            let mut outcome = None;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                match event["event"].as_str() {
                    Some("code") => {
                        if let Ok(code) = serde_json::from_value(event) {
                            report(PairEvent::Code(code));
                        }
                    }
                    Some("paired") => outcome = Some(PairEvent::Paired),
                    Some("error") => {
                        let error = event["error"].as_str().unwrap_or("pairing failed");
                        outcome = Some(PairEvent::Failed(sentence(error)));
                    }
                    _ => {}
                }
            }
            if let Some(mut process) = running.lock().expect("pairing lock").take() {
                let _ = process.wait();
            }
            if stop.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            report(outcome.unwrap_or_else(|| PairEvent::Failed("Pairing stopped.".into())));
        });
        Box::new(move || {
            cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(process) = child.lock().expect("pairing lock").as_mut() {
                let _ = process.kill();
            }
        })
    }

    fn logout(&self) -> Result<(), String> {
        let out = quiet(Command::new(cww()?).arg("logout"))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("Couldn't run cww: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(sentence(stderr.trim().trim_start_matches("error: ")))
        }
    }
}

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
