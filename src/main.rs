use std::path::PathBuf;

// mimalloc returns freed memory to the system; see Cargo.toml.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::Value;

use cww::config::{Config, DEFAULT_SERVER};
use cww::control::{self, ControlRequest};
use cww::paths::Paths;
use cww::roots::{NewRoot, add_root, remove_root};
use cww::{audit, auth, daemon, service};

/// Chat with Work Local Agent.
///
/// Shares folders you choose with Chat with Work, read-only. Without a
/// command, opens the terminal UI.
#[derive(Parser)]
#[command(name = "cww", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Pair this computer with Chat with Work (OAuth device flow).
    Login {
        /// Server origin, for self-hosted installs.
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
        /// Name shown in Settings (defaults to the hostname).
        #[arg(long)]
        name: Option<String>,
        /// Print the approval link without opening a browser.
        #[arg(long)]
        no_browser: bool,
        /// Print JSON lines for a program to read (the desktop app): the
        /// code, then the outcome. Never asks anything.
        #[arg(long)]
        json: bool,
    },
    /// Forget the pairing on this computer. Revoke it in Settings too.
    Logout,
    /// Manage the shared folders.
    Roots {
        #[command(subcommand)]
        command: RootsCommand,
    },
    /// Show the daemon's connection and index state.
    Status {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop answering tool calls until `cww resume`.
    Pause,
    /// Answer tool calls again.
    Resume,
    /// Make the daemon re-read the config file after you edit it.
    Reload,
    /// Show the local audit log.
    Log {
        /// Keep printing new entries.
        #[arg(short, long)]
        follow: bool,
        /// Entries to show first.
        #[arg(short = 'n', long, default_value_t = 20)]
        lines: usize,
        /// Print raw JSON lines.
        #[arg(long)]
        json: bool,
    },
    /// Run or register the background daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Diagnostics.
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
    /// Open the terminal UI (the default when run in a terminal).
    Tui,
}

#[derive(Subcommand)]
enum RootsCommand {
    /// Share a folder.
    Add {
        path: PathBuf,
        /// Label shown to Chat with Work (defaults to the folder name).
        #[arg(long)]
        label: Option<String>,
        /// Allow `/`, your home folder, or system directories.
        #[arg(long)]
        i_know: bool,
        /// Follow symlinks that stay inside the folder.
        #[arg(long)]
        follow_symlinks: bool,
    },
    /// List shared folders.
    List,
    /// Stop sharing a folder (by ID, label or path).
    Remove { root: String },
}

#[derive(Subcommand)]
enum DebugCommand {
    /// Confine this process as the daemon would, then try to read FILES.
    Sandbox { files: Vec<PathBuf> },
    /// Confine this process for a keychain-held key, then write, read and
    /// delete a test secret in the OS keychain.
    Keychain,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Run in the foreground.
    Run {
        /// Write the daemon's own log to this file instead of stderr.
        #[arg(long)]
        log_file: Option<PathBuf>,
        /// Don't confine the daemon with Landlock or Seatbelt.
        #[arg(long)]
        no_sandbox: bool,
        /// Run as the confined worker of a supervising `cww daemon run`.
        #[arg(long, hide = true)]
        worker: bool,
    },
    /// Ask the running daemon to stop.
    Stop,
    /// Install and start the per-user service (systemd --user or LaunchAgent).
    Install {
        /// Leave out systemd sandboxing options.
        #[arg(long)]
        no_hardening: bool,
    },
    /// Stop and remove the service.
    Uninstall {
        /// Also delete keys, config, index and logs.
        #[arg(long)]
        purge: bool,
    },
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let paths = Paths::from_env()?;
    let Some(command) = cli.command else {
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() && std::io::stdin().is_terminal() {
            return cww::tui::run(paths);
        }
        // Piped into `head`, a broken pipe isn't worth an error.
        let _ = Cli::command().print_help();
        return Ok(());
    };
    match command {
        Command::Tui => cww::tui::run(paths)?,
        Command::Login {
            server,
            name,
            no_browser,
            json: true,
        } => login_json(&paths, &server, name, !no_browser)?,
        Command::Login {
            server,
            name,
            no_browser,
            json: false,
        } => {
            let options = auth::LoginOptions {
                name,
                store: None,
                open_browser: !no_browser,
            };
            let paired = auth::login(&paths, &server, options, |line| println!("{line}"))?;
            println!();
            println!("Paired with {} as device {}.", paired.url, paired.device_id);
            notify_daemon(
                &paths,
                "Start the daemon with `cww daemon install` (or `cww daemon run`).",
            );
            if Config::load(&paths)?.roots.is_empty() {
                offer_documents(&paths)?;
            }
        }
        Command::Logout => {
            if auth::logout(&paths)? {
                println!("Forgot the pairing. Revoke this computer in Settings as well.");
            } else {
                println!("This computer wasn't paired.");
            }
            notify_daemon(&paths, "");
        }
        Command::Roots { command } => roots(&paths, command)?,
        Command::Status { json } => status(&paths, json)?,
        Command::Pause => pause(&paths, true)?,
        Command::Resume => pause(&paths, false)?,
        Command::Reload => match control::request(&paths.socket_path(), ControlRequest::Reload)? {
            Some(_) => println!("The daemon reloaded {}.", paths.config_file().display()),
            None => println!("The daemon is not running; it reads the config when it starts."),
        },
        Command::Log {
            follow,
            lines,
            json,
        } => log(&paths, lines, follow, json)?,
        Command::Debug {
            command: DebugCommand::Keychain,
        } => {
            let status = daemon::confine_for_check(&paths, Some(true))?;
            println!("sandbox: {}", serde_json::to_string(&status)?);
            let store = auth::secrets::SecretStore::Keyring;
            let name = "sandbox-probe";
            let result = store
                .set(name, "ok")
                .and_then(|()| store.get(name))
                .and_then(|value| store.delete(name).map(|()| value));
            match result {
                Ok(Some(value)) if value == "ok" => {
                    println!("keychain: write, read and delete work")
                }
                Ok(other) => println!("keychain: read back {other:?}"),
                Err(e) => println!("keychain: {e:#}"),
            }
        }
        Command::Debug {
            command: DebugCommand::Sandbox { files },
        } => {
            let status = daemon::confine_for_check(&paths, None)?;
            println!("sandbox: {}", serde_json::to_string(&status)?);
            for file in files {
                match std::fs::read(&file) {
                    Ok(_) => println!("read {}", file.display()),
                    Err(e) => println!("refused {} ({e})", file.display()),
                }
            }
        }
        Command::Daemon { command } => match command {
            DaemonCommand::Run {
                log_file,
                no_sandbox,
                worker,
            } => daemon::run_foreground(
                paths,
                &daemon::Foreground {
                    log_file,
                    sandbox: !no_sandbox,
                    worker,
                },
            )?,
            DaemonCommand::Stop => {
                match control::request(&paths.socket_path(), ControlRequest::Shutdown)? {
                    Some(_) => println!("The daemon is stopping."),
                    None => println!("The daemon is not running."),
                }
            }
            DaemonCommand::Install { no_hardening } => {
                let file = service::install(&paths, &service::InstallOptions { no_hardening })?;
                println!("Installed and started {}.", file.display());
            }
            DaemonCommand::Uninstall { purge } => {
                for path in service::uninstall(&paths, purge)? {
                    println!("Removed {}", path.display());
                }
                if !purge {
                    println!("Keys, config, index and logs were kept; use --purge to delete them.");
                }
            }
        },
    }
    Ok(())
}

