//! Path resolution on Windows.
//!
//! Windows has no `openat`, so resolution walks the path one component at a
//! time, opening each with `FILE_FLAG_OPEN_REPARSE_POINT` so a symlink or
//! junction is seen instead of followed. Name-surrogate reparse points
//! (symlinks, junctions, mount points) are refused. Other reparse points,
//! such as OneDrive's cloud placeholders, are ordinary files and folders.
//!
//! The walk alone can race with someone swapping a folder for a junction, so
//! every check that matters runs on the final handle: its real path
//! (`GetFinalPathNameByHandleW`) must be inside the root and must not match
//! the deny list, it must be a disk file, and a file must have one link.
//! Checking the real path also defeats 8.3 short names (`ENVPRO~1`) and other
//! aliases the logical path could use to dodge the deny list.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BAD_PATHNAME, ERROR_CANT_ACCESS_FILE, ERROR_DIRECTORY,
    ERROR_FILE_NOT_FOUND, ERROR_FILENAME_EXCED_RANGE, ERROR_INVALID_NAME, ERROR_NO_MORE_FILES,
    ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, HANDLE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_ID_BOTH_DIR_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
    FileAttributeTagInfo, FileIdBothDirectoryInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, GetFileType, GetFinalPathNameByHandleW,
};

use super::{DirEntryInfo, EntryKind, OpenPolicy, OpenedFile, RelPath, RootHandle, Strategy, Want};
use crate::config::Root;
use crate::error::{ErrorCode, ToolError};

/// Symlinks, junctions and mount points have this bit in their tag.
const NAME_SURROGATE: u32 = 0x2000_0000;
/// 100-nanosecond intervals between 1601-01-01 and 1970-01-01.
const EPOCH_DIFF_SECS: i64 = 11_644_473_600;

#[derive(Debug)]
pub struct DirHandle {
    // Held open for the life of the root, like the Unix directory handle.
    _file: File,
    /// The root's real path, as the kernel reports it for the open handle.
    real: PathBuf,
}

fn raw(file: &File) -> HANDLE {
    file.as_raw_handle() as HANDLE
}

