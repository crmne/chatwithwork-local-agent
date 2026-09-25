//! "Start at login" for the app itself: an XDG autostart entry on Linux, a
//! LaunchAgent on macOS, and the Run key on Windows. The daemon has its own
//! service (`cww daemon install`), which always starts at login.
//!
//! The entry starts the app with `--background`, so it only shows its
//! tray item.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// What the entry runs.
fn command() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding the app")?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

#[cfg(target_os = "linux")]
fn entry_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| crate::paths::home_dir().map(|h| h.join(".config")))?;
    Some(config.join("autostart/cww-app.desktop"))
}

#[cfg(target_os = "macos")]
const LABEL: &str = "com.chatwithwork.cww-app";

#[cfg(target_os = "macos")]
fn entry_path() -> Option<PathBuf> {
    crate::paths::home_dir().map(|h| h.join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_VALUE: &str = "ChatWithWorkLocalAgent";

pub fn is_enabled() -> bool {
    #[cfg(unix)]
    {
        entry_path().is_some_and(|p| p.exists())
    }
    #[cfg(windows)]
    {
        windows_registry::read_string(RUN_KEY, RUN_VALUE).is_some()
    }
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    #[cfg(unix)]
    {
        let path = entry_path().context("no home folder")?;
        if !enabled {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(e).with_context(|| format!("removing {}", path.display()))
                }
                _ => Ok(()),
            };
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, entry(&command()?))
            .with_context(|| format!("writing {}", path.display()))
    }
    #[cfg(windows)]
    {
        if enabled {
            let exe = command()?;
            let exe = exe.to_string_lossy();
            // canonicalize() gives a \\?\ path, which the Run key doesn't need.
            let exe = exe.strip_prefix(r"\\?\").unwrap_or(&exe);
            windows_registry::write_string(RUN_KEY, RUN_VALUE, &format!("\"{exe}\" --background"))
        } else {
            windows_registry::delete_value(RUN_KEY, RUN_VALUE)
        }
    }
}

/// The desktop entry (Linux) or property list (macOS) that starts `exe`.
#[cfg_attr(windows, allow(dead_code))]
fn entry(exe: &Path) -> String {
    #[cfg(target_os = "macos")]
    {
        let escape = |s: &str| {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>--background</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
  <key>ProcessType</key>
  <string>Interactive</string>
</dict>
</plist>
"#,
            exe = escape(&exe.to_string_lossy())
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Chat with Work Local Agent\n\
             Comment=Share folders you choose with Chat with Work\n\
             Exec={} --background\n\
             Icon=cww-app\n\
             Terminal=false\n\
             NoDisplay=true\n\
             X-GNOME-Autostart-enabled=true\n",
            desktop_exec_arg(&exe.to_string_lossy())
        )
    }
}

/// Quote a program path for a desktop entry's `Exec` key.
#[cfg(not(target_os = "macos"))]
#[cfg_attr(windows, allow(dead_code))]
fn desktop_exec_arg(arg: &str) -> String {
    let plain = arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+,".contains(c));
    if plain {
        return arg.to_string();
    }
    let mut quoted = String::from('"');
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        if c == '%' {
            quoted.push('%');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

#[cfg(windows)]
pub mod windows_registry {
    //! The few registry calls the app needs, all under HKEY_CURRENT_USER.

    use anyhow::{Result, bail};
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_SZ, RRF_RT_REG_DWORD,
        RRF_RT_REG_SZ, RegCloseKey, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn read_dword(key: &str, value: &str) -> Option<u32> {
        let (key, value) = (wide(key), wide(value));
        let mut data: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let mut kind = REG_DWORD;
        // SAFETY: the buffers outlive the call and `size` describes `data`.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                &mut kind,
                (&mut data as *mut u32).cast(),
                &mut size,
            )
        };
        (status == ERROR_SUCCESS).then_some(data)
    }

    pub fn read_string(key: &str, value: &str) -> Option<String> {
        let (key, value) = (wide(key), wide(value));
        let mut buf = vec![0u16; 2048];
        let mut size = (buf.len() * 2) as u32;
        let mut kind = REG_SZ;
        // SAFETY: the buffers outlive the call and `size` is `buf` in bytes.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                &mut kind,
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let len = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    fn open(key: &str, access: u32) -> Result<HKEY> {
        let key = wide(key);
        let mut handle: HKEY = std::ptr::null_mut();
        // SAFETY: `key` is NUL-terminated and `handle` receives the key.
        let status =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, access, &mut handle) };
        if status != ERROR_SUCCESS {
            bail!("opening the registry key failed ({status})");
        }
        Ok(handle)
    }

    pub fn write_string(key: &str, value: &str, data: &str) -> Result<()> {
        let handle = open(key, KEY_SET_VALUE)?;
        let (value, data) = (wide(value), wide(data));
        // SAFETY: `data` is NUL-terminated UTF-16 and its byte length is passed.
        let status = unsafe {
            RegSetValueExW(
                handle,
                value.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        // SAFETY: `handle` came from RegOpenKeyExW.
        unsafe { RegCloseKey(handle) };
        if status != ERROR_SUCCESS {
            bail!("writing the registry value failed ({status})");
        }
        Ok(())
    }

    pub fn delete_value(key: &str, value: &str) -> Result<()> {
        let handle = open(key, KEY_SET_VALUE | KEY_READ)?;
        let value = wide(value);
        // SAFETY: `value` is NUL-terminated.
        let status = unsafe { RegDeleteValueW(handle, value.as_ptr()) };
        // SAFETY: `handle` came from RegOpenKeyExW.
        unsafe { RegCloseKey(handle) };
        // ERROR_FILE_NOT_FOUND: it wasn't there, which is what we wanted.
        if status != ERROR_SUCCESS && status != 2 {
            bail!("removing the registry value failed ({status})");
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_entries_quote_the_program() {
        assert_eq!(desktop_exec_arg("/usr/bin/cww-app"), "/usr/bin/cww-app");
        assert_eq!(
            desktop_exec_arg("/home/u/My Apps/cww-app"),
            "\"/home/u/My Apps/cww-app\""
        );
        assert_eq!(desktop_exec_arg("/a/$b"), "\"/a/\\$b\"");
        let text = entry(Path::new("/usr/bin/cww-app"));
        assert!(
            text.contains("Exec=/usr/bin/cww-app --background\n"),
            "{text}"
        );
        assert!(text.starts_with("[Desktop Entry]\n"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launch_agents_run_the_app_in_the_background() {
        let text = entry(Path::new("/Applications/A & B.app/Contents/MacOS/cww-app"));
        assert!(
            text.contains("<string>/Applications/A &amp; B.app/Contents/MacOS/cww-app</string>")
        );
        assert!(text.contains("<string>--background</string>"));
    }
}
