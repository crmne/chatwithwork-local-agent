//! Where cww keeps its files.
//!
//! cww uses XDG-style directories on Linux and macOS alike, so the layout is
//! the same everywhere and easy to audit:
//!
//! | What                  | Default                          |
//! |-----------------------|----------------------------------|
//! | config and secrets    | `~/.config/cww`                  |
//! | search index          | `~/.local/share/cww`             |
//! | audit log, daemon log | `~/.local/state/cww`             |
//! | control socket        | `$XDG_RUNTIME_DIR/cww`, else the state directory |
//!
//! `CWW_HOME` puts all four under one directory, which is what the tests and
//! side-by-side development installs use.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Leaves room under the ~104-byte `sun_path` limit of macOS.
const MAX_SOCKET_PATH: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Paths {
    /// Resolve the directories from the environment.
    pub fn from_env() -> Result<Self> {
        if let Some(home) = std::env::var_os("CWW_HOME").filter(|v| !v.is_empty()) {
            return Ok(Self::under(Path::new(&home)));
        }
        let home = home_dir()?;
        let xdg = |var: &str, default: &str| -> PathBuf {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(default))
        };
        let state_dir = xdg("XDG_STATE_HOME", ".local/state").join("cww");
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|p| p.join("cww"))
            .unwrap_or_else(|| state_dir.clone());
        Ok(Self {
            config_dir: xdg("XDG_CONFIG_HOME", ".config").join("cww"),
            data_dir: xdg("XDG_DATA_HOME", ".local/share").join("cww"),
            state_dir,
            runtime_dir,
        })
    }

    /// Put every directory under `base`.
    pub fn under(base: &Path) -> Self {
        Self {
            config_dir: base.join("config"),
            data_dir: base.join("data"),
            state_dir: base.join("state"),
            runtime_dir: base.join("run"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Fallback secret store, used only when the OS keychain is unavailable.
    pub fn secrets_file(&self) -> PathBuf {
        self.config_dir.join("secrets.json")
    }

    pub fn index_dir(&self) -> PathBuf {
        self.data_dir.join("index")
    }

    pub fn audit_file(&self) -> PathBuf {
        self.state_dir.join("audit.jsonl")
    }

    pub fn daemon_log_file(&self) -> PathBuf {
        self.state_dir.join("daemon.log")
    }

    /// The control socket. Unix socket paths are limited to about 100
    /// bytes, so a long runtime directory falls back to a short private
    /// directory named after a hash of the preferred path.
    pub fn socket_path(&self) -> PathBuf {
        use std::os::unix::ffi::OsStrExt;
        let preferred = self.runtime_dir.join("cww.sock");
        if preferred.as_os_str().len() <= MAX_SOCKET_PATH {
            return preferred;
        }
        let digest = ring::digest::digest(&ring::digest::SHA256, preferred.as_os_str().as_bytes());
        let short: String = digest.as_ref()[..6]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.as_os_str().len() < 60)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let uid = rustix::process::geteuid().as_raw();
        base.join(format!("cww-{uid}-{short}")).join("cww.sock")
    }

    /// Every directory cww writes to. The deny list blocks all of them, so a
    /// root that contains them can never serve the index or the secrets.
    pub fn all_dirs(&self) -> [&Path; 4] {
        [
            &self.config_dir,
            &self.data_dir,
            &self.state_dir,
            &self.runtime_dir,
        ]
    }

    /// Create the directories with mode 0700 and tighten existing ones.
    pub fn ensure(&self) -> Result<()> {
        for dir in self.all_dirs() {
            ensure_private_dir(dir)?;
        }
        Ok(())
    }
}

pub fn home_dir() -> Result<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) if Path::new(&home).is_absolute() => Ok(PathBuf::from(home)),
        _ => bail!("HOME is not set to an absolute path"),
    }
}

pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    match fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
    {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
    }
    let meta = fs::symlink_metadata(dir).with_context(|| format!("checking {}", dir.display()))?;
    if !meta.is_dir() {
        bail!("{} exists and is not a directory", dir.display());
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        bail!("{} is owned by another user", dir.display());
    }
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("setting 0700 on {}", dir.display()))?;
    }
    Ok(())
}

/// Write `contents` to `path` atomically with mode 0600.
pub fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let dir = path.parent().context("path has no parent")?;
    ensure_private_dir(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_creates_private_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());
        paths.ensure().unwrap();
        for dir in paths.all_dirs() {
            let mode = fs::metadata(dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
        }
    }

    #[test]
    fn long_socket_paths_get_a_short_fallback() {
        let short = Paths::under(Path::new("/home/u/.cww"));
        assert_eq!(
            short.socket_path(),
            PathBuf::from("/home/u/.cww/run/cww.sock")
        );
        let long = Paths::under(&PathBuf::from(format!("/home/u/{}", "x".repeat(120))));
        let socket = long.socket_path();
        assert!(
            socket.as_os_str().len() <= MAX_SOCKET_PATH,
            "{}",
            socket.display()
        );
        assert_eq!(socket, long.socket_path(), "stable");
    }

    #[test]
    fn private_files_are_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("sub/secret");
        write_private_file(&file, b"x").unwrap();
        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(fs::read(&file).unwrap(), b"x");
    }
}
