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

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_BAD_PATHNAME, ERROR_CANT_ACCESS_FILE,
    ERROR_DIRECTORY, ERROR_DISK_FULL, ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND,
    ERROR_FILENAME_EXCED_RANGE, ERROR_INVALID_NAME, ERROR_NO_MORE_FILES, ERROR_NOT_SAME_DEVICE,
    ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, HANDLE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_WRITE, FILE_ID_BOTH_DIR_INFO, FILE_READ_ATTRIBUTES,
    FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
    FileAttributeTagInfo, FileIdBothDirectoryInfo, FileRenameInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, GetFileType, GetFinalPathNameByHandleW, READ_CONTROL,
    SYNCHRONIZE, SetFileInformationByHandle, WRITE_DAC,
};

use super::{
    DirEntryInfo, EntryKind, EntryStat, OpenPolicy, OpenedFile, RelPath, RenameError, RootHandle,
    Strategy, Want, temp_name,
};
use crate::config::Root;
use crate::error::{ErrorCode, ToolError};
use crate::trash::Trash;

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
        let info = file_info(&file)?;
        Ok(Self {
            id: root.id.clone(),
            label: root.label.clone(),
            path: root.path.clone(),
            follow_symlinks: root.follow_symlinks,
            writable: root.writable,
            identity: identity_of(&info),
            dir: DirHandle { _file: file, real },
        })
    }

    pub fn open_file_with(
        &self,
        rel: &RelPath,
        policy: OpenPolicy<'_>,
        _strategy: Strategy,
    ) -> Result<OpenedFile, ToolError> {
        let (file, info) = self.resolve(rel, Want::File, policy, self.follow_symlinks)?;
        Ok(OpenedFile {
            file,
            size: ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64,
            modified: unix_time(filetime(info.ftLastWriteTime)),
            identity: identity_of(&info),
        })
    }

    /// List a directory. Denied entries and names that aren't valid Unicode
    /// are left out. Links are reported as symlinks and never followed.
    pub fn list_dir(
        &self,
        rel: &RelPath,
        policy: OpenPolicy<'_>,
    ) -> Result<Vec<DirEntryInfo>, ToolError> {
        let (dir, _) = self.resolve(rel, Want::Dir, policy, self.follow_symlinks)?;
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

    /// Open the directory `rel` for a change. Links and junctions are
    /// never followed here, whatever the root allows for reads.
    pub fn change_dir_with(
        &self,
        rel: &RelPath,
        policy: OpenPolicy<'_>,
        _strategy: Strategy,
    ) -> Result<ChangeDir, ToolError> {
        let (dir, info) = self.resolve(rel, Want::Dir, policy, false)?;
        let real = final_path(&dir).map_err(|e| io_error(&e, "checking the path"))?;
        Ok(ChangeDir {
            path: self.abs_path(rel),
            _dir: dir,
            real,
            identity: identity_of(&info),
        })
    }

    fn resolve(
        &self,
        rel: &RelPath,
        want: Want,
        policy: OpenPolicy<'_>,
        follow: bool,
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
            None => self.open_walk(parts, follow)?,
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
    fn open_walk(&self, parts: &[String], follow: bool) -> Result<File, ToolError> {
        let mut current = self.dir.real.clone();
        let mut opened = None;
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            current.push(part);
            let file = open_no_follow(&current).map_err(|e| open_error(&e))?;
            let (attributes, tag) = attribute_tag(&file).map_err(|e| io_error(&e, "stat"))?;
            if is_link(attributes, tag) && !follow {
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

/// A directory inside a shared folder, opened for a change. Windows has no
/// `openat`, so each change opens `<real path of this handle>\<name>`
/// without following reparse points, then checks the handle it got.
#[derive(Debug)]
pub struct ChangeDir {
    /// The directory's logical absolute path. Local use only.
    pub path: PathBuf,
    /// Held open while the change runs, like the Unix directory handle.
    _dir: File,
    /// The path the kernel reports for `_dir`.
    real: PathBuf,
    /// The volume serial number and file ID of `_dir`.
    identity: (u64, u64),
}

/// A new file written next to its final name, not yet in place.
#[derive(Debug)]
pub struct Staged {
    file: File,
    path: PathBuf,
}

/// Open without following a final reparse point, with `access`.
fn open_with(path: &Path, access: u32) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// The volume serial number and file index: what identifies a file or
/// folder whatever name reaches it.
fn identity_of(info: &BY_HANDLE_FILE_INFORMATION) -> (u64, u64) {
    (
        info.dwVolumeSerialNumber as u64,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
    )
}

fn stat_of(file: &File) -> io::Result<EntryStat> {
    let (attributes, tag) = attribute_tag(file)?;
    let info = file_info(file)?;
    let kind = if is_link(attributes, tag) {
        EntryKind::Symlink
    } else if unsafe { GetFileType(raw(file)) } != FILE_TYPE_DISK {
        EntryKind::Other
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    Ok(EntryStat {
        kind,
        size: ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64,
        modified: unix_time(filetime(info.ftLastWriteTime)),
        links: info.nNumberOfLinks as u64,
        mode: 0,
        foreign: is_foreign(file),
        readonly: info.dwFileAttributes & FILE_ATTRIBUTE_READONLY != 0,
        identity: identity_of(&info),
    })
}

/// Volumes that keep owners and ACLs have this flag (NTFS, ReFS; not FAT).
const FILE_PERSISTENT_ACLS: u32 = 0x0000_0008;

/// Owned by someone other than the user the daemon runs as. A file whose
/// owner can't be read counts as someone else's, unless its volume keeps
/// no owners at all (FAT). The handle needs `READ_CONTROL`.
fn is_foreign(file: &File) -> bool {
    match crate::win::file_owner_sid(file) {
        Ok(owner) => crate::win::user_sid() != Some(owner.as_str()),
        Err(_) => volume_flags(file).is_none_or(|flags| flags & FILE_PERSISTENT_ACLS != 0),
    }
}

/// The file system flags of the volume `file` is on.
fn volume_flags(file: &File) -> Option<u32> {
    let mut flags = 0u32;
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetVolumeInformationByHandleW(
            raw(file),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            std::ptr::null_mut(),
            0,
        )
    };
    (ok != 0).then_some(flags)
}

/// Whether the file `name` starts as a Windows program does (`MZ`, the
/// header of every `.exe`, `.dll`, `.scr` and `.sys`), whatever its name.
fn has_program_header(file: &mut File) -> io::Result<bool> {
    let mut head = [0u8; 2];
    let mut read = 0;
    while read < head.len() {
        match file.read(&mut head[read..])? {
            0 => break,
            n => read += n,
        }
    }
    Ok(read == 2 && head == *b"MZ")
}

fn is_missing(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error().map(|c| c as u32),
        Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
    )
}

fn is_exists(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error().map(|c| c as u32),
        Some(ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS)
    )
}

impl ChangeDir {
    /// `name` in this directory, as the kernel names it.
    fn child(&self, name: &OsStr) -> PathBuf {
        self.real.join(name)
    }

    /// Open `name` without following it, and insist the handle is really
    /// in this directory.
    fn open_child(&self, name: &OsStr, access: u32) -> Result<(File, EntryStat), ToolError> {
        let file = open_with(&self.child(name), access).map_err(|e| change_error(&e))?;
        let stat = stat_of(&file).map_err(|e| io_error(&e, "stat"))?;
        if stat.kind == EntryKind::Symlink {
            return Err(ToolError::denied("Links and junctions are never followed."));
        }
        let real = final_path(&file).map_err(|e| io_error(&e, "checking the path"))?;
        if !real.parent().is_some_and(|p| same_path(p, &self.real)) {
            return Err(ToolError::new(
                ErrorCode::OutsideRoot,
                "This path resolves outside its folder.",
            ));
        }
        // An 8.3 short name (`ENVPRO~1`) is another name for a file the
        // deny list may cover: only the file's own name is accepted.
        if !same_name(&real, name) {
            return Err(short_name());
        }
        Ok((file, stat))
    }

    /// The entry `name`, without following it. An 8.3 short name or any
    /// other alias for an entry with a different name is refused, so every
    /// check that sees an entry sees it by its own name.
    pub fn entry(&self, name: &OsStr) -> Result<Option<EntryStat>, ToolError> {
        let file = match open_with(
            &self.child(name),
            FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
        ) {
            Ok(file) => file,
            Err(e) if is_missing(&e) => return Ok(None),
            Err(e) => return Err(change_error(&e)),
        };
        let stat = stat_of(&file).map_err(|e| io_error(&e, "stat"))?;
        let real = final_path(&file).map_err(|e| io_error(&e, "checking the path"))?;
        if !same_name(&real, name) {
            return Err(short_name());
        }
        Ok(Some(stat))
    }

    /// Whether the regular file `name` is a Windows program by its
    /// content (an `MZ` header), whatever it is called.
    pub fn is_program(&self, name: &OsStr) -> Result<bool, ToolError> {
        let (mut file, stat) =
            self.open_child(name, windows_sys::Win32::Foundation::GENERIC_READ)?;
        if stat.kind != EntryKind::File {
            return Ok(false);
        }
        has_program_header(&mut file).map_err(|e| io_error(&e, "reading the file"))
    }

    pub fn read(&self, name: &OsStr, cap: u64) -> Result<(Vec<u8>, EntryStat), ToolError> {
        let (file, stat) = self.open_child(name, windows_sys::Win32::Foundation::GENERIC_READ)?;
        if stat.kind != EntryKind::File {
            return Err(ToolError::new(
                ErrorCode::NotAFile,
                "This path is not a regular file.",
            ));
        }
        if stat.size > cap {
            return Err(too_large(stat.size, cap));
        }
        let mut bytes = Vec::new();
        file.take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(&e, "reading the file"))?;
        if bytes.len() as u64 > cap {
            return Err(too_large(bytes.len() as u64, cap));
        }
        Ok((bytes, stat))
    }

    /// Write `bytes` to a new hidden file in this directory, marked with
    /// the Mark-of-the-Web, and flush it. With `like`, the name of a file
    /// it is about to replace, it gets that file's access list too, as far
    /// as the user may set it.
    pub fn stage(
        &self,
        bytes: &[u8],
        _mode: u32,
        like: Option<&OsStr>,
    ) -> Result<Staged, ToolError> {
        for _ in 0..16 {
            let path = self.child(OsStr::new(&temp_name()));
            // `write(true)` only satisfies std's check that a file it
            // creates is opened for writing; `access_mode` is what Windows
            // gets.
            let mut file = match OpenOptions::new()
                .write(true)
                .access_mode(FILE_GENERIC_WRITE | DELETE | WRITE_DAC | READ_CONTROL)
                .share_mode(0)
                .create_new(true)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&path)
            {
                Ok(file) => file,
                Err(e) if is_exists(&e) => continue,
                Err(e) => return Err(change_error(&e)),
            };
            if let Some(like) = like
                && let Ok((old, _)) = self.open_child(like, READ_CONTROL | SYNCHRONIZE)
                && let Err(e) = crate::win::copy_dacl(&old, &file)
            {
                tracing::warn!("keeping the file's access list: {e:#}");
            }
            let written = file
                .write_all(bytes)
                .and_then(|()| file.sync_all())
                .and_then(|()| mark_from_internet(&file, &path));
            let staged = Staged { file, path };
            if let Err(e) = written {
                self.discard(staged);
                return Err(change_error(&e));
            }
            return Ok(staged);
        }
        Err(ToolError::internal("no free name for a temporary file"))
    }

    pub fn commit(&self, staged: Staged, name: &OsStr) -> Result<(), ToolError> {
        match rename_handle(&staged.file, &self.child(name)) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.discard(staged);
                Err(if is_exists(&e) {
                    exists()
                } else {
                    change_error(&e)
                })
            }
        }
    }

    /// Windows has no swap of two names; the caller trashes, then commits.
    pub fn exchange(&self, _staged: &Staged, _name: &OsStr) -> Result<bool, ToolError> {
        Ok(false)
    }

    /// Only after [`ChangeDir::exchange`], which Windows never does.
    pub fn trash_staged(
        &self,
        _staged: &Staged,
        _shown: &OsStr,
        _trash: &Trash,
        _stat: &EntryStat,
    ) -> Result<PathBuf, ToolError> {
        Err(ToolError::internal("Windows can't swap files"))
    }

    pub fn discard(&self, staged: Staged) {
        drop(staged.file);
        let _ = std::fs::remove_file(&staged.path);
    }

    pub fn make_dir(&self, name: &OsStr, _mode: u32) -> Result<(), ToolError> {
        std::fs::create_dir(self.child(name)).map_err(|e| {
            if is_exists(&e) {
                exists()
            } else {
                change_error(&e)
            }
        })?;
        self.subdir(name).map(|_| ())
    }

    pub fn subdir(&self, name: &OsStr) -> Result<ChangeDir, ToolError> {
        let (dir, stat) = self.open_child(name, windows_sys::Win32::Foundation::GENERIC_READ)?;
        if stat.kind != EntryKind::Dir {
            return Err(ToolError::new(
                ErrorCode::NotADirectory,
                "A part of the path is not a folder.",
            ));
        }
        let real = final_path(&dir).map_err(|e| io_error(&e, "checking the path"))?;
        Ok(ChangeDir {
            path: self.path.join(name),
            _dir: dir,
            real,
            identity: stat.identity,
        })
    }

    /// Everything in this directory, links included and not followed.
    /// Each entry is opened (as a reparse point) and described from its
    /// handle, so its link count and identity are real.
    pub fn entries(&self) -> Result<Vec<(OsString, EntryStat)>, ToolError> {
        // A fresh handle, so the listing starts at the beginning.
        let dir = open_with(&self.real, windows_sys::Win32::Foundation::GENERIC_READ)
            .map_err(|e| change_error(&e))?;
        let mut names = Vec::new();
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
                return Err(io_error(&err, "reading the folder"));
            }
            let base = buf.as_ptr() as *const u8;
            let mut offset = 0usize;
            loop {
                // SAFETY: the kernel filled `buf` with a chain of entries.
                let entry = unsafe { &*(base.add(offset) as *const FILE_ID_BOTH_DIR_INFO) };
                let name_len = entry.FileNameLength as usize / 2;
                let name_ptr = std::ptr::addr_of!(entry.FileName) as *const u16;
                let name =
                    OsString::from_wide(unsafe { std::slice::from_raw_parts(name_ptr, name_len) });
                if name != "." && name != ".." {
                    names.push(name);
                }
                if entry.NextEntryOffset == 0 {
                    break;
                }
                offset += entry.NextEntryOffset as usize;
            }
        }
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            if let Some(stat) = self.entry(&name)? {
                out.push((name, stat));
            }
        }
        Ok(out)
    }

    pub fn rename(&self, name: &OsStr, to: &ChangeDir, to_name: &OsStr) -> Result<(), RenameError> {
        let (file, _) = self
            .open_child(
                name,
                DELETE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
            )
            .map_err(RenameError::Other)?;
        rename_handle(&file, &to.child(to_name)).map_err(|e| {
            if e.raw_os_error() == Some(ERROR_NOT_SAME_DEVICE as i32) {
                RenameError::CrossDevice
            } else if is_exists(&e) {
                RenameError::Exists
            } else {
                RenameError::Other(change_error(&e))
            }
        })
    }

    pub fn trash(
        &self,
        name: &OsStr,
        trash: &Trash,
        stat: &EntryStat,
    ) -> Result<PathBuf, ToolError> {
        let (file, _) = self.open_child(
            name,
            DELETE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
        )?;
        let real = final_path(&file).map_err(|e| io_error(&e, "checking the path"))?;
        trash.put(
            &file,
            &self.path.join(name),
            &real,
            stat.kind == EntryKind::Dir,
            stat.size,
        )
    }

    pub fn remove_own(&self, name: &OsStr) {
        let path = self.child(name);
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }

    pub fn device(&self) -> u64 {
        self.identity.0
    }

    /// The volume serial number and file ID of the open directory.
    pub fn identity(&self) -> (u64, u64) {
        self.identity
    }

    /// Where the kernel says the open directory is
    /// (`GetFinalPathNameByHandleW`): long names, as stored.
    pub fn real_path(&self) -> Option<PathBuf> {
        Some(self.real.clone())
    }
}

