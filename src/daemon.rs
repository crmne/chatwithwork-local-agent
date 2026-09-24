//! `cww daemon run`: the reader, the control socket and the tunnel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use crate::audit::{AuditEntry, AuditLog};
use crate::auth::load_credentials;
use crate::config::Config;
use crate::control::{self, ControlHandler, ControlRequest};
use crate::limits::Limiter;
use crate::paths::Paths;
use crate::reader::Reader;
use crate::status::{Connection, SharedStatus};
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
}

/// Run the daemon until `shutdown` is cancelled (or SIGINT/SIGTERM when
/// `handle_signals` is set).
pub async fn run(paths: Paths, shutdown: CancellationToken, handle_signals: bool) -> Result<()> {
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    paths.ensure()?;
    let config = Config::load(&paths)?;
    let audit = Arc::new(AuditLog::open(&paths.audit_file())?);
    // Bind first: a second daemon must fail before it touches the index.
    let listener = control::bind(&paths.socket_path())?;

    let reader = {
        let (config, paths) = (config.clone(), paths.clone());
        tokio::task::spawn_blocking(move || Reader::new(&config, &paths))
            .await
            .context("starting the reader")??
    };
    let daemon = Arc::new(Daemon {
        limiter: Arc::new(Limiter::new(config.limits.clone())),
        paused: Arc::new(AtomicBool::new(config.paused)),
        config: Mutex::new(config),
        reader: Arc::new(reader),
        audit,
        status: SharedStatus::default(),
        tunnel_token: Mutex::new(shutdown.child_token()),
        reloaded: Notify::new(),
        paths,
    });

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
    let _ = std::fs::remove_file(daemon.paths.socket_path());
    Ok(())
}

impl Daemon {
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
        Ok(())
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
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "connection": self.status.get(),
            "server": config.server.as_ref().map(|s| &s.url),
            "device_id": config.server.as_ref().map(|s| &s.device_id),
            "paused": self.paused.load(Ordering::SeqCst),
            "roots": roots,
        })
    }
}

impl ControlHandler for Daemon {
    async fn handle(&self, request: ControlRequest) -> Result<Value> {
        match request {
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
        }
    }
}

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
