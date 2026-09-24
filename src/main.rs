use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::Value;

use cww::config::{Config, DEFAULT_SERVER};
use cww::control::{self, ControlRequest};
use cww::paths::Paths;
use cww::roots::{NewRoot, add_root, remove_root};
use cww::{audit, auth, daemon, service};

/// Chat with Work Local Agent.
///
/// Shares folders you choose with Chat with Work, read-only.
#[derive(Parser)]
#[command(name = "cww", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
enum DaemonCommand {
    /// Run in the foreground.
    Run,
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
    match cli.command {
        Command::Login { server, name } => {
            let options = auth::LoginOptions { name, store: None };
            let paired = auth::login(&paths, &server, options, |line| println!("{line}"))?;
            println!();
            println!("Paired with {} as device {}.", paired.url, paired.device_id);
            notify_daemon(
                &paths,
                "Start the daemon with `cww daemon install` (or `cww daemon run`).",
            );
            if Config::load(&paths)?.roots.is_empty() {
                println!("Nothing is shared yet. Share a folder with `cww roots add <path>`.");
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
        } => audit::print_log(&paths.audit_file(), lines, follow, json)?,
        Command::Daemon { command } => match command {
            DaemonCommand::Run => {
                init_logging();
                let runtime = tokio::runtime::Runtime::new()?;
                runtime.block_on(daemon::run(
                    paths,
                    tokio_util::sync::CancellationToken::new(),
                    true,
                ))?;
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

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("CWW_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
