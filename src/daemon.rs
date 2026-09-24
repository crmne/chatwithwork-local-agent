//! `cww daemon run`: the reader, the control socket and the tunnel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, broadcast};
use tokio_util::sync::CancellationToken;

use crate::audit::{AuditEntry, AuditLog};
use crate::auth::load_credentials;
use crate::config::Config;
use crate::control::{self, ControlEvent, ControlHandler, ControlRequest, PROTOCOL_VERSION};
use crate::limits::Limiter;
use crate::paths::Paths;
use crate::reader::Reader;
use crate::roots::{NewRoot, add_root, remove_root};
use crate::status::{Changes, Connection, SharedStatus};
use crate::tools::LocalFiles;
use crate::tunnel::{Tunnel, TunnelExit, TunnelSettings};

struct Daemon {
    paths: Paths,
    config: Mutex<Config>,
    reader: Arc<Reader>,
    limiter: Arc<Limiter>,
    audit: Arc<AuditLog>,
    paused: Arc<AtomicBool>,
    status: SharedStatus,
    /// Cancelled to restart the tunnel with new credentials.
    tunnel_token: Mutex<CancellationToken>,
    /// Wakes the supervisor after a reload.
    reloaded: Notify,
    /// Events for control-channel subscribers.
    events: broadcast::Sender<ControlEvent>,
    /// Bumped whenever something in `status` changes.
    changes: Changes,
    /// Cancelled by a `shutdown` request.
    shutdown: CancellationToken,
}

/// `cww daemon run` (and `cww-agent` on Windows): set up logging, then run
/// the daemon on a fresh runtime until a signal or a `shutdown` request.
pub fn run_foreground(paths: Paths, log_file: Option<&std::path::Path>) -> Result<()> {
    crate::logging::init(log_file)?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(run(paths, CancellationToken::new(), true))
}

/// Run the daemon until `shutdown` is cancelled (or SIGINT/SIGTERM when
/// `handle_signals` is set).
pub async fn run(paths: Paths, shutdown: CancellationToken, handle_signals: bool) -> Result<()> {
    #[cfg(unix)]
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    paths.ensure()?;
    let config = Config::load(&paths)?;
    let (events, _) = broadcast::channel(256);
    let audit = Arc::new(AuditLog::open(&paths.audit_file())?.with_events(events.clone()));
    // Bind first: a second daemon must fail before it touches the index.
    let listener = control::bind(&paths.socket_path())?;

    let changes = Changes::default();
    let reader = {
        let (config, paths, changes) = (config.clone(), paths.clone(), changes.clone());
        tokio::task::spawn_blocking(move || Reader::new(&config, &paths, changes))
            .await
            .context("starting the reader")??
    };
    let daemon = Arc::new(Daemon {
        limiter: Arc::new(Limiter::new(config.limits.clone())),
        paused: Arc::new(AtomicBool::new(config.paused)),
        config: Mutex::new(config),
        reader: Arc::new(reader),
        audit,
        status: SharedStatus::new(changes.clone()),
        tunnel_token: Mutex::new(shutdown.child_token()),
        reloaded: Notify::new(),
        events,
        changes,
        shutdown: shutdown.clone(),
        paths,
    });
    tokio::spawn(Arc::clone(&daemon).publish_status(shutdown.clone()));

    if handle_signals {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            wait_for_signal().await;
            shutdown.cancel();
        });
    }
    tokio::spawn(control::serve(
        listener,
        Arc::clone(&daemon),
        shutdown.clone(),
    ));

    let mut entry = AuditEntry::event("started");
    entry.detail = Some(format!("cww {}", env!("CARGO_PKG_VERSION")));
    daemon.audit.append(&entry);

    daemon.supervise(&shutdown).await;

    daemon.audit.append(&AuditEntry::event("stopped"));
    Ok(())
}

