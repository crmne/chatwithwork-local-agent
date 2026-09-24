//! `cww-agent`: the daemon, for the Windows logon task.
//!
//! On Windows this binary uses the GUI subsystem, so starting it at logon
//! opens no console window, and it runs exactly what `cww daemon run` runs.
//! Elsewhere it is the same as `cww daemon run`; `cww daemon install`
//! registers `cww` itself on Linux and macOS.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let mut log_file: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--log-file") => log_file = args.next().map(PathBuf::from),
            // The Windows task passes CWW_HOME this way.
            Some("--home") => {
                if let Some(home) = args.next() {
                    // SAFETY: single-threaded here, before anything reads the
                    // environment.
                    unsafe { std::env::set_var("CWW_HOME", home) };
                }
            }
            Some("--version") => {
                println!("cww-agent {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => {
                eprintln!("usage: cww-agent [--log-file PATH] [--home DIR]");
                std::process::exit(2);
            }
        }
    }
    let result = cww::paths::Paths::from_env()
        .and_then(|paths| cww::daemon::run_foreground(paths, log_file.as_deref()));
    if let Err(e) = result {
        // Without a console, the log file is the only place this can go.
        tracing::error!("cww-agent stopped: {e:#}");
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
