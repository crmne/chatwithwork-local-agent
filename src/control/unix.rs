//! Control transport for Linux and macOS: a Unix socket.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::net::{UnixListener, UnixStream};

pub type ClientStream = StdUnixStream;

pub struct Listener {
    listener: UnixListener,
    path: PathBuf,
    uid: u32,
}

pub fn bind(path: &Path) -> Result<Listener> {
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
    Ok(Listener {
        listener,
        path: path.to_path_buf(),
        uid: rustix::process::geteuid().as_raw(),
    })
}

impl Listener {
    /// The next connection from a process of the same user.
    pub async fn accept(&mut self) -> Result<UnixStream> {
        let (stream, _) = self.listener.accept().await?;
        let cred = stream.peer_cred().context("checking the control peer")?;
        if cred.uid() != self.uid {
            bail!("refused a connection from user {}", cred.uid());
        }
        Ok(stream)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn connect(path: &Path) -> Result<Option<ClientStream>> {
    let stream = match StdUnixStream::connect(path) {
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
    Ok(Some(stream))
}

pub fn clear_timeout(stream: &ClientStream) {
    let _ = stream.set_read_timeout(None);
}