/// Whether the last component of `real` is `name`, ignoring case: not an
/// 8.3 short name or another alias for it.
fn same_name(real: &Path, name: &OsStr) -> bool {
    real.file_name().is_some_and(|n| {
        n.to_string_lossy().to_lowercase() == name.to_string_lossy().to_lowercase()
    })
}

fn short_name() -> ToolError {
    ToolError::denied("This is a short name for another file; use the file's full name.")
}

/// Volumes with alternate data streams have this flag.
const FILE_NAMED_STREAMS: u32 = 0x0004_0000;

/// Give a file cww made the Mark-of-the-Web (a `Zone.Identifier` stream
/// with `ZoneId=3`, the Internet zone), as browsers do for downloads: Office
/// opens it in Protected View and SmartScreen checks it before it runs. A
/// volume without alternate data streams (FAT) can't hold the mark.
fn mark_from_internet(file: &File, path: &Path) -> io::Result<()> {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    let marked = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&stream)
        .and_then(|mut zone| {
            zone.write_all(b"[ZoneTransfer]\r\nZoneId=3\r\n")
                .and_then(|()| zone.sync_all())
        });
    match marked {
        Err(_) if !has_named_streams(file) => Ok(()),
        other => other,
    }
}

fn has_named_streams(file: &File) -> bool {
    // If the volume can't be asked, assume it has them: refuse rather than
    // write a file without the mark.
    volume_flags(file).is_none_or(|flags| flags & FILE_NAMED_STREAMS != 0)
}

