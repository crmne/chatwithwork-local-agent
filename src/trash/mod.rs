//! The system trash, where every deleted file and every replaced version
//! goes. cww never deletes a user's file for good: if the trash can't take
//! it, the change is refused.
//!
//! - **Linux:** the freedesktop.org trash. Files on the filesystem of the
//!   home trash (`$XDG_DATA_HOME/Trash`) go there; files on other
//!   filesystems go to `$topdir/.Trash/$uid` when the administrator set up a
//!   sticky `.Trash`, otherwise to `$topdir/.Trash-$uid`. Each item gets a
//!   `.trashinfo` record, so file managers can put it back.
//! - **macOS:** `~/.Trash` for the home volume, `<volume>/.Trashes/$uid`
//!   for others. Items are moved in place (no AppleScript, no Finder), which
//!   works under Seatbelt; Finder shows them, without "Put Back".
//! - **Windows:** the Recycle Bin of the item's drive
//!   (`<drive>\$Recycle.Bin\<user SID>`), written the way Explorer writes
//!   it (`$R` item plus `$I` record), so Restore works.
//!
//! Items are moved with a rename relative to an open handle on their
//! folder, never copied and deleted. An item on a different filesystem than
//! its trash (a mount inside a shared folder) is refused.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::path::PathBuf;
use std::sync::OnceLock;

use time::UtcOffset;

use crate::error::{ErrorCode, ToolError};

#[cfg(unix)]
pub use unix::prepare;
#[cfg(windows)]
pub use windows::prepare;

/// Where the system trash is for files in the home folder's filesystem:
/// `$XDG_DATA_HOME/Trash` on Linux and other Unix, `~/.Trash` on macOS.
/// `None` on Windows, where every drive has its own Recycle Bin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trash {
    home: Option<PathBuf>,
}

impl Trash {
    pub fn new(home: Option<PathBuf>) -> Self {
        Self { home }
    }

    pub fn home(&self) -> Option<&std::path::Path> {
        self.home.as_deref()
    }
}

/// The local time offset, worked out while the process has one thread
/// (`time` refuses to later). Trash records use local times.
static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

/// Remember the local time offset. Call before starting threads.
pub fn remember_local_offset() {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let _ = LOCAL_OFFSET.set(offset);
}

#[cfg(unix)]
fn local_offset() -> UtcOffset {
    LOCAL_OFFSET.get().copied().unwrap_or(UtcOffset::UTC)
}

fn unavailable(detail: impl std::fmt::Display) -> ToolError {
    tracing::warn!("the trash is unavailable: {detail}");
    ToolError::new(
        ErrorCode::TrashUnavailable,
        "The system trash can't take this item, so nothing was changed. cww never deletes \
         a file for good.",
    )
}

/// The well-known SIDs Explorer's Recycle Bin folders belong to, besides
/// the user's own: `SYSTEM` and `Administrators`.
const BIN_ADMINS: [&str; 2] = ["S-1-5-18", "S-1-5-32-544"];

/// Rights anyone may have on a Recycle Bin folder without seeing or
/// changing what is in it: synchronize, read its permissions, read its
/// attributes and extended attributes, traverse it.
const HARMLESS_RIGHTS: u32 = 0x0010_0000 | 0x0002_0000 | 0x0080 | 0x0008 | 0x0020;

/// Whether a Recycle Bin folder is the user's own, as Explorer makes it:
/// owned by the user (or by `SYSTEM` or `Administrators`), with a DACL that
/// gives no one else any right to list, read, add, change or delete what
/// is in it. `grants` is the DACL's allow entries as `(SID, rights)`, or
/// `None` for a null DACL; `unusual` says it has entries this check can't
/// read. Pure, so it is tested on every platform.
#[cfg_attr(not(windows), allow(dead_code))]
fn check_private_bin(
    user: &str,
    owner: &str,
    grants: Option<&[(String, u32)]>,
    unusual: bool,
) -> Result<(), String> {
    let trusted = |sid: &str| sid == user || BIN_ADMINS.contains(&sid);
    if !trusted(owner) {
        return Err(format!("it belongs to {owner}"));
    }
    let Some(grants) = grants else {
        return Err("anyone may use it (it has no access list)".into());
    };
    if unusual {
        return Err("its access list has entries cww can't check".into());
    }
    if let Some((sid, _)) = grants
        .iter()
        .find(|(sid, rights)| !trusted(sid) && rights & !HARMLESS_RIGHTS != 0)
    {
        return Err(format!("{sid} may use it"));
    }
    Ok(())
}

/// `name` with a number added before its extension, for the `n`th try:
/// `notes.md`, then `notes.2.md` (Linux, as GNOME does) or `notes 2.md`
/// (macOS, as Finder does). The result fits in `max` bytes.
#[cfg(unix)]
fn numbered(name: &str, n: u32, is_dir: bool, max: usize) -> String {
    let suffix = if n == 0 {
        String::new()
    } else if cfg!(target_os = "macos") {
        format!(" {}", n + 1)
    } else {
        format!(".{}", n + 1)
    };
    suffixed(name, &suffix, is_dir, max)
}

