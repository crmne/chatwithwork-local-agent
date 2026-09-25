//! Chat with Work Local Agent: the menu bar app and settings window for the
//! `cww` daemon. It shows the daemon's state, pauses and resumes it, and
//! manages shared folders and pairing, all over the daemon's control socket
//! (CONTROL.md).
//!
//! While the window is closed the app is a tray item and three threads
//! blocked on I/O (the tray, the daemon's status, other launches), so it
//! uses no CPU until something happens.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod agent;
mod autostart;
mod control;
#[cfg(any(test, feature = "demo"))]
mod demo;
mod events;
mod icons;
mod instance;
mod model;
mod paths;
mod platform;
mod screenshot;
mod shell;
mod time;
mod tray;
mod ui;

use std::path::PathBuf;

use ui::Page;

const USAGE: &str = "\
Chat with Work Local Agent

Usage: cww-app [options]

Options:
  --background         Start in the tray without opening the settings window
                       (how the app starts at login)
  --page <name>        Open the settings window on a page: folders, privacy,
                       activity, account, general, welcome
  --screenshot <file>  Save a PNG of the settings window and quit
  --version            Print the version
  --help               Print this help

The cww command runs the agent itself; see `cww --help`.";

fn main() {
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("CWW_APP_LOG", "warn")).init();
    match parse(std::env::args().skip(1)) {
        Ok(Some(options)) => {
            if let Err(e) = run(options) {
                log::error!("{e:#}");
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
        Ok(None) => {}
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

struct Args {
    shell: shell::Options,
    /// `--demo` (a paired sample) or `--demo-fresh` (a first run).
    #[cfg_attr(not(feature = "demo"), allow(dead_code))]
    demo: Option<bool>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut options = shell::Options {
        open_settings: true,
        background: false,
        page: None,
        screenshot: None,
    };
    let mut demo = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--background" => {
                options.background = true;
                options.open_settings = false;
            }
            "--page" => {
                let name = args.next().ok_or("--page needs a name")?;
                options.page = Some(page(&name).ok_or_else(|| format!("unknown page {name:?}"))?);
            }
            "--screenshot" => {
                options.screenshot = Some(PathBuf::from(
                    args.next().ok_or("--screenshot needs a file")?,
                ));
            }
            "--demo" if cfg!(feature = "demo") => demo = Some(false),
            "--demo-fresh" if cfg!(feature = "demo") => demo = Some(true),
            "--version" | "-V" => {
                println!("cww-app {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(None);
            }
            // macOS passes this when an app is opened from Finder.
            other if other.starts_with("-psn_") => {}
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(Some(Args {
        shell: options,
        demo,
    }))
}

fn page(name: &str) -> Option<Page> {
    Some(match name {
        "welcome" => Page::Welcome,
        "folders" => Page::Folders,
        "privacy" => Page::Privacy,
        "activity" => Page::Activity,
        "account" => Page::Account,
        "general" => Page::General,
        _ => return None,
    })
}

fn run(args: Args) -> anyhow::Result<()> {
    #[cfg(feature = "demo")]
    if let Some(fresh) = args.demo {
        let scenario = if fresh {
            demo::Scenario::Fresh
        } else {
            demo::Scenario::Sample
        };
        let server = demo::start(scenario)?;
        return shell::run(server.paths.clone(), args.shell);
    }
    let paths = paths::Paths::from_env()?;
    shell::run(paths, args.shell)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Option<Args>, String> {
        parse(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_options() {
        let plain = args(&[]).unwrap().unwrap();
        assert!(plain.shell.open_settings && !plain.shell.background);
        let login = args(&["--background"]).unwrap().unwrap();
        assert!(!login.shell.open_settings && login.shell.background);
        let page = args(&["--page", "activity"]).unwrap().unwrap();
        assert_eq!(page.shell.page, Some(Page::Activity));
        assert!(args(&["--page", "nope"]).is_err());
        assert!(args(&["--frobnicate"]).is_err());
        assert!(args(&["-psn_0_12345"]).unwrap().is_some());
        assert!(args(&["--version"]).unwrap().is_none());
    }
}
