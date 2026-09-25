//! Starting the daemon. The app doesn't run it itself: `cww daemon install`
//! registers the per-user service (systemd on Linux, launchd on macOS, a
//! logon Scheduled Task on Windows),
//! which starts it now and at every login, and keeps it running when the
//! app is closed.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

#[cfg(windows)]
const CWW: &str = "cww.exe";
#[cfg(not(windows))]
const CWW: &str = "cww";

/// The `cww` command: next to the app (bundles and packages ship both),
/// then on the PATH, then where installers put it.
pub fn cww_binary() -> Option<PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.canonicalize().ok())
        .and_then(|exe| exe.parent().map(|dir| dir.join(CWW)));
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(CWW));
    let home = crate::paths::home_dir();
    let well_known = [
        Some(PathBuf::from("/opt/homebrew/bin/cww")),
        Some(PathBuf::from("/usr/local/bin/cww")),
        Some(PathBuf::from("/home/linuxbrew/.linuxbrew/bin/cww")),
        home.as_ref().map(|h| h.join(".cargo/bin").join(CWW)),
    ];
    beside
        .into_iter()
        .chain(on_path)
        .chain(well_known.into_iter().flatten())
        .find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(windows)]
    {
        path.is_file()
    }
}

/// Register and start the daemon's service. Returns what `cww` printed.
pub fn start() -> Result<String> {
    let cww = cww_binary()
        .context("Couldn't find the cww command. Install the Local Agent, then try again.")?;
    let out = Command::new(&cww)
        .args(["daemon", "install"])
        .output()
        .with_context(|| format!("running {}", cww.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let message = stderr.trim().trim_start_matches("error: ");
        bail!("Couldn't start the Local Agent: {message}");
    }
    Ok(stdout)
}
