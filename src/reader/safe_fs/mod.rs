//! Safe path parsing and resolution.
//!
//! Tool paths are always `root_id:relative/path`. They are parsed and
//! rejected (absolute paths, `..`, NUL bytes, backslashes, drive letters)
//! before any filesystem access. Resolution then happens relative to an open
//! directory handle for the root, and every check that matters runs on the
//! opened handle, not on a string, which avoids the CVE-2025-53109/53110 class
//! of prefix and symlink escapes:
//!
//! - **Linux:** `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`, plus
//!   `RESOLVE_NO_SYMLINKS` unless the root opts into following symlinks. The
//!   kernel refuses any resolution that leaves the root.
//! - **macOS, and Linux without `openat2`:** walk one component at a time
//!   with `O_NOFOLLOW`, then read the final handle's path (`F_GETPATH` or
//!   `/proc/self/fd`) and require it to be inside the root.
//! - **Windows:** walk one component at a time with
//!   `FILE_FLAG_OPEN_REPARSE_POINT`, refusing symlinks and junctions, then
//!   read the final handle's path (`GetFinalPathNameByHandleW`) and require
//!   it to be inside the root. The deny list is checked against that real
//!   path too, so 8.3 short names can't dodge it.
//!
//! Only regular files are read and only directories are listed. FIFOs,
//! sockets and devices are refused, and so are files with more than one hard
//! link unless the config allows them.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ToolError;
use crate::policy::DenyList;

const MAX_PATH_LEN: usize = 4096;
const MAX_COMPONENT_LEN: usize = 255;

/// A validated path relative to a root. Empty means the root itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RelPath(Vec<String>);

impl RelPath {
    pub fn parse(rel: &str) -> Result<Self, ToolError> {
        if rel.starts_with('/') {
            return Err(ToolError::invalid_path(
                "absolute paths are not allowed; use root_id:relative/path",
            ));
        }
        let mut parts = Vec::new();
        for part in rel.split('/') {
            match part {
                "" | "." => continue,
                ".." => return Err(ToolError::invalid_path("'..' is not allowed in paths")),
                _ if part.len() > MAX_COMPONENT_LEN => {
                    return Err(ToolError::invalid_path("path component is too long"));
                }
                // `name:stream` would open an NTFS alternate data stream.
                _ if cfg!(windows) && part.contains(':') => {
                    return Err(ToolError::invalid_path(
                        "':' is not allowed in file names on Windows",
                    ));
                }
                _ => parts.push(part.to_string()),
            }
        }
        Ok(Self(parts))
    }

    /// Build from a relative path that comes from the filesystem (walker,
    /// watcher), with the platform's separators. Names that aren't UTF-8,
    /// or that a tool path couldn't express, give `None`.
    pub fn from_relative_path(path: &Path) -> Option<Self> {
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                std::path::Component::Normal(name) => {
                    let name = name.to_str()?;
                    if name.len() > MAX_COMPONENT_LEN
                        || name.contains(['/', '\\', '\0'])
                        || name == ".."
                    {
                        return None;
                    }
                    parts.push(name.to_string());
                }
                std::path::Component::CurDir => {}
                _ => return None,
            }
        }
        Some(Self(parts))
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn components(&self) -> &[String] {
        &self.0
    }

    pub fn file_name(&self) -> Option<&str> {
        self.0.last().map(String::as_str)
    }

    pub fn join(&self, name: &str) -> Self {
        let mut parts = self.0.clone();
        parts.push(name.to_string());
        Self(parts)
    }

    pub fn as_string(&self) -> String {
        self.0.join("/")
    }

    #[cfg(unix)]
    fn fs_path(&self) -> PathBuf {
        if self.0.is_empty() {
            PathBuf::from(".")
        } else {
            self.0.iter().collect()
        }
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_string())
    }
}

/// A parsed `root_id:relative/path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPath {
    pub root_id: String,
    pub rel: RelPath,
}

