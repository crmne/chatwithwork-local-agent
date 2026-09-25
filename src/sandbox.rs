//! The kernel sandbox around the daemon.
//!
//! Path resolution already keeps every tool call inside the shared folders
//! (see `reader::safe_fs`). The sandbox is the second line: if that code had
//! a bug, or a document parser were exploited, the kernel still refuses to
//! open anything outside the shared folders, cww's own directories, and the
//! system files a process needs to run.
//!
//! - **Linux:** Landlock. The worker may read the shared folders and system
//!   paths (`/etc`, `/usr`, `/lib`, `/nix/store`), and write only cww's own
//!   directories. Landlock applies to the calling thread and its children,
//!   so it is set up before the runtime starts any thread.
//! - **macOS:** Seatbelt (`sandbox_init`), with the same file rules plus the
//!   Mach services the daemon needs: DNS, FSEvents, and the keychain when the
//!   device key lives there.
//! - **Windows:** not yet (a restricted token or AppContainer is planned).
//!
//! A sandbox can't be widened once applied, even across `exec`. So `cww
//! daemon run` is a small unconfined supervisor that runs the confined
//! daemon as a child and starts it again, with new rules, when a folder is
//! shared that the current rules don't cover (exit code [`RESTART_CODE`]).

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Serialize;

use crate::paths::Paths;

/// The worker exits with this code (EX_TEMPFAIL) to be started again.
pub const RESTART_CODE: i32 = 75;

/// What the sandbox allows, decided before it is applied.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Shared folders: read only.
    pub roots: Vec<PathBuf>,
    /// cww's own directories: read and write.
    pub own: Vec<PathBuf>,
    /// The control socket, which the daemon binds and accepts on.
    pub socket: PathBuf,
    /// Whether the device key lives in the OS keychain (macOS needs the
    /// keychain's files and services then).
    pub keychain: bool,
}

impl Plan {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>, paths: &Paths, keychain: bool) -> Self {
        let mut own: Vec<PathBuf> = paths.all_dirs().iter().map(|p| p.to_path_buf()).collect();
        // A long runtime directory moves the socket to a short one elsewhere.
        if let Some(dir) = paths.socket_path().parent()
            && !own.iter().any(|o| dir.starts_with(o))
        {
            own.push(dir.to_path_buf());
        }
        Self {
            roots: roots.into_iter().collect(),
            own,
            socket: paths.socket_path(),
            keychain,
        }
    }

    /// Whether every one of `roots` is readable under this plan.
    pub fn covers(&self, roots: &[PathBuf]) -> bool {
        roots
            .iter()
            .all(|root| self.roots.iter().any(|allowed| root.starts_with(allowed)))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Status {
    /// `landlock`, `seatbelt`, or `none`.
    pub kind: &'static str,
    /// `enforced`, `partial` (an older kernel enforces some of the rules),
    /// `off` (turned off in the config), or `unavailable`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

static STATUS: OnceLock<Status> = OnceLock::new();
static PLAN: OnceLock<Plan> = OnceLock::new();

/// How this process is confined, for `status`.
pub fn status() -> Status {
    STATUS.get().cloned().unwrap_or(Status {
        kind: "none",
        state: "off",
        detail: None,
    })
}

/// The plan this process was confined with, if it was.
pub fn plan() -> Option<&'static Plan> {
    PLAN.get()
}

/// Record that the sandbox was turned off.
pub fn disabled(reason: &str) {
    let _ = STATUS.set(Status {
        kind: "none",
        state: "off",
        detail: Some(reason.to_string()),
    });
}

/// Confine this process. Must run before any other thread starts.
pub fn confine(plan: Plan) -> Status {
    let status = sys::confine(&plan);
    let _ = PLAN.set(plan);
    let _ = STATUS.set(status.clone());
    status
}

/// Read-only system locations a process may need: the dynamic loader's
/// libraries, DNS and TLS configuration.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn existing(paths: &[&str]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect()
}

#[cfg(target_os = "linux")]
mod sys {
    use super::*;
    use landlock::{
        ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
        path_beneath_rules,
    };

