//! A per-user fallback when Task Scheduler denies registration.
//!
//! A Startup shortcut supports long paths and arguments without the Run
//! registry key's 260-character command-line limit. Windows PowerShell and
//! WScript.Shell are built into Windows; no administrator rights are needed.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

const SCRIPT: &str = include_str!("windows_startup.ps1");

fn shortcut(command: Option<(&Path, &[String])>) -> Result<Option<PathBuf>> {
    let mut shell = Command::new("powershell.exe");
    shell.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        SCRIPT,
    ]);
    shell.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    if let Some((exe, arguments)) = command {
        shell
            .env("CWW_STARTUP_ACTION", "install")
            .env("CWW_STARTUP_EXE", exe)
            .env(
                "CWW_STARTUP_ARGS",
                super::windows_task::quote_arguments(arguments),
            );
    } else {
        shell.env("CWW_STARTUP_ACTION", "remove");
    }
    let output = shell
        .output()
        .context("setting up the Local Agent's startup shortcut")?;
    if !output.status.success() {
        bail!(
            "Could not update the Local Agent's startup shortcut: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let path = String::from_utf8(output.stdout).context("reading the startup shortcut path")?;
    Ok((!path.trim().is_empty()).then(|| PathBuf::from(path.trim())))
}

pub(super) fn install(exe: &Path, arguments: &[String]) -> Result<PathBuf> {
    shortcut(Some((exe, arguments)))?.context("Windows did not return the startup shortcut path")
}

pub(super) fn remove() -> Result<Option<PathBuf>> {
    shortcut(None)
}

pub(super) fn start(exe: &Path, arguments: &[String]) -> Result<()> {
    // Detach from the setup process, including its standard streams, so
    // closing the app or installer leaves the daemon running.
    Command::new(exe)
        .args(arguments)
        .creation_flags(0x0000_0008 | 0x0000_0200) // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting the Local Agent in the background")?;
    Ok(())
}