impl ToolPath {
    pub fn parse(input: &str) -> Result<Self, ToolError> {
        if input.len() > MAX_PATH_LEN {
            return Err(ToolError::invalid_path("path is too long"));
        }
        if input.contains('\0') {
            return Err(ToolError::invalid_path(
                "NUL bytes are not allowed in paths",
            ));
        }
        if input.contains('\\') {
            return Err(ToolError::invalid_path(
                "backslashes are not allowed in paths; use '/'",
            ));
        }
        if input.starts_with('/') || input.starts_with('~') {
            return Err(ToolError::invalid_path(
                "absolute paths are not allowed; use root_id:relative/path",
            ));
        }
        let Some((root_id, rel)) = input.split_once(':') else {
            return Err(ToolError::invalid_path(
                "paths look like root_id:relative/path; call roots for the IDs",
            ));
        };
        if !is_valid_root_id(root_id) {
            return Err(ToolError::invalid_path(format!(
                "{root_id:?} is not a root ID; call roots for the IDs"
            )));
        }
        Ok(Self {
            root_id: root_id.to_string(),
            rel: RelPath::parse(rel)?,
        })
    }

    pub fn display(root_id: &str, rel: &RelPath) -> String {
        format!("{root_id}:{rel}")
    }
}

impl std::fmt::Display for ToolPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.root_id, self.rel)
    }
}

/// Root IDs are 1 to 32 characters of `[a-z0-9_-]`, starting with a letter or
/// digit. Single letters are refused so `C:` can never look like a root.
pub fn is_valid_root_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (2..=32).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// What a caller wants at the end of a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    File,
    Dir,
}

/// How to resolve paths. `Auto` picks `openat2` where the kernel has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Auto,
    /// Component-by-component walk with `O_NOFOLLOW` and a final handle check.
    Walk,
}

/// Rules applied to every open, on top of staying inside the root.
#[derive(Debug, Clone, Copy)]
pub struct OpenPolicy<'a> {
    pub deny: &'a DenyList,
    pub allow_hardlinks: bool,
}

/// An open handle on a shared root.

#[derive(Debug)]
pub struct RootHandle {
    pub id: String,
    pub label: String,
    /// Canonical absolute path. Used for the deny list, the walker and
    /// handle verification. Never sent to the server.
    pub path: PathBuf,
    pub follow_symlinks: bool,
    dir: sys::DirHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug, Clone)]
pub struct DirEntryInfo {
    pub name: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub modified: Option<i64>,
}

pub struct OpenedFile {
    pub file: File,
    pub size: u64,
    pub modified: i64,
    /// `(device, inode)`, used as a cache key.
    pub identity: (u64, u64),
}

impl RootHandle {
    /// Absolute local path for `rel`. Local use only (deny list, logs).
    pub fn abs_path(&self, rel: &RelPath) -> PathBuf {
        let mut path = self.path.clone();
        path.extend(rel.components());
        path
    }

    pub fn open_file(
        &self,
        rel: &RelPath,
        policy: OpenPolicy<'_>,
    ) -> Result<OpenedFile, ToolError> {
        self.open_file_with(rel, policy, Strategy::Auto)
    }
}

/// Component-wise containment: `/a/root2` is not inside `/a/root`.
pub fn is_within(root: &Path, candidate: &Path) -> bool {
    candidate.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn parses_tool_paths() {
        let p = ToolPath::parse("docs:a/./b//c.txt").unwrap();
        assert_eq!(p.root_id, "docs");
        assert_eq!(p.rel.as_string(), "a/b/c.txt");
        assert!(ToolPath::parse("docs:").unwrap().rel.is_root());
        for bad in [
            "/etc/passwd",
            "~/secret",
            "docs:../x",
            "docs:a/../../x",
            "docs:/etc/passwd",
            "docs:a\0b",
            "C:\\Windows\\system.ini",
            "c:/Windows",
            "no-colon",
            "UPPER:x",
            "",
        ] {
            let err = ToolPath::parse(bad).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidPath, "{bad:?}");
        }
    }

    #[test]
    fn prefix_containment_is_component_wise() {
        assert!(is_within(Path::new("/a/root"), Path::new("/a/root/x")));
        assert!(is_within(Path::new("/a/root"), Path::new("/a/root")));
        assert!(!is_within(Path::new("/a/root"), Path::new("/a/root2/x")));
        assert!(!is_within(Path::new("/a/root"), Path::new("/a/roo")));
    }
}
