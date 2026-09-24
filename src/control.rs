//! The local control socket, used by the CLI to talk to the daemon.
//!
//! A Unix socket at `$XDG_RUNTIME_DIR/cww/cww.sock` (mode 0600, in a 0700
//! directory). Every connection's peer UID must match the daemon's, so
//! other users can't drive it. One JSON request per line, one JSON response
//! per line.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "cmd")]
pub enum ControlRequest {
    Status,
    Pause,
    Resume,
    /// Re-read the config file (after `cww roots add`, `cww login`, ...).
    Reload,
}

pub trait ControlHandler: Send + Sync + 'static {
    fn handle(&self, request: ControlRequest) -> impl Future<Output = Result<Value>> + Send;
}

/// Bind the socket, refusing if another daemon is already listening.
pub fn bind(path: &Path) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        crate::paths::ensure_private_dir(dir)?;
    }
    if path.exists() {
        if StdUnixStream::connect(path).is_ok() {
            bail!("another cww daemon is already running ({})", path.display());
        }
        std::fs::remove_file(path).with_context(|| format!("removing stale {}", path.display()))?;
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn serve<H: ControlHandler>(
    listener: UnixListener,
    handler: Arc<H>,
    shutdown: CancellationToken,
) {
    let uid = rustix::process::geteuid().as_raw();
    loop {
        let stream = tokio::select! {
            () = shutdown.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    tracing::warn!("control socket accept failed: {e}");
                    continue;
                }
            },
        };
        match stream.peer_cred() {
            Ok(cred) if cred.uid() == uid => {}
            Ok(cred) => {
                tracing::warn!(
                    peer_uid = cred.uid(),
                    "refused a control connection from another user"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!("can't check the control peer: {e}");
                continue;
            }
        }
        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, handler).await {
                tracing::debug!("control connection: {e:#}");
            }
        });
    }
}

async fn handle_connection<H: ControlHandler>(stream: UnixStream, handler: Arc<H>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = TokioBufReader::new(read).lines();
    while let Some(line) =
        tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await??
    {
        if line.len() > 4096 {
            bail!("control request too long");
        }
        let response = match serde_json::from_str::<ControlRequest>(&line) {
            Ok(request) => match handler.handle(request).await {
                Ok(mut value) => {
                    value["ok"] = json!(true);
                    value
                }
                Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
            },
            Err(e) => json!({ "ok": false, "error": format!("bad request: {e}") }),
        };
        let mut out = serde_json::to_vec(&response)?;
        out.push(b'\n');
        write.write_all(&out).await?;
    }
    Ok(())
}

/// Send one request to the running daemon. `Ok(None)` means no daemon is
/// listening.
pub fn request(path: &Path, request: ControlRequest) -> Result<Option<Value>> {
    let mut stream = match StdUnixStream::connect(path) {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e).with_context(|| format!("connecting to {}", path.display())),
    };
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut response = String::new();
    BufReader::new(&stream).read_line(&mut response)?;
    let value: Value = serde_json::from_str(&response).context("bad response from the daemon")?;
    if value["ok"] != json!(true) {
        bail!(
            "{}",
            value["error"]
                .as_str()
                .unwrap_or("the daemon returned an error")
        );
    }
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl ControlHandler for Echo {
        async fn handle(&self, request: ControlRequest) -> Result<Value> {
            match request {
                ControlRequest::Pause => bail!("nope"),
                other => Ok(json!({ "got": other })),
            }
        }
    }

    #[tokio::test]
    async fn round_trip_and_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("run/cww.sock");
        let listener = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(bind(&path).is_err(), "second daemon refused");

        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(listener, Arc::new(Echo), shutdown.clone()));
        let p = path.clone();
        let status = tokio::task::spawn_blocking(move || request(&p, ControlRequest::Status))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(status["got"]["cmd"], "status");
        let p = path.clone();
        let err = tokio::task::spawn_blocking(move || request(&p, ControlRequest::Pause))
            .await
            .unwrap()
            .unwrap_err();
        assert!(err.to_string().contains("nope"));
        shutdown.cancel();
        task.await.unwrap();

        let missing = tmp.path().join("none.sock");
        assert!(request(&missing, ControlRequest::Status).unwrap().is_none());
    }
}
