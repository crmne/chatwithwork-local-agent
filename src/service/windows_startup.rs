//! A per-user fallback when Task Scheduler denies registration.
//!
//! A Startup shortcut supports long paths and arguments without the Run
//! registry key's 260-character command-line limit. Windows PowerShell and
//! WScript.Shell are built into Windows; no administrator rights are needed.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION, STARTUPINFOW,
};

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
    // Command::spawn inherits every inheritable Windows handle, even when
    // its standard streams are redirected to NUL. An MSI's output pipe can
    // then remain open in the daemon and keep WixQuietExec waiting forever.
    // CreateProcessW with inheritance disabled leaves all installer handles
    // behind. A detached daemon has no console or standard streams and logs
    // to the --log-file argument supplied by the caller.
    let mut executable: Vec<u16> = exe.as_os_str().encode_wide().collect();
    let mut command = vec![b'"' as u16];
    command.extend_from_slice(&executable);
    command.extend([b'"' as u16, b' ' as u16]);
    command.extend(super::windows_task::quote_arguments(arguments).encode_utf16());
    if executable.contains(&0) || command.contains(&0) {
        bail!("The Local Agent's command contains a null character");
    }
    executable.push(0);
    command.push(0);
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: both strings are terminated, command is writable, and the
    // structures remain live for the call. Null environment and directory
    // pointers keep the current user's environment and working directory.
    let created = unsafe {
        CreateProcessW(
            executable.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        )
    };
    if created == 0 {
        return Err(std::io::Error::last_os_error())
            .context("starting the Local Agent in the background");
    }
    // SAFETY: successful creation returns these two owned handles. Closing
    // them does not terminate the background process.
    unsafe {
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    Ok(())
}
