//! `cww daemon install|uninstall`: register the daemon per user.
//!
//! - Linux: a `systemd --user` unit at `~/.config/systemd/user/cww.service`
//!   with the hardening options from the design doc.
//! - macOS: a LaunchAgent plist at `~/Library/LaunchAgents/com.chatwithwork.cww.plist`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::paths::{Paths, home_dir};

pub const LAUNCHD_LABEL: &str = "com.chatwithwork.cww";

pub struct InstallOptions {
    /// Leave out the systemd sandboxing options, for systems where user
    /// services can't use them.
    pub no_hardening: bool,
}

pub fn install(paths: &Paths, options: &InstallOptions) -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .context("finding the cww binary")?
        .canonicalize()?;
    paths.ensure()?;
    if cfg!(target_os = "macos") {
        let plist = launch_agent_path()?;
        write_file(&plist, &launch_agent(&exe, paths))?;
        let domain = format!("gui/{}", rustix::process::getuid().as_raw());
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("{domain}/{LAUNCHD_LABEL}")])
            .status();
        run(
            "launchctl",
            &["bootstrap", &domain, &plist.to_string_lossy()],
        )?;
        Ok(plist)
    } else if cfg!(target_os = "linux") {
        let unit = systemd_unit_path()?;
        write_file(&unit, &systemd_unit(&exe, paths, options))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", "--now", "cww.service"])?;
        run("systemctl", &["--user", "restart", "cww.service"])?;
        Ok(unit)
    } else {
        bail!("cww daemon install supports Linux and macOS");
    }
}

/// Remove the service. With `purge`, also delete keys, config, index and logs.
pub fn uninstall(paths: &Paths, purge: bool) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    if cfg!(target_os = "macos") {
        let plist = launch_agent_path()?;
        let domain = format!("gui/{}", rustix::process::getuid().as_raw());
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("{domain}/{LAUNCHD_LABEL}")])
            .status();
        if plist.exists() {
            std::fs::remove_file(&plist)?;
            removed.push(plist);
        }
    } else if cfg!(target_os = "linux") {
        let unit = systemd_unit_path()?;
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", "cww.service"])
            .status();
        if unit.exists() {
            std::fs::remove_file(&unit)?;
            removed.push(unit);
        }
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
    }
    if purge {
        let _ = crate::auth::logout(paths);
        for dir in paths.all_dirs() {
            if dir.exists() {
                std::fs::remove_dir_all(dir)
                    .with_context(|| format!("removing {}", dir.display()))?;
                removed.push(dir.to_path_buf());
            }
        }
    }
    Ok(removed)
}

fn systemd_unit_path() -> Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map_or_else(|| home_dir().map(|h| h.join(".config")), Ok)?;
    Ok(config.join("systemd/user/cww.service"))
}

fn launch_agent_path() -> Result<PathBuf> {
    Ok(home_dir()?.join(format!("Library/LaunchAgents/{LAUNCHD_LABEL}.plist")))
}

pub fn systemd_unit(exe: &Path, paths: &Paths, options: &InstallOptions) -> String {
    let mut unit = format!(
        "[Unit]\n\
         Description=Chat with Work local files agent (cww)\n\
         Documentation=https://github.com/crmne/chatwithwork-agent\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=\"{exe}\" daemon run\n\
         Restart=on-failure\n\
         RestartSec=5\n",
        exe = exe.display()
    );
    if !options.no_hardening {
        let writable: Vec<String> = [
            &paths.config_dir,
            &paths.data_dir,
            &paths.state_dir,
            &paths.runtime_dir,
        ]
        .iter()
        .map(|p| format!("\"{}\"", p.display()))
        .collect();
        unit.push_str(&format!(
            "NoNewPrivileges=yes\n\
             PrivateTmp=yes\n\
             ProtectSystem=strict\n\
             ReadWritePaths={}\n\
             RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6\n\
             SystemCallFilter=@system-service\n\
             SystemCallArchitectures=native\n\
             MemoryDenyWriteExecute=yes\n\
             LockPersonality=yes\n\
             RestrictRealtime=yes\n\
             RestrictSUIDSGID=yes\n",
            writable.join(" ")
        ));
    }
    unit.push_str("\n[Install]\nWantedBy=default.target\n");
    unit
}

pub fn launch_agent(exe: &Path, paths: &Paths) -> String {
    let log = paths.daemon_log_file();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>daemon</string>
    <string>run</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ProcessType</key>
  <string>Background</string>
  <key>ThrottleInterval</key>
  <integer>10</integer>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        exe = xml_escape(&exe.to_string_lossy()),
        log = xml_escape(&log.to_string_lossy()),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("{program} {} failed ({status})", args.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_and_plist_render() {
        let paths = Paths::under(Path::new("/home/u/.cww"));
        let unit = systemd_unit(
            Path::new("/usr/bin/cww"),
            &paths,
            &InstallOptions {
                no_hardening: false,
            },
        );
        assert!(unit.contains("ExecStart=\"/usr/bin/cww\" daemon run"));
        assert!(unit.contains("ProtectSystem=strict"));
        assert!(unit.contains("ReadWritePaths=\"/home/u/.cww/config\""));
        let plain = systemd_unit(
            Path::new("/usr/bin/cww"),
            &paths,
            &InstallOptions { no_hardening: true },
        );
        assert!(!plain.contains("ProtectSystem"));
        let plist = launch_agent(Path::new("/opt/homebrew/bin/cww"), &paths);
        assert!(plist.contains("<string>com.chatwithwork.cww</string>"));
        assert!(plist.contains("<string>/opt/homebrew/bin/cww</string>"));
    }
}