/// Rename the open `file` to `target` (a full path), never replacing
/// anything there. The handle needs `DELETE` access.
pub fn rename_handle(file: &File, target: &Path) -> io::Result<()> {
    let name: Vec<u16> = target.as_os_str().encode_wide().collect();
    let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let size = header + (name.len() + 1) * 2;
    // u64 elements keep the buffer aligned for FILE_RENAME_INFO.
    let mut buf = vec![0u64; size.div_ceil(8)];
    let info = buf.as_mut_ptr() as *mut FILE_RENAME_INFO;
    // SAFETY: `buf` is zeroed, aligned and large enough for the header and
    // the name that follows it.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = (name.len() * 2) as u32;
        let dst = std::ptr::addr_of_mut!((*info).FileName) as *mut u16;
        std::ptr::copy_nonoverlapping(name.as_ptr(), dst, name.len());
    }
    let ok = unsafe {
        SetFileInformationByHandle(raw(file), FileRenameInfo, buf.as_ptr().cast(), size as u32)
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn exists() -> ToolError {
    ToolError::new(ErrorCode::Exists, "Something already exists at this path.")
}

fn too_large(size: u64, cap: u64) -> ToolError {
    ToolError::new(
        ErrorCode::TooLarge,
        format!("The file is {size} bytes; files up to {cap} bytes can be changed."),
    )
}

fn change_error(err: &io::Error) -> ToolError {
    match err.raw_os_error().map(|c| c as u32) {
        Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => {
            ToolError::new(ErrorCode::NotFound, "Nothing exists at this path.")
        }
        Some(ERROR_ACCESS_DENIED) => ToolError::denied("The operating system denied the change."),
        Some(ERROR_SHARING_VIOLATION) => {
            ToolError::internal("Another program has this file open, so it can't be changed.")
        }
        Some(ERROR_DISK_FULL) => ToolError::internal("The disk is full."),
        Some(ERROR_INVALID_NAME | ERROR_BAD_PATHNAME) => {
            ToolError::invalid_path("This name is not valid on Windows.")
        }
        _ => io_error(err, "changing the file"),
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
            writable: false,
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
        // Changes refuse the short name too.
        let deny = deny();
        let policy = OpenPolicy {
            deny: &deny,
            allow_hardlinks: false,
        };
        let top = f
            .root
            .change_dir_with(&RelPath::default(), policy, Strategy::Auto)
            .unwrap();
        let err = top.read(OsStr::new(name), 1024).unwrap_err();
        assert_eq!(err.code, ErrorCode::Denied, "{name}");
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
    fn change_dirs_never_follow_junctions() {
        for follow in [false, true] {
            let f = fixture(follow);
            let deny = deny();
            let policy = OpenPolicy {
                deny: &deny,
                allow_hardlinks: false,
            };
            let sub = f
                .root
                .change_dir_with(&RelPath::parse("sub").unwrap(), policy, Strategy::Auto)
                .unwrap();
            assert!(sub.entry(OsStr::new("deep.txt")).unwrap().is_some());
            for rel in ["escape_dir", "inside_dir", "escape_dir/x"] {
                let err = f
                    .root
                    .change_dir_with(&RelPath::parse(rel).unwrap(), policy, Strategy::Auto)
                    .unwrap_err();
                assert_eq!(err.code, ErrorCode::Denied, "{rel}, follow {follow}");
            }
            let top = f
                .root
                .change_dir_with(&RelPath::default(), policy, Strategy::Auto)
                .unwrap();
            let junction = top.entry(OsStr::new("escape_dir")).unwrap().unwrap();
            assert_eq!(junction.kind, EntryKind::Symlink);
            assert!(top.subdir(OsStr::new("escape_dir")).is_err());
            let hard = top.entry(OsStr::new("hardlink.txt")).unwrap().unwrap();
            assert_eq!(hard.links, 2);
        }
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