/// Open without following a final reparse point. Works for directories too.
fn open_no_follow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Open normally, letting the filesystem handle reparse points (used for
/// cloud placeholders, and for links when the root allows them).
fn open_follow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// `(attributes, reparse tag)`.
fn attribute_tag(file: &File) -> io::Result<(u32, u32)> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO {
        FileAttributes: 0,
        ReparseTag: 0,
    };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            raw(file),
            FileAttributeTagInfo,
            (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((info.FileAttributes, info.ReparseTag))
}

fn is_link(attributes: u32, tag: u32) -> bool {
    attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && tag & NAME_SURROGATE != 0
}

fn file_info(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(raw(file), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

/// The path the kernel reports for an open handle (`\\?\C:\...`).
pub fn final_path(file: &File) -> io::Result<PathBuf> {
    let mut buf = vec![0u16; 512];
    loop {
        let n =
            unsafe { GetFinalPathNameByHandleW(raw(file), buf.as_mut_ptr(), buf.len() as u32, 0) }
                as usize;
        if n == 0 {
            return Err(io::Error::last_os_error());
        }
        if n < buf.len() {
            buf.truncate(n);
            return Ok(PathBuf::from(OsString::from_wide(&buf)));
        }
        buf.resize(n + 1, 0);
    }
}

fn unix_time(filetime: i64) -> i64 {
    filetime.div_euclid(10_000_000) - EPOCH_DIFF_SECS
}

fn filetime(ft: windows_sys::Win32::Foundation::FILETIME) -> i64 {
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

fn lowered(p: &Path) -> Vec<String> {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// Whether two paths name the same place, ignoring case.
fn same_path(a: &Path, b: &Path) -> bool {
    lowered(a) == lowered(b)
}

/// Case-insensitive, component-wise containment for two real paths.
fn within(root: &Path, candidate: &Path) -> bool {
    lowered(candidate).starts_with(&lowered(root))
}

impl RootHandle {
    pub fn open(root: &Root) -> io::Result<Self> {
        let file = open_follow(&root.path)?;
        let (attributes, _) = attribute_tag(&file)?;
        if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "the shared folder is not a directory",
            ));
        }
        let real = final_path(&file)?;
        Ok(Self {
            id: root.id.clone(),
            label: root.label.clone(),
            path: root.path.clone(),
            follow_symlinks: root.follow_symlinks,
            dir: DirHandle { _file: file, real },
        })
    }

    pub fn open_file_with(
        &self,
        rel: &RelPath,
        policy: OpenPolicy<'_>,
        _strategy: Strategy,
    ) -> Result<OpenedFile, ToolError> {
        let (file, info) = self.resolve(rel, Want::File, policy)?;
        Ok(OpenedFile {
            file,
            size: ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64,
            modified: unix_time(filetime(info.ftLastWriteTime)),
            identity: (
                info.dwVolumeSerialNumber as u64,
                ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            ),
        })
    }

    /// List a directory. Denied entries and names that aren't valid Unicode
    /// are left out. Links are reported as symlinks and never followed.
    pub fn list_dir(
        &self,
        rel: &RelPath,
        policy: OpenPolicy<'_>,
    ) -> Result<Vec<DirEntryInfo>, ToolError> {
        let (dir, _) = self.resolve(rel, Want::Dir, policy)?;
        let dir_path = self.abs_path(rel);
        let mut entries = Vec::new();
        // u64 elements keep the buffer aligned for FILE_ID_BOTH_DIR_INFO.
        let mut buf = vec![0u64; 8 * 1024];
        loop {
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    raw(&dir),
                    FileIdBothDirectoryInfo,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * 8) as u32,
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    break;
                }
                return Err(ToolError::internal(format!("reading directory: {err}")));
            }
            let base = buf.as_ptr() as *const u8;
            let mut offset = 0usize;
            loop {
                // SAFETY: the kernel filled `buf` with a chain of entries; each
                // offset stays inside the buffer.
                let entry = unsafe { &*(base.add(offset) as *const FILE_ID_BOTH_DIR_INFO) };
                let name_len = entry.FileNameLength as usize / 2;
                let name_ptr = std::ptr::addr_of!(entry.FileName) as *const u16;
                let name = unsafe { std::slice::from_raw_parts(name_ptr, name_len) };
                if let Ok(name) = String::from_utf16(name)
                    && name != "."
                    && name != ".."
                    && !policy.deny.is_denied(&dir_path.join(&name))
                {
                    let attributes = entry.FileAttributes;
                    // For reparse points, EaSize holds the reparse tag.
                    let kind = if is_link(attributes, entry.EaSize) {
                        EntryKind::Symlink
                    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    };
                    entries.push(DirEntryInfo {
                        name,
                        kind,
                        size: (kind == EntryKind::File).then_some(entry.EndOfFile.max(0) as u64),
                        modified: Some(unix_time(entry.LastWriteTime)),
                    });
                }
                if entry.NextEntryOffset == 0 {
                    break;
                }
                offset += entry.NextEntryOffset as usize;
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn resolve(
        &self,
        rel: &RelPath,
        want: Want,
        policy: OpenPolicy<'_>,
    ) -> Result<(File, BY_HANDLE_FILE_INFORMATION), ToolError> {
        let logical = self.abs_path(rel);
        if let Some(pattern) = policy.deny.denied_by(&logical) {
            return Err(ToolError::denied(format!(
                "this path is on the deny list ({pattern})"
            )));
        }

        let parts = rel.components();
        let file = match self.open_direct(rel) {
            Some(file) => file,
            None => self.open_walk(parts)?,
        };
        self.check_opened(file, want, policy)
    }

    /// The fast path: one open, accepted only when the kernel's real path is
    /// exactly the requested one (ignoring case). Any link, junction or 8.3
    /// short name on the way makes the two differ, and then the careful walk
    /// decides.
    fn open_direct(&self, rel: &RelPath) -> Option<File> {
        if rel.is_root() {
            return None;
        }
        let mut logical = self.dir.real.clone();
        logical.extend(rel.components());
        let file = open_no_follow(&logical).ok()?;
        let (attributes, _) = attribute_tag(&file).ok()?;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return None;
        }
        let real = final_path(&file).ok()?;
        same_path(&real, &logical).then_some(file)
    }

    /// One component at a time, refusing links, so an error can say what
    /// was wrong.
    fn open_walk(&self, parts: &[String]) -> Result<File, ToolError> {
        let mut current = self.dir.real.clone();
        let mut opened = None;
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            current.push(part);
            let file = open_no_follow(&current).map_err(|e| open_error(&e))?;
            let (attributes, tag) = attribute_tag(&file).map_err(|e| io_error(&e, "stat"))?;
            if is_link(attributes, tag) && !self.follow_symlinks {
                return Err(ToolError::denied("symlinks are not followed"));
            }
            if !last && attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                return Err(ToolError::new(
                    ErrorCode::NotADirectory,
                    "a path component is not a directory",
                ));
            }
            if last {
                // Let the filesystem handle cloud placeholders (and links,
                // when allowed): reading through a reparse-point handle
                // wouldn't fetch their contents.
                opened = Some(if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    open_follow(&current).map_err(|e| open_error(&e))?
                } else {
                    file
                });
            }
        }
        match opened {
            Some(file) => Ok(file),
            None => open_follow(&self.dir.real).map_err(|e| open_error(&e)),
        }
    }

    /// The checks that matter, on the opened handle.
    fn check_opened(
        &self,
        file: File,
        want: Want,
        policy: OpenPolicy<'_>,
    ) -> Result<(File, BY_HANDLE_FILE_INFORMATION), ToolError> {
        if unsafe { GetFileType(raw(&file)) } != FILE_TYPE_DISK {
            return Err(ToolError::denied(
                "only regular files and directories can be accessed",
            ));
        }
        let info = file_info(&file).map_err(|e| io_error(&e, "stat"))?;
        let is_dir = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        match (want, is_dir) {
            (Want::File, true) => {
                return Err(ToolError::new(
                    ErrorCode::NotAFile,
                    "this path is a directory",
                ));
            }
            (Want::Dir, false) => {
                return Err(ToolError::new(
                    ErrorCode::NotADirectory,
                    "this path is a file",
                ));
            }
            _ => {}
        }
        if want == Want::File && info.nNumberOfLinks > 1 && !policy.allow_hardlinks {
            return Err(ToolError::denied(
                "files with more than one hard link are not served",
            ));
        }
        let real = final_path(&file).map_err(|e| io_error(&e, "checking the path"))?;
        if !within(&self.dir.real, &real) {
            return Err(ToolError::new(
                ErrorCode::OutsideRoot,
                "this path resolves outside the root",
            ));
        }
        if let Some(pattern) = policy.deny.denied_by(&real) {
            return Err(ToolError::denied(format!(
                "this path is on the deny list ({pattern})"
            )));
        }
        Ok((file, info))
    }
}

