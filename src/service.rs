//! `cww daemon install|uninstall`: register the daemon per user.
//!
//! - Linux: a `systemd --user` unit at `~/.config/systemd/user/cww.service`
//!   with the hardening options from the design doc.
//! - macOS: a LaunchAgent plist at `~/Library/LaunchAgents/com.chatwithwork.cww.plist`.
//! - Windows: a Scheduled Task that starts at logon, runs as the user with
//!   least privilege, and restarts on failure. It runs `cww-agent.exe`, the
//!   daemon built without a console, so no window appears.

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
    let exe = stable_exe()?;
    paths.ensure()?;
    if cfg!(target_os = "macos") {
        let plist = launch_agent_path()?;
        write_file(&plist, &launch_agent(&exe, paths))?;
        let domain = launchd_domain();
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("{domain}/{LAUNCHD_LABEL}")])
            .status();
        run(
            "launchctl",
            &["bootstrap", &domain, &plist.to_string_lossy()],
        )?;
        Ok(plist)
    } else if cfg!(target_os = "linux") {
        // A distribution package ships the unit already; enable that one
        // instead of shadowing it, unless the paths were customized.
        let packaged = Path::new(PACKAGED_UNIT);
        if exe == Path::new("/usr/bin/cww")
            && packaged.exists()
            && Paths::from_env().ok().as_ref() == Some(paths)
            && std::env::var_os("CWW_HOME").is_none()
            && !options.no_hardening
        {
            let _ = std::fs::remove_file(systemd_unit_path()?);
            run("systemctl", &["--user", "daemon-reload"])?;
            run("systemctl", &["--user", "enable", "--now", "cww.service"])?;
            run("systemctl", &["--user", "restart", "cww.service"])?;
            return Ok(packaged.to_path_buf());
        }
        let unit = systemd_unit_path()?;
        write_file(&unit, &systemd_unit(&exe, paths, options))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", "--now", "cww.service"])?;
        run("systemctl", &["--user", "restart", "cww.service"])?;
        Ok(unit)
    } else if cfg!(windows) {
        windows_task::install(&exe, paths)
    } else {
        bail!("cww daemon install supports Linux, macOS and Windows");
    }
}

