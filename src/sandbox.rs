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
//!   directories, the folders that allow changes (create, write, rename and
//!   remove, never execute), and the trash directories their files go to.
//!   Landlock applies to the calling thread and its children, so it is set
//!   up before the runtime starts any thread.
//! - **macOS:** Seatbelt (`sandbox_init`), with the same file rules plus the
//!   Mach services the daemon needs: DNS, FSEvents, and the keychain when the
//!   device key lives there.
//! - **Windows:** not yet (a restricted token or AppContainer is planned).
//!
//! A sandbox can't be widened once applied, even across `exec`. So `cww
//! daemon run` is a small unconfined supervisor that runs the confined
//! daemon as a child and starts it again, with new rules, when a folder is
//! shared that the current rules don't cover, or when the set of folders
//! that allow changes changes (exit code [`RESTART_CODE`]).

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
    /// Shared folders that allow changes: read and write.
    pub writable: Vec<PathBuf>,
    /// The trash directories the writable folders' files go to: write.
    pub trash: Vec<PathBuf>,
    /// Folders shared read-only inside ones that allow changes. Seatbelt
    /// takes their write rights away again; Landlock can't (its rules only
    /// add rights), so there the writer's checks are what hold.
    pub read_only_inside: Vec<PathBuf>,
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
            writable: Vec::new(),
            trash: Vec::new(),
            read_only_inside: Vec::new(),
            own,
            socket: paths.socket_path(),
            keychain,
        }
    }

    /// Also allow changes in `writable`, whose files go to `trash`.
    pub fn with_changes(mut self, writable: Vec<PathBuf>, trash: Vec<PathBuf>) -> Self {
        self.read_only_inside = self
            .roots
            .iter()
            .filter(|r| !writable.contains(r) && writable.iter().any(|w| r.starts_with(w)))
            .cloned()
            .collect();
        self.writable = writable;
        self.trash = trash;
        self
    }

    /// Whether every one of `roots` is readable under this plan, and the
    /// folders it lets the daemon change are exactly `writable`: a folder
    /// that stopped allowing changes loses its write rights too.
    pub fn covers(&self, roots: &[PathBuf], writable: &[PathBuf]) -> bool {
        let mut want = writable.to_vec();
        want.sort();
        want.dedup();
        let mut have = self.writable.clone();
        have.sort();
        have.dedup();
        // A folder newly shared read-only inside one that allows changes
        // needs a sandbox that keeps it read-only.
        let nested_covered = roots
            .iter()
            .filter(|r| !writable.contains(r) && writable.iter().any(|w| r.starts_with(w)))
            .all(|r| self.read_only_inside.contains(r));
        want == have
            && nested_covered
            && roots
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
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn existing(paths: &[&str]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect()
}

/// The directory `/etc/resolv.conf` really lives in. It is often a link out
/// of `/etc`: systemd-resolved and NetworkManager keep it under `/run`.
/// The static Linux builds read it themselves (musl has no NSS), so without
/// it every name fails to resolve. The whole directory, because its owner
/// replaces the file by renaming a new one over it.
#[cfg(target_os = "linux")]
fn resolver_dir(resolv_conf: &std::path::Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(resolv_conf).ok()?;
    real.parent().map(PathBuf::from)
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
        // Changes: make and replace files and folders, rename across
        // folders (Refer) and remove. Never execute, make devices, FIFOs,
        // sockets or symlinks, or use ioctls.
        let change = AccessFs::WriteFile
            | AccessFs::MakeReg
            | AccessFs::MakeDir
            | AccessFs::RemoveFile
            | AccessFs::RemoveDir
            | AccessFs::Refer
            | AccessFs::Truncate;
        // The trash only takes items in: a subset of a changeable folder's
        // rights, as Landlock requires of a rename's destination.
        let trash = AccessFs::WriteFile
            | AccessFs::MakeReg
            | AccessFs::MakeDir
            | AccessFs::RemoveFile
            | AccessFs::Refer;
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
        let system: Vec<PathBuf> = system
            .into_iter()
            .chain(resolver_dir(std::path::Path::new("/etc/resolv.conf")))
            .collect();
        let status = Ruleset::default()
            .handle_access(all)
            .and_then(|r| r.create())
            .and_then(|r| r.add_rules(path_beneath_rules(&system, read)))
            .and_then(|r| r.add_rules(path_beneath_rules(existing_paths(&plan.roots), read_data)))
            .and_then(|r| {
                r.add_rules(path_beneath_rules(
                    existing_paths(&plan.writable),
                    read_data | change,
                ))
            })
            .and_then(|r| r.add_rules(path_beneath_rules(existing_paths(&plan.trash), trash)))
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
    use super::seatbelt::profile;
    use super::*;
    use std::ffi::{CStr, CString, c_char, c_int};

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
}

/// The Seatbelt write operations a change needs.
#[cfg(any(target_os = "macos", test))]
const CHANGE_WRITES: &str = "file-write-create file-write-data file-write-unlink \
                             file-write-mode file-write-xattr file-write-owner";

#[cfg(any(target_os = "macos", test))]
mod seatbelt {
    use super::*;
    use std::path::Path;

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

    /// The Seatbelt profile for `plan`. Built on every platform, so its
    /// tests run everywhere; applied on macOS.
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
        if !plan.writable.is_empty() {
            // Only the writes a change makes: create (and rename in), write
            // data, remove (and rename out), set the mode (never the setuid
            // or setgid bits: that is `file-write-setugid`), set extended
            // attributes (the quarantine mark, and those a replaced file
            // had), and give a replaced file its group back
            // (`file-write-owner`; only a group the user is in). Never
            // flags, times, ACLs or mounts.
            rules.push_str(&format!(
                "(allow file-read* {CHANGE_WRITES}{})\n",
                subpaths(&plan.writable)
            ));
        }
        if !plan.read_only_inside.is_empty() {
            // Later rules win: a folder shared read-only stays read-only to
            // the kernel too, inside a folder that allows changes.
            rules.push_str(&format!(
                "(deny file-write*{})\n",
                subpaths(&plan.read_only_inside)
            ));
        }
        if !plan.trash.is_empty() {
            // Items are renamed into the trash; nothing there is read. The
            // trash directory itself is opened (not listed) to rename
            // relative to it.
            rules.push_str(&format!(
                "(allow file-write-create{})\n",
                subpaths(&plan.trash)
            ));
            // A volume's `.Trashes/<uid>` is opened from the volume's root
            // down, one directory at a time.
            let mut opened: Vec<&Path> = Vec::new();
            for trash in &plan.trash {
                opened.push(trash);
                if let Some(trashes) = trash.parent()
                    && trashes.file_name().is_some_and(|n| n == ".Trashes")
                {
                    opened.push(trashes);
                    opened.extend(trashes.parent());
                }
            }
            let literals: String = opened
                .iter()
                .map(|p| format!(" (literal {})", quote(p)))
                .collect();
            rules.push_str(&format!("(allow file-read-data{literals})\n"));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn follows_resolv_conf_to_the_directory_it_lives_in() {
        let run = tempfile::tempdir().unwrap();
        let etc = tempfile::tempdir().unwrap();
        let stub = run.path().join("stub-resolv.conf");
        std::fs::write(&stub, "nameserver 127.0.0.53\n").unwrap();
        let link = etc.path().join("resolv.conf");
        std::os::unix::fs::symlink(&stub, &link).unwrap();
        assert_eq!(
            resolver_dir(&link),
            Some(std::fs::canonicalize(run.path()).unwrap())
        );
        assert_eq!(resolver_dir(&etc.path().join("missing")), None);
    }

    #[test]
    fn plans_cover_folders_inside_their_roots() {
        let plan = Plan {
            roots: vec!["/home/u/Documents".into()],
            ..Plan::default()
        };
        assert!(plan.covers(&["/home/u/Documents".into()], &[]));
        assert!(plan.covers(&["/home/u/Documents/Work".into()], &[]));
        assert!(!plan.covers(&["/home/u/Downloads".into()], &[]));
        assert!(!plan.covers(&["/home/u/Documents2".into()], &[]));
    }

    /// Seatbelt gives changes only the writes they make, and takes them
    /// away again from folders shared read-only inside.
    #[test]
    fn seatbelt_keeps_read_only_folders_inside_read_only() {
        let docs = PathBuf::from("/Users/u/Documents");
        let private = docs.join("Private");
        let plan = Plan {
            roots: vec![docs.clone(), private.clone(), "/Users/u/Elsewhere".into()],
            ..Plan::default()
        }
        .with_changes(vec![docs.clone()], vec!["/Users/u/.Trash".into()]);
        assert_eq!(plan.read_only_inside, [private]);
        let profile = seatbelt::profile(&plan);
        let allow = profile.find(r#"(subpath "/Users/u/Documents"))"#).unwrap();
        let deny = profile
            .find(r#"(deny file-write* (subpath "/Users/u/Documents/Private"))"#)
            .unwrap();
        assert!(deny > allow, "the deny comes after the allow, so it wins");
        for broad in ["file-write-flags", "file-write-setugid", "file-write-times"] {
            assert!(!profile.contains(broad), "{broad}");
        }
        // Writes are listed one by one where changes are allowed.
        assert!(
            !profile.contains(r#"(allow file-read* file-write* (subpath "/Users/u/Documents"))"#)
        );
    }

    #[test]
    fn plans_cover_exactly_the_folders_that_allow_changes() {
        let docs = PathBuf::from("/home/u/Documents");
        let plan = Plan {
            roots: vec![docs.clone()],
            ..Plan::default()
        };
        assert!(!plan.covers(std::slice::from_ref(&docs), std::slice::from_ref(&docs)));
        let plan = plan.with_changes(
            vec![docs.clone()],
            vec!["/home/u/.local/share/Trash".into()],
        );
        assert!(plan.covers(std::slice::from_ref(&docs), std::slice::from_ref(&docs)));
        // Stopping changes needs a new sandbox too.
        assert!(!plan.covers(std::slice::from_ref(&docs), &[]));
        // So does a folder shared read-only inside one that allows changes.
        let inner = docs.join("Private");
        assert!(!plan.covers(&[docs.clone(), inner.clone()], std::slice::from_ref(&docs)));
        let plan = Plan {
            roots: vec![docs.clone(), inner.clone()],
            ..Plan::default()
        }
        .with_changes(vec![docs.clone()], Vec::new());
        assert!(plan.covers(&[docs.clone(), inner], std::slice::from_ref(&docs)));
    }

    #[test]
    fn seatbelt_profile_quotes_paths() {
        let plan = Plan {
            roots: vec![r#"/Users/u/My "odd" folder"#.into()],
            writable: vec!["/Users/u/Drafts".into()],
            trash: vec!["/Users/u/.Trash".into()],
            read_only_inside: Vec::new(),
            own: vec!["/Users/u/.config/cww".into()],
            socket: "/Users/u/.local/state/cww/cww.sock".into(),
            keychain: true,
        };
        let profile = seatbelt::profile(&plan);
        assert!(profile.contains(r#"(subpath "/Users/u/My \"odd\" folder")"#));
        assert!(profile.contains(&format!(
            r#"(allow file-read* {CHANGE_WRITES} (subpath "/Users/u/Drafts"))"#
        )));
        assert!(profile.contains(r#"(subpath "/Users/u/.Trash"))"#));
        assert!(profile.contains(r#"(allow file-read-data (literal "/Users/u/.Trash"))"#));
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("com.apple.SecurityServer"));
    }
}
