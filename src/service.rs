//! `cww daemon install|uninstall`: register the daemon per user.
//!
//! - Linux: a `systemd --user` unit at `~/.config/systemd/user/cww.service`
//!   with the hardening options from the design doc.
//! - macOS: a LaunchAgent plist at `~/Library/LaunchAgents/com.chatwithwork.cww.plist`.
//! - Windows: a Scheduled Task that starts at logon, runs as the user with
//!   least privilege, and restarts on failure. It runs `cww-agent.exe`, the
//!   daemon built without a console, so no window appears. If Task Scheduler
//!   denies access, a per-user Startup shortcut starts it at login instead.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::paths::{Paths, home_dir};

#[cfg(windows)]
#[path = "service/windows_startup.rs"]
mod windows_startup;

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
        // Fails harmlessly when nothing is loaded yet; keep that quiet.
        let bootout = Command::new("launchctl")
            .args(["bootout", &format!("{domain}/{LAUNCHD_LABEL}")])
            .stderr(std::process::Stdio::null())
            .status();
        if bootout.is_ok_and(|status| status.success()) {
            wait_for_launchd_removal(|| {
                Command::new("launchctl")
                    .args(["print", &format!("{domain}/{LAUNCHD_LABEL}")])
                    .output()
                    .map(|output| output.status.success())
                    .context("waiting for the previous Local Agent to stop")
            })?;
        }
        let output = Command::new("launchctl")
            .args(["bootstrap", &domain, &plist.to_string_lossy()])
            .output()
            .context("registering the Local Agent with launchd")?;
        if !output.status.success() {
            bail!(
                "launchctl bootstrap failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
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
        // Fails harmlessly when nothing is loaded yet; keep that quiet.
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("{domain}/{LAUNCHD_LABEL}")])
            .stderr(std::process::Stdio::null())
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

/// `bootout` returns before launchd finishes removing the old service.
/// An immediate `bootstrap` can then report exit code 5 (I/O error), even
/// though launchd logs the underlying EALREADY. Wait for that service to
/// disappear instead of interpreting bootstrap's ambiguous error codes.
fn wait_for_launchd_removal(mut still_registered: impl FnMut() -> Result<bool>) -> Result<()> {
    for _ in 0..100 {
        if !still_registered()? {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("The previous Local Agent is still stopping. Try again in a moment.")
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
            (agent, Vec::new())
        } else {
            (exe.to_path_buf(), vec!["daemon".into(), "run".into()])
        };
        arguments.extend(["--log-file".into(), log.to_string_lossy().into_owned()]);
        // A task can't set environment variables, so pass CWW_HOME along.
        if let Some(home) = custom_home() {
            arguments.extend(["--home".into(), home.to_string_lossy().into_owned()]);
        }
        let xml = task_xml(&command, &quote_arguments(&arguments), &current_user()?);
        let file = paths.state_dir.join("cww-task.xml");
        // schtasks reads task XML as UTF-16 with a byte order mark.
        let mut bytes = vec![0xFF, 0xFE];
        for unit in xml.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&file, bytes).with_context(|| format!("writing {}", file.display()))?;
        let registered = task_command(
            "schtasks",
            &[
                "/Create",
                "/TN",
                TASK_NAME,
                "/XML",
                &file.to_string_lossy(),
                "/F",
            ],
        );
        stop_existing(paths)?;
        let started =
            registered.and_then(|()| task_command("schtasks", &["/Run", "/TN", TASK_NAME]));
        match started {
            Ok(()) => {
                #[cfg(windows)]
                if let Err(error) = windows_startup::remove() {
                    tracing::warn!(%error, "could not remove the previous startup shortcut");
                }
                wait_for_start(paths)?;
                Ok(file)
            }
            Err(error) => {
                #[cfg(windows)]
                {
                    // Standard users and existing tasks owned by an elevated
                    // installer can deny registration. The current user's
                    // Startup folder needs no Task Scheduler permissions.
                    let shortcut = windows_startup::install(&command, &arguments)
                        .with_context(|| format!("Task Scheduler was unavailable ({error:#}); setting up startup for your account also failed"))?;
                    stop_existing(paths)?;
                    windows_startup::start(&command, &arguments)?;
                    wait_for_start(paths)?;
                    Ok(shortcut)
                }
                #[cfg(not(windows))]
                Err(error)
            }
        }
    }

    pub fn uninstall(paths: &Paths) -> Result<Vec<PathBuf>> {
        let mut removed = Vec::new();
        #[cfg(windows)]
        if let Some(shortcut) = windows_startup::remove()? {
            removed.push(shortcut);
        }
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

    fn task_command(program: &str, args: &[&str]) -> Result<()> {
        let mut command = Command::new(program);
        command.args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let output = command
            .output()
            .with_context(|| format!("running {program}"))?;
        if !output.status.success() {
            bail!(
                "{program} failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn stop_existing(paths: &Paths) -> Result<()> {
        use crate::control::{ControlRequest, request};
        let _ = request(&paths.socket_path(), ControlRequest::Shutdown);
        for _ in 0..50 {
            if !matches!(
                request(&paths.socket_path(), ControlRequest::Status),
                Ok(Some(_))
            ) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        bail!("The previous Local Agent did not stop. Try again in a moment.")
    }

    fn wait_for_start(paths: &Paths) -> Result<()> {
        for _ in 0..100 {
            if matches!(
                crate::control::request(
                    &paths.socket_path(),
                    crate::control::ControlRequest::Status
                ),
                Ok(Some(_))
            ) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        bail!(
            "The Local Agent did not start. See {} for details.",
            paths.daemon_log_file().display()
        )
    }

    /// Windows command-line quoting, including backslashes before quotes
    /// and before the closing quote of a directory ending in a backslash.
    pub(super) fn quote_arguments(arguments: &[String]) -> String {
        arguments
            .iter()
            .map(|arg| {
                let mut quoted = String::from('"');
                let mut slashes = 0;
                for c in arg.chars() {
                    if c == '\\' {
                        slashes += 1;
                    } else {
                        quoted.extend(std::iter::repeat_n(
                            '\\',
                            slashes * if c == '"' { 2 } else { 1 },
                        ));
                        if c == '"' {
                            quoted.push('\\');
                        }
                        quoted.push(c);
                        slashes = 0;
                    }
                }
                quoted.extend(std::iter::repeat_n('\\', slashes * 2));
                quoted.push('"');
                quoted
            })
            .collect::<Vec<_>>()
            .join(" ")
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

/// Run a service manager. What it prints goes to stderr: stdout may be
/// `cww login --json`'s, which only carries JSON.
fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .stdout(std::io::stderr())
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
    fn launchd_waits_until_the_old_registration_is_gone() {
        let mut calls = 0;
        wait_for_launchd_removal(|| {
            calls += 1;
            Ok(calls == 1)
        })
        .unwrap();
        assert_eq!(calls, 2);

        let error = wait_for_launchd_removal(|| bail!("cannot run launchctl")).unwrap_err();
        assert!(error.to_string().contains("cannot run launchctl"));
    }

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

    #[test]
    fn windows_arguments_preserve_spaces_quotes_and_trailing_backslashes() {
        let arguments = ["--home", r"C:\Users\Zoë\Folder & files\", "a\"b", ""].map(str::to_string);
        assert_eq!(
            windows_task::quote_arguments(&arguments),
            "\"--home\" \"C:\\Users\\Zoë\\Folder & files\\\\\" \"a\\\"b\" \"\""
        );
    }
}