/// `name` with a random suffix before its extension (`notes.3f9a01c2.md`),
/// for when the numbered names are all taken.
#[cfg(unix)]
fn randomized(name: &str, is_dir: bool, max: usize) -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 4];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let sep = if cfg!(target_os = "macos") { " " } else { "." };
    suffixed(name, &format!("{sep}{hex}"), is_dir, max)
}

/// `name` with `suffix` before its extension, cut to fit in `max` bytes.
#[cfg(unix)]
fn suffixed(name: &str, suffix: &str, is_dir: bool, max: usize) -> String {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !is_dir && !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    let room = max.saturating_sub(suffix.len() + ext.len());
    let mut stem = stem.to_string();
    if stem.len() > room {
        let mut cut = room;
        while !stem.is_char_boundary(cut) {
            cut -= 1;
        }
        stem.truncate(cut);
    }
    let ext = if stem.is_empty() && ext.len() > max.saturating_sub(suffix.len()) {
        String::new()
    } else {
        ext
    };
    format!("{stem}{suffix}{ext}")
}

#[cfg(test)]
mod bin_tests {
    use super::*;

    const ME: &str = "S-1-5-21-1-2-3-1001";

    fn grants(list: &[(&str, u32)]) -> Vec<(String, u32)> {
        list.iter().map(|(s, m)| (s.to_string(), *m)).collect()
    }

    #[test]
    fn accepts_a_recycle_bin_as_explorer_makes_it() {
        let explorer = grants(&[
            ("S-1-5-18", 0x001F_01FF),
            ("S-1-5-32-544", 0x001F_01FF),
            (ME, 0x001F_01FF),
        ]);
        assert_eq!(check_private_bin(ME, ME, Some(&explorer), false), Ok(()));
        assert_eq!(
            check_private_bin(ME, "S-1-5-32-544", Some(&explorer), false),
            Ok(())
        );
        // Traverse and read-attributes for everyone reveal nothing.
        let traverse = grants(&[(ME, 0x001F_01FF), ("S-1-1-0", 0x0010_00A0)]);
        assert_eq!(check_private_bin(ME, ME, Some(&traverse), false), Ok(()));
    }

    #[test]
    fn refuses_a_recycle_bin_others_can_use() {
        let mine = grants(&[(ME, 0x001F_01FF)]);
        let other = "S-1-5-21-1-2-3-1002";
        assert!(check_private_bin(ME, other, Some(&mine), false).is_err());
        assert!(check_private_bin(ME, ME, None, false).is_err());
        assert!(check_private_bin(ME, ME, Some(&mine), true).is_err());
        for (sid, rights) in [
            ("S-1-1-0", 0x0012_0089),  // Everyone: read
            ("S-1-5-32-545", 0x0002),  // Users: add files
            ("S-1-5-11", 0x0001_0000), // Authenticated Users: delete
            (other, 0x1000_0000),      // someone: generic all
        ] {
            let list = grants(&[(ME, 0x001F_01FF), (sid, rights)]);
            assert!(
                check_private_bin(ME, ME, Some(&list), false).is_err(),
                "{sid} {rights:#x}"
            );
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn numbers_go_before_the_extension() {
        assert_eq!(numbered("notes.md", 0, false, 255), "notes.md");
        let second = numbered("notes.md", 1, false, 255);
        let third = numbered("archive.tar.gz", 2, false, 255);
        let folder = numbered("v1.2", 1, true, 255);
        let hidden = numbered(".env", 1, false, 255);
        if cfg!(target_os = "macos") {
            assert_eq!(second, "notes 2.md");
            assert_eq!(third, "archive.tar 3.gz");
            assert_eq!(folder, "v1.2 2");
            assert_eq!(hidden, ".env 2");
        } else {
            assert_eq!(second, "notes.2.md");
            assert_eq!(third, "archive.tar.3.gz");
            assert_eq!(folder, "v1.2.2");
            assert_eq!(hidden, ".env.2");
        }
    }

    #[test]
    fn random_names_keep_the_extension() {
        let a = randomized("notes.md", false, 255);
        let b = randomized("notes.md", false, 255);
        assert_ne!(a, b);
        assert!(a.starts_with("notes") && a.ends_with(".md"), "{a}");
        assert_eq!(a.len(), "notes.12345678.md".len());
        let long = format!("{}.txt", "x".repeat(300));
        assert!(randomized(&long, false, 245).len() <= 245);
    }

    #[test]
    fn long_names_are_cut_to_fit() {
        let long = format!("{}.txt", "é".repeat(200));
        let name = numbered(&long, 12, false, 245);
        assert!(name.len() <= 245, "{}", name.len());
        assert!(name.ends_with(".txt"));
    }
}