impl Daemon {
    /// Push a fresh status to subscribers whenever something changes. Sleeps
    /// until then; bursts of changes are coalesced.
    async fn publish_status(self: Arc<Self>, shutdown: CancellationToken) {
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = self.changes.wait() => {}
            }
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(Duration::from_millis(150)) => {}
            }
            if self.events.receiver_count() > 0 {
                let status = self.status_json().await;
                let _ = self.events.send(ControlEvent::Status { status });
            }
        }
    }

    /// Keep a tunnel running whenever the computer is paired.
    async fn supervise(self: &Arc<Self>, shutdown: &CancellationToken) {
        while !shutdown.is_cancelled() {
            let config = self.config.lock().await.clone();
            let paths = self.paths.clone();
            let credentials =
                tokio::task::spawn_blocking(move || load_credentials(&config, &paths)).await;
            let credentials = match credentials {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    tracing::error!("can't load credentials: {e:#}");
                    self.status
                        .set(Connection::NotPaired, Some(format!("{e:#}")));
                    None
                }
                Err(e) => {
                    tracing::error!("credential task failed: {e}");
                    None
                }
            };
            let Some(credentials) = credentials else {
                if self.status.get().connection != Connection::NotPaired {
                    self.status.set(Connection::NotPaired, None);
                }
                self.wait_for_reload(shutdown).await;
                continue;
            };

            let token = shutdown.child_token();
            *self.tunnel_token.lock().await = token.clone();
            let max_message_bytes = self.config.lock().await.limits.max_message_bytes;
            let files = LocalFiles::new(
                Arc::clone(&self.reader),
                Arc::clone(&self.limiter),
                Arc::clone(&self.audit),
                Arc::clone(&self.paused),
                max_message_bytes,
            );
            let tunnel = Tunnel::new(
                TunnelSettings {
                    server: credentials.server,
                    device_id: credentials.device_id,
                    key: Arc::new(credentials.key),
                    store: credentials.store,
                    max_message_bytes,
                },
                files,
                Arc::clone(&self.audit),
                self.status.clone(),
            );
            if tunnel.run(token).await == TunnelExit::Revoked {
                self.wait_for_reload(shutdown).await;
            }
        }
    }

    async fn wait_for_reload(&self, shutdown: &CancellationToken) {
        tokio::select! {
            () = shutdown.cancelled() => {}
            () = self.reloaded.notified() => {}
        }
    }

    async fn reload(&self) -> Result<()> {
        let paths = self.paths.clone();
        let new = Config::load(&paths)?;
        let reader = Arc::clone(&self.reader);
        let (cfg, p) = (new.clone(), paths.clone());
        tokio::task::spawn_blocking(move || reader.reload(&cfg, &p)).await??;
        self.limiter.set_limits(new.limits.clone());
        self.paused.store(new.paused, Ordering::SeqCst);
        let mut config = self.config.lock().await;
        let pairing_changed =
            config.server != new.server || config.secret_store != new.secret_store;
        *config = new;
        drop(config);
        if pairing_changed {
            self.tunnel_token.lock().await.cancel();
        }
        self.reloaded.notify_one();
        self.audit.append(&AuditEntry::event("reloaded"));
        self.changes.bump();
        Ok(())
    }

    async fn add_root(&self, new: ControlRequest) -> Result<Value> {
        let ControlRequest::RootsAdd {
            path,
            label,
            follow_symlinks,
            i_know,
        } = new
        else {
            unreachable!("only called for roots_add");
        };
        let paths = self.paths.clone();
        let root = tokio::task::spawn_blocking(move || {
            let mut config = Config::load(&paths)?;
            let root = add_root(
                &mut config,
                &paths,
                NewRoot {
                    path: &path,
                    label,
                    i_know,
                    follow_symlinks,
                },
            )?;
            config.save(&paths)?;
            Ok::<_, anyhow::Error>(root)
        })
        .await??;
        let mut entry = AuditEntry::event("root_added");
        entry.detail = Some(format!("{} ({})", root.id, root.label));
        self.audit.append(&entry);
        self.reload().await?;
        Ok(json!({ "root": root }))
    }

    async fn remove_root(&self, which: String) -> Result<Value> {
        let paths = self.paths.clone();
        let root = tokio::task::spawn_blocking(move || {
            let mut config = Config::load(&paths)?;
            let root = remove_root(&mut config, &which)?;
            config.save(&paths)?;
            Ok::<_, anyhow::Error>(root)
        })
        .await??;
        let mut entry = AuditEntry::event("root_removed");
        entry.detail = Some(format!("{} ({})", root.id, root.label));
        self.audit.append(&entry);
        self.reload().await?;
        Ok(json!({ "root": root }))
    }

    async fn suggested_roots(&self) -> Value {
        let config = self.config.lock().await.clone();
        let suggestions: Vec<Value> = crate::paths::documents_dir()
            .into_iter()
            .map(|path| {
                let real = path.canonicalize().ok();
                let shared = config
                    .roots
                    .iter()
                    .any(|r| Some(&r.path) == real.as_ref() || r.path == path);
                json!({
                    "path": path,
                    "label": "Documents",
                    "exists": path.is_dir(),
                    "shared": shared,
                })
            })
            .collect();
        json!({ "suggestions": suggestions })
    }

    async fn set_paused(&self, paused: bool) -> Result<()> {
        self.paused.store(paused, Ordering::SeqCst);
        let mut config = self.config.lock().await;
        config.paused = paused;
        config.save(&self.paths)?;
        self.audit.append(&AuditEntry::event(if paused {
            "paused"
        } else {
            "resumed"
        }));
        self.changes.bump();
        Ok(())
    }

    async fn status_json(&self) -> Value {
        let config = self.config.lock().await.clone();
        let reader = Arc::clone(&self.reader);
        let roots = tokio::task::spawn_blocking(move || reader.roots())
            .await
            .unwrap_or_default();
        let roots: Vec<Value> = roots
            .into_iter()
            .map(|r| {
                let path = config.root(&r.id).map(|c| c.path.display().to_string());
                let mut v = serde_json::to_value(&r).unwrap_or_default();
                v["local_path"] = json!(path);
                v
            })
            .collect();
        json!({
            "protocol": PROTOCOL_VERSION,
            "version": env!("CARGO_PKG_VERSION"),
            "platform": std::env::consts::OS,
            "pid": std::process::id(),
            "connection": self.status.get(),
            "paired": config.server.is_some(),
            "config_file": self.paths.config_file(),
            "audit_file": self.paths.audit_file(),
            "server": config.server.as_ref().map(|s| &s.url),
            "device_id": config.server.as_ref().map(|s| &s.device_id),
            "paused": self.paused.load(Ordering::SeqCst),
            "roots": roots,
        })
    }
}