    pub fn confine(plan: &Plan) -> Status {
        match apply(plan) {
            Ok(RulesetStatus::FullyEnforced) => Status {
                kind: "landlock",
                state: "enforced",
                detail: None,
            },
            Ok(RulesetStatus::PartiallyEnforced) => Status {
                kind: "landlock",
                state: "partial",
                detail: Some("this kernel enforces an older set of Landlock rules".into()),
            },
            Ok(RulesetStatus::NotEnforced) => Status {
                kind: "landlock",
                state: "unavailable",
                detail: Some("Landlock is not enabled in this kernel".into()),
            },
            Err(e) => Status {
                kind: "landlock",
                state: "unavailable",
                detail: Some(e),
            },
        }
    }

    fn apply(plan: &Plan) -> Result<RulesetStatus, String> {
        let abi = ABI::V5;
        // System paths may be executed (the dynamic loader, NSS modules);
        // shared folders and cww's own directories never are.
        let read = AccessFs::from_read(abi);
        let all = AccessFs::from_all(abi);
        let read_data = AccessFs::ReadFile | AccessFs::ReadDir;
        let own = all & !AccessFs::Execute;
        let system = existing(&[
            "/etc",
            "/usr",
            "/lib",
            "/lib64",
            "/lib32",
            "/nix/store",
            "/run/current-system",
            "/proc/self",
            "/sys/fs/cgroup",
            "/sys/devices/system/cpu",
            "/dev/null",
            "/dev/urandom",
        ]);
        let status = Ruleset::default()
            .handle_access(all)
            .and_then(|r| r.create())
            .and_then(|r| r.add_rules(path_beneath_rules(&system, read)))
            .and_then(|r| r.add_rules(path_beneath_rules(existing_paths(&plan.roots), read_data)))
            .and_then(|r| r.add_rules(path_beneath_rules(existing_paths(&plan.own), own)))
            .and_then(|r| r.restrict_self())
            .map_err(|e| format!("{e}"))?;
        Ok(status.ruleset)
    }

    fn existing_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
        paths.iter().filter(|p| p.exists()).cloned().collect()
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use super::*;
    use std::ffi::{CStr, CString, c_char, c_int};
    use std::path::Path;

