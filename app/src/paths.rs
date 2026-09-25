//! Where the daemon's control channel and the app's own state live. The
//! daemon's layout comes from the `cww` crate, so the app always finds the
//! socket (or, on Windows, the pipe) exactly where the daemon listens.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub daemon: cww::paths::Paths,
    /// A control endpoint somewhere else (the demo, tests).
    pub socket: Option<PathBuf>,
}

impl Paths {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            daemon: cww::paths::Paths::from_env()?,
            socket: None,
        })
    }

    /// Everything under `base`, like `CWW_HOME` does for the daemon.
    #[cfg_attr(not(any(test, feature = "demo")), allow(dead_code))]
    pub fn under(base: &Path) -> Self {
        Self {
            daemon: cww::paths::Paths::under(base),
            socket: None,
        }
    }

    /// The app's own settings (onboarding done, and so on).
    pub fn app_state_file(&self) -> PathBuf {
        self.daemon.config_dir.join("app.json")
    }

    /// The daemon's control endpoint.
    pub fn socket_path(&self) -> PathBuf {
        self.socket
            .clone()
            .unwrap_or_else(|| self.daemon.socket_path())
    }

    /// The app's own single-instance endpoint, next to the daemon's.
    pub fn instance_path(&self) -> PathBuf {
        let socket = self.socket_path();
        #[cfg(windows)]
        {
            PathBuf::from(format!("{}-app", socket.display()))
        }
        #[cfg(unix)]
        {
            socket.with_file_name("app.sock")
        }
    }
}

pub fn home_dir() -> Option<PathBuf> {
    cww::paths::home_dir().ok()
}

/// The user's Documents folder, or the platform's equivalent.
pub fn documents_dir() -> Option<PathBuf> {
    cww::paths::documents_dir().filter(|p| p.is_dir())
}

/// Show `path` with `~` for the home folder, as people know it.
pub fn display(path: &Path) -> String {
    if let Some(home) = home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display());
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_lives_next_to_the_daemon() {
        let base = std::env::temp_dir().join("cww-paths-test");
        let paths = Paths::under(&base);
        assert_eq!(
            paths.socket_path(),
            cww::paths::Paths::under(&base).socket_path()
        );
        assert_eq!(paths.app_state_file(), base.join("config").join("app.json"));
        assert_ne!(paths.instance_path(), paths.socket_path());
        let moved = Paths {
            socket: Some(base.join("elsewhere.sock")),
            ..paths
        };
        assert_eq!(moved.socket_path(), base.join("elsewhere.sock"));
    }

    #[test]
    fn home_is_shown_as_a_tilde() {
        let Some(home) = home_dir() else { return };
        assert_eq!(display(&home), "~");
        let docs = home.join("Documents");
        assert_eq!(
            display(&docs),
            format!("~{}Documents", std::path::MAIN_SEPARATOR)
        );
    }
}