impl ControlHandler for Daemon {
    fn events(&self) -> Option<broadcast::Receiver<ControlEvent>> {
        Some(self.events.subscribe())
    }

    async fn initial_status(&self) -> Option<Value> {
        Some(self.status_json().await)
    }

    async fn handle(&self, request: ControlRequest) -> Result<Value> {
        match request {
            ControlRequest::Hello => Ok(json!({
                "protocol": PROTOCOL_VERSION,
                "version": env!("CARGO_PKG_VERSION"),
                "platform": std::env::consts::OS,
                "pid": std::process::id(),
            })),
            ControlRequest::Status => Ok(self.status_json().await),
            ControlRequest::Pause => {
                self.set_paused(true).await?;
                Ok(json!({ "paused": true }))
            }
            ControlRequest::Resume => {
                self.set_paused(false).await?;
                Ok(json!({ "paused": false }))
            }
            ControlRequest::Reload => {
                self.reload().await?;
                Ok(json!({}))
            }
            ControlRequest::Shutdown => {
                let mut entry = AuditEntry::event("shutdown_requested");
                entry.detail = Some("over the control channel".into());
                self.audit.append(&entry);
                // Answer first, then stop.
                let shutdown = self.shutdown.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    shutdown.cancel();
                });
                Ok(json!({ "stopping": true }))
            }
            request @ ControlRequest::RootsAdd { .. } => self.add_root(request).await,
            ControlRequest::RootsRemove { root } => self.remove_root(root).await,
            ControlRequest::AuditTail { lines } => {
                let path = self.audit.path().to_path_buf();
                let n = lines.unwrap_or(50).min(1000);
                let entries =
                    tokio::task::spawn_blocking(move || crate::audit::tail(&path, n)).await??;
                Ok(json!({ "entries": entries }))
            }
            ControlRequest::SuggestedRoots => Ok(self.suggested_roots().await),
            ControlRequest::Subscribe { .. } => {
                anyhow::bail!("subscribe is handled by the control channel")
            }
        }
    }
}

#[cfg(windows)]
async fn wait_for_signal() {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};
    let (Ok(mut c), Ok(mut brk), Ok(mut close), Ok(mut logoff), Ok(mut shutdown)) = (
        ctrl_c(),
        ctrl_break(),
        ctrl_close(),
        ctrl_logoff(),
        ctrl_shutdown(),
    ) else {
        std::future::pending::<()>().await;
        return;
    };
    tokio::select! {
        _ = c.recv() => {}
        _ = brk.recv() => {}
        _ = close.recv() => {}
        _ = logoff.recv() => {}
        _ = shutdown.recv() => {}
    }
}

#[cfg(unix)]
async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        std::future::pending::<()>().await;
        return;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}