    unsafe extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(errorbuf: *mut c_char);
    }

    pub fn confine(plan: &Plan) -> Status {
        let profile = match CString::new(profile(plan)) {
            Ok(p) => p,
            Err(_) => {
                return Status {
                    kind: "seatbelt",
                    state: "unavailable",
                    detail: Some("a path contains a NUL byte".into()),
                };
            }
        };
        let mut error: *mut c_char = std::ptr::null_mut();
        // SAFETY: the profile is a valid C string; on failure the error
        // buffer is freed with the matching call.
        let result = unsafe { sandbox_init(profile.as_ptr(), 0, &mut error) };
        if result == 0 {
            return Status {
                kind: "seatbelt",
                state: "enforced",
                detail: None,
            };
        }
        let detail = if error.is_null() {
            "sandbox_init failed".to_string()
        } else {
            // SAFETY: sandbox_init set a NUL-terminated message.
            let message = unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(error) };
            message
        };
        Status {
            kind: "seatbelt",
            state: "unavailable",
            detail: Some(detail),
        }
    }

    /// A Seatbelt string literal.
    fn quote(path: &Path) -> String {
        let s = path.to_string_lossy();
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }

    fn subpaths(paths: &[PathBuf]) -> String {
        paths
            .iter()
            .map(|p| format!(" (subpath {})", quote(p)))
            .collect()
    }

    pub fn profile(plan: &Plan) -> String {
        let system = existing(&[
            "/System",
            "/usr/lib",
            "/usr/share",
            "/Library/Preferences",
            "/private/var/db/timezone",
            "/private/etc",
            "/private/var/run/resolv.conf",
            "/dev",
        ]);
        let mut rules = String::from(
            "(version 1)\n\
             (deny default)\n\
             (allow process-info* (target self))\n\
             (allow signal (target self))\n\
             (allow sysctl-read)\n\
             (allow system-info)\n\
             ; stat() anywhere: needed to walk to a shared folder; reveals no contents.\n\
             (allow file-read-metadata)\n\
             (allow file-read* (literal \"/\"))\n\
             (allow file-write* (literal \"/dev/null\"))\n\
             (allow network-outbound)\n\
             (allow system-socket)\n\
             (allow mach-lookup\n\
               (global-name \"com.apple.dnssd.service\")\n\
               (global-name \"com.apple.system.opendirectoryd.libinfo\")\n\
               (global-name \"com.apple.system.notification_center\")\n\
               (global-name \"com.apple.system.logger\")\n\
               (global-name \"com.apple.logd\")\n\
               (global-name \"com.apple.FSEvents\")\n\
               (global-name \"com.apple.cfprefsd.daemon\")\n\
               (global-name \"com.apple.cfprefsd.agent\"))\n\
             (allow ipc-posix-shm-read-data (ipc-posix-name \"apple.shm.notification_center\"))\n",
        );
        rules.push_str(&format!("(allow file-read*{})\n", subpaths(&system)));
        if !plan.roots.is_empty() {
            rules.push_str(&format!("(allow file-read*{})\n", subpaths(&plan.roots)));
        }
        rules.push_str(&format!(
            "(allow file-read* file-write*{})\n",
            subpaths(&plan.own)
        ));
        rules.push_str(&format!(
            "(allow network-bind network-inbound (local unix-socket (path-literal {})))\n",
            quote(&plan.socket)
        ));
        if plan.keychain {
            if let Ok(home) = crate::paths::home_dir() {
                rules.push_str(&format!(
                    "(allow file-read* file-write* (subpath {}))\n",
                    quote(&home.join("Library/Keychains"))
                ));
            }
            // The keychain checks who is asking by reading the calling
            // binary, and reads a few preferences.
            if let Ok(exe) = std::env::current_exe() {
                let mut own = vec![exe.clone()];
                if let Ok(real) = exe.canonicalize() {
                    own.push(real);
                }
                for path in own {
                    rules.push_str(&format!("(allow file-read* (literal {}))\n", quote(&path)));
                    if let Some(dir) = path.parent() {
                        rules.push_str(&format!("(allow file-read* (literal {}))\n", quote(dir)));
                    }
                }
            }
            rules.push_str(
                "(allow mach-lookup\n\
                   (global-name \"com.apple.SecurityServer\")\n\
                   (global-name \"com.apple.securityd.xpc\")\n\
                   (global-name \"com.apple.security.agent\")\n\
                   (global-name \"com.apple.trustd.agent\")\n\
                   (global-name \"com.apple.ocspd\")\n\
                   (global-name \"com.apple.bsd.dirhelper\")\n\
                   (global-name \"com.apple.system.opendirectoryd.membership\")\n\
                   (global-name \"com.apple.CoreServices.coreservicesd\"))\n\
                 (allow user-preference-read)\n\
                 (allow ipc-posix-shm-read-data (ipc-posix-name-prefix \"apple.cfprefs.\"))\n",
            );
        }
        rules
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod sys {
    use super::*;

    pub fn confine(_plan: &Plan) -> Status {
        Status {
            kind: "none",
            state: "unavailable",
            detail: Some("no sandbox on this platform yet".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_cover_folders_inside_their_roots() {
        let plan = Plan {
            roots: vec!["/home/u/Documents".into()],
            ..Plan::default()
        };
        assert!(plan.covers(&["/home/u/Documents".into()]));
        assert!(plan.covers(&["/home/u/Documents/Work".into()]));
        assert!(!plan.covers(&["/home/u/Downloads".into()]));
        assert!(!plan.covers(&["/home/u/Documents2".into()]));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_profile_quotes_paths() {
        let plan = Plan {
            roots: vec![r#"/Users/u/My "odd" folder"#.into()],
            own: vec!["/Users/u/.config/cww".into()],
            socket: "/Users/u/.local/state/cww/cww.sock".into(),
            keychain: true,
        };
        let profile = sys::profile(&plan);
        assert!(profile.contains(r#"(subpath "/Users/u/My \"odd\" folder")"#));
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("com.apple.SecurityServer"));
    }
}