/// Remove the service. With `purge`, also delete keys, config, index and logs.
pub fn uninstall(paths: &Paths, purge: bool) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    if cfg!(target_os = "macos") {
        let plist = launch_agent_path()?;
        let domain = launchd_domain();
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
    } else if cfg!(windows) {
        removed.extend(windows_task::uninstall(paths)?);
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

/// `CWW_HOME`, when set, so the service uses the same directories as the
/// command that installed it.
fn custom_home() -> Option<PathBuf> {
    std::env::var_os("CWW_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The launchd domain of the logged-in user, `gui/<uid>`.
fn launchd_domain() -> String {
    #[cfg(unix)]
    return format!("gui/{}", rustix::process::getuid().as_raw());
    #[cfg(not(unix))]
    String::new()
}

/// Where distribution packages install the systemd user unit.
pub const PACKAGED_UNIT: &str = "/usr/lib/systemd/user/cww.service";

/// The path to register in the service. Package managers keep a stable
/// symlink (`/opt/homebrew/bin/cww`) pointing into a versioned directory
/// that disappears on upgrade, so prefer a well-known path that resolves to
/// this same binary.
fn stable_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding the cww binary")?;
    if cfg!(windows) {
        return crate::roots::canonical(&exe);
    }
    let real = exe.canonicalize()?;
    let mut candidates: Vec<PathBuf> = [
        "/opt/homebrew/bin/cww",
        "/usr/local/bin/cww",
        "/home/linuxbrew/.linuxbrew/bin/cww",
        "/usr/bin/cww",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
        candidates.push(PathBuf::from(cargo_home).join("bin/cww"));
    }
    if let Ok(home) = home_dir() {
        candidates.push(home.join(".cargo/bin/cww"));
    }
    Ok(candidates
        .into_iter()
        .find(|c| c.canonicalize().ok().as_ref() == Some(&real))
        .unwrap_or(real))
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
         Description=Chat with Work Local Agent (cww)\n\
         Documentation=https://github.com/crmne/chatwithwork-local-agent\n\
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
    if let Some(home) = custom_home() {
        unit.push_str(&format!("Environment=\"CWW_HOME={}\"\n", home.display()));
    }
    if !options.no_hardening {
        let mut dirs: Vec<&Path> = paths.all_dirs().to_vec();
        // A long runtime directory moves the socket somewhere shorter.
        let socket = paths.socket_path();
        if let Some(dir) = socket.parent()
            && !dirs.iter().any(|d| dir.starts_with(d))
        {
            dirs.push(dir);
        }
        // "-": a directory that doesn't exist yet mustn't stop the service.
        let writable: Vec<String> = dirs
            .iter()
            .map(|p| format!("\"-{}\"", p.display()))
            .collect();
        // A private /tmp would hide cww's own directories if they live there.
        let in_tmp = dirs
            .iter()
            .any(|d| d.starts_with("/tmp") || d.starts_with("/var/tmp"));
        unit.push_str("NoNewPrivileges=yes\n");
        if !in_tmp {
            unit.push_str("PrivateTmp=yes\n");
        }
        unit.push_str(&format!(
            "ProtectSystem=strict\n\
             ReadWritePaths={}\n\
             RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6\n\
             SystemCallFilter=@system-service @sandbox\n\
             SystemCallErrorNumber=EPERM\n\
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
{environment}  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        exe = xml_escape(&exe.to_string_lossy()),
        log = xml_escape(&log.to_string_lossy()),
        environment = custom_home()
            .map(|home| format!(
                "  <key>EnvironmentVariables</key>\n  <dict>\n    <key>CWW_HOME</key>\n    <string>{}</string>\n  </dict>\n",
                xml_escape(&home.to_string_lossy())
            ))
            .unwrap_or_default(),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// The Windows Scheduled Task.
mod windows_task {
    use super::*;

    pub const TASK_NAME: &str = "Chat with Work Local Agent";

    pub fn install(exe: &Path, paths: &Paths) -> Result<PathBuf> {
        let log = paths.daemon_log_file();
        // cww-agent.exe ships next to cww.exe and opens no console window.
        // A bare cww.exe (cargo install) still works, with a console window.
        let agent = exe.with_file_name("cww-agent.exe");
        let (command, mut arguments) = if agent.exists() {
            (agent, format!("--log-file \"{}\"", log.display()))
        } else {
            (
                exe.to_path_buf(),
                format!("daemon run --log-file \"{}\"", log.display()),
            )
        };
        // A task can't set environment variables, so pass CWW_HOME along.
        if let Some(home) = custom_home() {
            arguments.push_str(&format!(" --home \"{}\"", home.display()));
        }
        let xml = task_xml(&command, &arguments, &current_user()?);
        let file = paths.state_dir.join("cww-task.xml");
        // schtasks reads task XML as UTF-16 with a byte order mark.
        let mut bytes = vec![0xFF, 0xFE];
        for unit in xml.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&file, bytes).with_context(|| format!("writing {}", file.display()))?;
        // Replace a running daemon, if any, so the new binary takes over.
        let _ = crate::control::request(
            &paths.socket_path(),
            crate::control::ControlRequest::Shutdown,
        );
        run(
            "schtasks",
            &[
                "/Create",
                "/TN",
                TASK_NAME,
                "/XML",
                &file.to_string_lossy(),
                "/F",
            ],
        )?;
        run("schtasks", &["/Run", "/TN", TASK_NAME])?;
        Ok(file)
    }

    pub fn uninstall(paths: &Paths) -> Result<Vec<PathBuf>> {
        let mut removed = Vec::new();
        let _ = crate::control::request(
            &paths.socket_path(),
            crate::control::ControlRequest::Shutdown,
        );
        let _ = Command::new("schtasks")
            .args(["/End", "/TN", TASK_NAME])
            .output();
        let deleted = Command::new("schtasks")
            .args(["/Delete", "/TN", TASK_NAME, "/F"])
            .output()
            .is_ok_and(|o| o.status.success());
        if deleted {
            removed.push(PathBuf::from(format!("Scheduled Task \\{TASK_NAME}")));
        }
        let file = paths.state_dir.join("cww-task.xml");
        if file.exists() {
            std::fs::remove_file(&file)?;
            removed.push(file);
        }
        Ok(removed)
    }

    /// `DOMAIN\user`, which the logon trigger and principal need.
    fn current_user() -> Result<String> {
        let user = std::env::var("USERNAME").context("USERNAME is not set")?;
        Ok(match std::env::var("USERDOMAIN") {
            Ok(domain) if !domain.is_empty() => format!("{domain}\\{user}"),
            _ => user,
        })
    }

    pub fn task_xml(command: &Path, arguments: &str, user: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Chat with Work Local Agent (cww): shares the folders you choose with Chat with Work, read-only.</Description>
    <URI>\{TASK_NAME}</URI>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>999</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
            user = xml_escape(user),
            command = xml_escape(&command.to_string_lossy()),
            arguments = xml_escape(arguments),
        )
    }
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

    #[cfg(unix)]
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
        assert!(unit.contains("ReadWritePaths=\"-/home/u/.cww/config\""));
        assert!(unit.contains("SystemCallFilter=@system-service @sandbox"));
        assert!(unit.contains("PrivateTmp=yes"));
        let in_tmp = systemd_unit(
            Path::new("/usr/bin/cww"),
            &Paths::under(Path::new("/tmp/cww-test")),
            &InstallOptions {
                no_hardening: false,
            },
        );
        assert!(!in_tmp.contains("PrivateTmp"), "{in_tmp}");
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

    #[test]
    fn task_xml_renders() {
        let task = windows_task::task_xml(
            Path::new(r"C:\Apps\cww-agent.exe"),
            r#"--log-file "C:\Logs\daemon.log""#,
            "PC\\carmine",
        );
        assert!(task.contains("<UserId>PC\\carmine</UserId>"));
        assert!(task.contains("<Command>C:\\Apps\\cww-agent.exe</Command>"));
        assert!(
            task.contains("<Arguments>--log-file &quot;C:\\Logs\\daemon.log&quot;</Arguments>")
        );
        assert!(task.contains("<RunLevel>LeastPrivilege</RunLevel>"));
    }
}