fn roots(paths: &Paths, command: RootsCommand) -> Result<()> {
    let mut config = Config::load(paths)?;
    match command {
        RootsCommand::Add {
            path,
            label,
            i_know,
            follow_symlinks,
        } => {
            let root = add_root(
                &mut config,
                paths,
                NewRoot {
                    path: &path,
                    label,
                    i_know,
                    follow_symlinks,
                },
            )?;
            config.save(paths)?;
            println!(
                "Sharing {} as {} ({}).",
                root.path.display(),
                root.id,
                root.label
            );
            notify_daemon(paths, "");
        }
        RootsCommand::List => {
            if config.roots.is_empty() {
                println!("Nothing is shared. Add a folder with `cww roots add <path>`.");
            }
            for root in &config.roots {
                println!("{:<24} {:<24} {}", root.id, root.label, root.path.display());
            }
        }
        RootsCommand::Remove { root } => {
            let removed = remove_root(&mut config, &root)?;
            config.save(paths)?;
            println!(
                "Stopped sharing {} ({}).",
                removed.path.display(),
                removed.id
            );
            notify_daemon(paths, "");
        }
    }
    Ok(())
}

fn status(paths: &Paths, json: bool) -> Result<()> {
    let Some(status) = control::request(&paths.socket_path(), ControlRequest::Status)? else {
        let config = Config::load(paths)?;
        if json {
            println!(
                "{}",
                serde_json::json!({ "running": false, "paired": config.server.is_some() })
            );
        } else {
            println!(
                "The daemon is not running. Start it with `cww daemon install` or `cww daemon run`."
            );
            match &config.server {
                Some(s) => println!("Paired with {} as device {}.", s.url, s.device_id),
                None => println!("Not paired. Run `cww login`."),
            }
        }
        return Ok(());
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    let conn = &status["connection"];
    println!(
        "Daemon:     running (pid {}, cww {})",
        status["pid"],
        str(&status["version"])
    );
    println!(
        "Server:     {}",
        status["server"].as_str().unwrap_or("not paired")
    );
    println!(
        "Connection: {} since {}{}",
        str(&conn["connection"]).replace('_', " "),
        str(&conn["since"]),
        conn["last_error"]
            .as_str()
            .map(|e| format!(" ({e})"))
            .unwrap_or_default()
    );
    println!(
        "Tool calls: {}",
        if status["paused"] == Value::Bool(true) {
            "PAUSED"
        } else {
            "answered"
        }
    );
    println!("Roots:");
    let roots = status["roots"].as_array().cloned().unwrap_or_default();
    if roots.is_empty() {
        println!("  none; add one with `cww roots add <path>`");
    }
    for r in roots {
        println!(
            "  {:<20} {:<20} index {} ({} files){}  {}",
            str(&r["id"]),
            str(&r["label"]),
            str(&r["index"]),
            r["indexed_files"].as_u64().unwrap_or(0),
            if r["available"] == Value::Bool(false) {
                " UNAVAILABLE"
            } else {
                ""
            },
            str(&r["local_path"]),
        );
    }
    Ok(())
}

fn pause(paths: &Paths, paused: bool) -> Result<()> {
    let request = if paused {
        ControlRequest::Pause
    } else {
        ControlRequest::Resume
    };
    if control::request(&paths.socket_path(), request)?.is_none() {
        let mut config = Config::load(paths)?;
        config.paused = paused;
        config.save(paths)?;
        println!("The daemon is not running; saved for its next start.");
    }
    println!(
        "{}",
        if paused {
            "Paused: tool calls from Chat with Work are refused."
        } else {
            "Resumed: tool calls are answered again."
        }
    );
    Ok(())
}

/// Ask a running daemon to re-read the config.
/// `cww login --json`: one JSON object per line on stdout. First
/// `{"event":"code",...}` with what to show and open, then `{"event":"paired",...}`
/// or `{"event":"error",...}`. The daemon is told to reload on success.
fn login_json(paths: &Paths, server: &str, name: Option<String>, open: bool) -> Result<()> {
    use std::sync::atomic::AtomicBool;

    let emit = |value: Value| println!("{value}");
    let options = auth::LoginOptions {
        name,
        store: None,
        open_browser: false,
    };
    let result = auth::start_login(paths, server, options).and_then(|pending| {
        let opened = open && pending.browser_url().is_some_and(cww::browser::open);
        emit(serde_json::json!({
            "event": "code",
            "user_code": pending.user_code(),
            "verification_uri": pending.verification_uri(),
            "verification_uri_complete": pending.verification_uri_complete(),
            "browser_url": pending.browser_url(),
            "device_name": pending.device_name(),
            "fingerprint": pending.fingerprint(),
            "opened": opened,
        }));
        pending.wait(&AtomicBool::new(false))
    });
    match result {
        Ok(paired) => {
            let _ = control::request(&paths.socket_path(), ControlRequest::Reload);
            emit(serde_json::json!({
                "event": "paired",
                "server": paired.url,
                "device_id": paired.device_id,
            }));
            Ok(())
        }
        Err(e) => {
            emit(serde_json::json!({ "event": "error", "error": format!("{e:#}") }));
            std::process::exit(1);
        }
    }
}

fn notify_daemon(paths: &Paths, if_not_running: &str) {
    match control::request(&paths.socket_path(), ControlRequest::Reload) {
        Ok(Some(_)) => println!("The daemon picked up the change."),
        Ok(None) if !if_not_running.is_empty() => println!("{if_not_running}"),
        Ok(None) => {}
        Err(e) => eprintln!("warning: couldn't reach the daemon: {e:#}"),
    }
}

fn str(v: &Value) -> &str {
    v.as_str().unwrap_or("?")
}

/// Print the audit log. With `follow`, new entries come from the running
/// daemon as they happen; without a daemon, the file is tailed instead.
fn log(paths: &Paths, lines: usize, follow: bool, raw: bool) -> Result<()> {
    audit::print_log(&paths.audit_file(), lines, false, raw)?;
    if !follow {
        return Ok(());
    }
    let Some(client) = control::Client::connect(&paths.socket_path())? else {
        return audit::print_log(&paths.audit_file(), 0, true, raw);
    };
    for event in client.subscribe(&[control::Topic::Audit])? {
        let event = event?;
        if event["event"] != "audit" {
            continue;
        }
        let line = serde_json::to_string(&event["entry"])?;
        if raw {
            println!("{line}");
        } else {
            println!("{}", audit::format_line(&line));
        }
    }
    eprintln!("The daemon stopped.");
    Ok(())
}

/// After pairing, offer to share the Documents folder. Nothing is shared
/// without an explicit yes; without a terminal to ask on, nothing happens.
fn offer_documents(paths: &Paths) -> Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    let hint = "Share a folder with `cww roots add <path>`.";
    let Some(documents) = cww::paths::documents_dir().filter(|d| d.is_dir()) else {
        println!("Nothing is shared yet. {hint}");
        return Ok(());
    };
    if !std::io::stdin().is_terminal() {
        println!("Nothing is shared yet. {hint}");
        return Ok(());
    }
    println!();
    println!("Nothing is shared yet. Chat with Work can search your Documents folder:");
    println!("  {}", documents.display());
    println!("Secrets inside it (keys, .env files, password databases) stay private either way.");
    print!("Share it? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Not shared. {hint}");
        return Ok(());
    }
    roots(
        paths,
        RootsCommand::Add {
            path: documents,
            label: Some("Documents".into()),
            i_know: false,
            follow_symlinks: false,
        },
    )
}
