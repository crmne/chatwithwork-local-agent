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

/// `name` with a number added before its extension, for the `n`th try:
/// `notes.md`, then `notes.2.md` (Linux, as GNOME does) or `notes 2.md`
/// (macOS, as Finder does). The result fits in `max` bytes.
#[cfg(unix)]
fn numbered(name: &str, n: u32, is_dir: bool, max: usize) -> String {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !is_dir && !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    let suffix = if n == 0 {
        String::new()
    } else if cfg!(target_os = "macos") {
        format!(" {}", n + 1)
    } else {
        format!(".{}", n + 1)
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
    fn long_names_are_cut_to_fit() {
        let long = format!("{}.txt", "é".repeat(200));
        let name = numbered(&long, 12, false, 245);
        assert!(name.len() <= 245, "{}", name.len());
        assert!(name.ends_with(".txt"));
    }
}