fn open_error(err: &io::Error) -> ToolError {
    let code = err.raw_os_error().unwrap_or(0) as u32;
    match code {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => {
            ToolError::new(ErrorCode::NotFound, "no such file or directory")
        }
        ERROR_ACCESS_DENIED | ERROR_CANT_ACCESS_FILE => {
            ToolError::denied("the operating system denied access")
        }
        ERROR_INVALID_NAME | ERROR_BAD_PATHNAME => {
            ToolError::invalid_path("this name is not valid on Windows")
        }
        ERROR_FILENAME_EXCED_RANGE => ToolError::invalid_path("path is too long"),
        ERROR_DIRECTORY => ToolError::new(
            ErrorCode::NotADirectory,
            "a path component is not a directory",
        ),
        ERROR_SHARING_VIOLATION => {
            ToolError::internal("another program has this file open and doesn't allow reading it")
        }
        _ => io_error(err, "opening path"),
    }
}

fn io_error(err: &io::Error, what: &str) -> ToolError {
    ToolError::internal(format!("{what}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::DenyList;

    fn deny() -> DenyList {
        DenyList::new(crate::policy::DEFAULT_DENY.iter().map(|s| s.to_string())).unwrap()
    }

    fn mklink(kind: &str, link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", kind])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: RootHandle,
        symlinks: bool,
    }

    fn fixture(follow: bool) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root_path = base.join("root");
        let outside = base.join("outside");
        for d in [&root_path, &outside, &root_path.join("sub")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(root_path.join("notes.md"), "hello").unwrap();
        std::fs::write(root_path.join("sub/deep.txt"), "deep").unwrap();
        std::fs::write(outside.join("secret.txt"), "outside secret").unwrap();
        std::fs::write(root_path.join(".env"), "TOKEN=1").unwrap();
        std::fs::write(root_path.join(".env.production"), "TOKEN=2").unwrap();
        // Junctions need no privileges; file symlinks need Developer Mode.
        assert!(mklink("/J", &root_path.join("escape_dir"), &outside));
        assert!(mklink(
            "/J",
            &root_path.join("inside_dir"),
            &root_path.join("sub")
        ));
        let symlinks = mklink(
            "",
            &root_path.join("escape.txt"),
            &outside.join("secret.txt"),
        );
        std::fs::hard_link(outside.join("secret.txt"), root_path.join("hardlink.txt")).unwrap();
        let root = RootHandle::open(&Root {
            id: "docs".into(),
            label: "Docs".into(),
            path: root_path,
            follow_symlinks: follow,
        })
        .unwrap();
        Fixture {
            _tmp: tmp,
            root,
            symlinks,
        }
    }

    fn open(f: &Fixture, rel: &str) -> Result<String, ToolError> {
        let deny = deny();
        let policy = OpenPolicy {
            deny: &deny,
            allow_hardlinks: false,
        };
        let rel = RelPath::parse(rel)?;
        let mut opened = f.root.open_file_with(&rel, policy, Strategy::Auto)?;
        let mut s = String::new();
        std::io::Read::read_to_string(&mut opened.file, &mut s).unwrap();
        Ok(s)
    }

    #[test]
    fn reads_inside_root() {
        let f = fixture(false);
        assert_eq!(open(&f, "notes.md").unwrap(), "hello");
        assert_eq!(open(&f, "sub/deep.txt").unwrap(), "deep");
        assert_eq!(
            open(&f, "SUB/Deep.TXT").unwrap(),
            "deep",
            "case-insensitive"
        );
    }

    #[test]
    fn refuses_links_hard_links_and_secrets() {
        let f = fixture(false);
        for rel in ["escape_dir/secret.txt", "inside_dir/deep.txt"] {
            assert_eq!(open(&f, rel).unwrap_err().code, ErrorCode::Denied, "{rel}");
        }
        if f.symlinks {
            assert_eq!(open(&f, "escape.txt").unwrap_err().code, ErrorCode::Denied);
        }
        assert_eq!(
            open(&f, "hardlink.txt").unwrap_err().code,
            ErrorCode::Denied
        );
        assert_eq!(open(&f, ".env").unwrap_err().code, ErrorCode::Denied);
        assert_eq!(open(&f, ".ENV").unwrap_err().code, ErrorCode::Denied);
        assert_eq!(
            open(&f, "notes.md:hidden").unwrap_err().code,
            ErrorCode::InvalidPath,
            "alternate data streams"
        );
        assert_eq!(open(&f, "sub").unwrap_err().code, ErrorCode::NotAFile);
        assert_eq!(
            open(&f, "missing.txt").unwrap_err().code,
            ErrorCode::NotFound
        );
        assert_eq!(
            open(&f, "notes.md/x").unwrap_err().code,
            ErrorCode::NotADirectory
        );
        for device in ["CON", "NUL", "sub/aux.txt"] {
            assert!(open(&f, device).is_err(), "{device}");
        }
    }

    #[test]
    fn short_names_cant_dodge_the_deny_list() {
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
        let f = fixture(false);
        let long = f.root.path.join(".env.production");
        let wide = crate::win::wide(&long.to_string_lossy());
        let mut buf = vec![0u16; 1024];
        let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        let short = PathBuf::from(String::from_utf16_lossy(&buf[..n as usize]));
        let Some(name) = short.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        if name.eq_ignore_ascii_case(".env.production") {
            // 8.3 names are off on this volume; nothing to dodge with.
            return;
        }
        assert_eq!(
            open(&f, name).unwrap_err().code,
            ErrorCode::Denied,
            "{name}"
        );
    }

    #[test]
    fn follow_mode_still_refuses_escapes() {
        let f = fixture(true);
        assert_eq!(open(&f, "inside_dir/deep.txt").unwrap(), "deep");
        // The target also has a hard link in the root, so either refusal
        // can come first.
        let err = open(&f, "escape_dir/secret.txt").unwrap_err();
        assert!(
            matches!(err.code, ErrorCode::OutsideRoot | ErrorCode::Denied),
            "{err:?}"
        );
        std::fs::write(f.root.path.join("../outside/other.txt"), "x").unwrap();
        let err = open(&f, "escape_dir/other.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::OutsideRoot);
    }

    #[test]
    fn lists_without_denied_entries() {
        let f = fixture(false);
        let deny = deny();
        let policy = OpenPolicy {
            deny: &deny,
            allow_hardlinks: false,
        };
        let entries = f.root.list_dir(&RelPath::default(), policy).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"notes.md"));
        assert!(!names.contains(&".env"));
        let junction = entries.iter().find(|e| e.name == "escape_dir").unwrap();
        assert_eq!(junction.kind, EntryKind::Symlink);
        let sub = entries.iter().find(|e| e.name == "sub").unwrap();
        assert_eq!(sub.kind, EntryKind::Dir);
        let notes = entries.iter().find(|e| e.name == "notes.md").unwrap();
        assert_eq!(notes.size, Some(5));
        let err = f
            .root
            .list_dir(&RelPath::parse("escape_dir").unwrap(), policy)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Denied);
    }
}
