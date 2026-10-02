//! Path resolution on Linux and macOS: `openat2` or an `O_NOFOLLOW` walk.

// `stat` field types differ between Linux and macOS, so some casts are only
// no-ops on one of them.
#![allow(clippy::unnecessary_cast)]

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, RenameFlags, Stat};
use rustix::io::Errno;

use super::{
    DirEntryInfo, EntryKind, EntryStat, OpenPolicy, OpenedFile, RelPath, RenameError, RootHandle,
    Strategy, Want, is_within, temp_name,
};
use crate::config::Root;
use crate::error::{ErrorCode, ToolError};
use crate::trash::Trash;

pub type DirHandle = OwnedFd;

static OPENAT2_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

impl RootHandle {
    pub fn open(root: &Root) -> std::io::Result<Self> {
        let dir = rustix::fs::open(
            &root.path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )?;
        Ok(Self {
            id: root.id.clone(),
            label: root.label.clone(),
            path: root.path.clone(),
            follow_symlinks: root.follow_symlinks,
            writable: root.writable,
            dir,
        })
    }

    pub fn open_file_with(
        &self,
        rel: &RelPath,
        policy: OpenPolicy<'_>,
        strategy: Strategy,
    ) -> Result<OpenedFile, ToolError> {
        let (fd, stat) = self.resolve(rel, Want::File, policy, strategy, self.follow_symlinks)?;
        Ok(OpenedFile {
            file: File::from(fd),
            size: stat.st_size as u64,
            modified: stat.st_mtime as i64,
            identity: (stat.st_dev as u64, stat.st_ino as u64),
        })
    }

    pub fn open_dir(&self, rel: &RelPath, policy: OpenPolicy<'_>) -> Result<OwnedFd, ToolError> {
        self.resolve(rel, Want::Dir, policy, Strategy::Auto, self.follow_symlinks)
            .map(|(fd, _)| fd)
    }

    /// Open the directory `rel` for a change. Symlinks are never followed
    /// here, whatever the root allows for reads.
    pub fn change_dir_with(
        &self,
        rel: &RelPath,
        policy: OpenPolicy<'_>,
        strategy: Strategy,
    ) -> Result<ChangeDir, ToolError> {
        let (fd, stat) = self.resolve(rel, Want::Dir, policy, strategy, false)?;
        Ok(ChangeDir {
            path: self.abs_path(rel),
            fd,
            dev: stat.st_dev as u64,
        })
    }

    /// List a directory. Denied entries and names that aren't UTF-8 are left
    /// out. Symlinks are reported as symlinks and never followed.
    pub fn list_dir(
        &self,
        rel: &RelPath,
        policy: OpenPolicy<'_>,
    ) -> Result<Vec<DirEntryInfo>, ToolError> {
        let fd = self.open_dir(rel, policy)?;
        let dir_path = self.abs_path(rel);
        let dir = rustix::fs::Dir::read_from(&fd).map_err(|e| io_error(e, "reading directory"))?;
        let mut entries = Vec::new();
        for entry in dir {
            let entry = entry.map_err(|e| io_error(e, "reading directory"))?;
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            if policy.deny.is_denied(&dir_path.join(name)) {
                continue;
            }
            let Ok(stat) = rustix::fs::statat(&fd, name, AtFlags::SYMLINK_NOFOLLOW) else {
                continue;
            };
            let kind = match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => EntryKind::File,
                FileType::Directory => EntryKind::Dir,
                FileType::Symlink => EntryKind::Symlink,
                _ => EntryKind::Other,
            };
            entries.push(DirEntryInfo {
                name: name.to_string(),
                kind,
                size: (kind == EntryKind::File).then_some(stat.st_size as u64),
                modified: Some(stat.st_mtime as i64),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn resolve(
        &self,
        rel: &RelPath,
        want: Want,
        policy: OpenPolicy<'_>,
        strategy: Strategy,
        follow: bool,
    ) -> Result<(OwnedFd, Stat), ToolError> {
        let logical = self.abs_path(rel);
        if let Some(pattern) = policy.deny.denied_by(&logical) {
            return Err(ToolError::denied(format!(
                "this path is on the deny list ({pattern})"
            )));
        }

        let fd = match strategy {
            Strategy::Auto
                if cfg!(target_os = "linux") && !OPENAT2_UNAVAILABLE.load(Ordering::Relaxed) =>
            {
                match self.open_beneath(rel, want, follow) {
                    Ok(fd) => fd,
                    Err(Errno::NOSYS) | Err(Errno::INVAL) | Err(Errno::PERM)
                        if !OPENAT2_UNAVAILABLE.load(Ordering::Relaxed) && openat2_missing() =>
                    {
                        OPENAT2_UNAVAILABLE.store(true, Ordering::Relaxed);
                        self.open_walk(rel, want, follow)?
                    }
                    Err(e) => return Err(self.classify_beneath_error(e, rel, follow)),
                }
            }
            _ => self.open_walk(rel, want, follow)?,
        };

        let stat = rustix::fs::fstat(&fd).map_err(|e| io_error(e, "stat"))?;
        match (want, FileType::from_raw_mode(stat.st_mode)) {
            (Want::File, FileType::RegularFile) | (Want::Dir, FileType::Directory) => {}
            (Want::File, FileType::Directory) => {
                return Err(ToolError::new(
                    ErrorCode::NotAFile,
                    "this path is a directory",
                ));
            }
            (Want::Dir, FileType::RegularFile) => {
                return Err(ToolError::new(
                    ErrorCode::NotADirectory,
                    "this path is a file",
                ));
            }
            _ => {
                return Err(ToolError::denied(
                    "only regular files and directories can be accessed",
                ));
            }
        }
        if want == Want::File && stat.st_nlink as u64 > 1 && !policy.allow_hardlinks {
            return Err(ToolError::denied(
                "files with more than one hard link are not served",
            ));
        }
        if follow {
            // The logical path may differ from where symlinks led; check the
            // real location against the deny list too.
            if let Some(real) = handle_path(&fd)
                && let Some(pattern) = policy.deny.denied_by(&real)
            {
                return Err(ToolError::denied(format!(
                    "this path is on the deny list ({pattern})"
                )));
            }
        }
        Ok((fd, stat))
    }

    /// `openat2` reports a symlink as `ELOOP` or `ENOTDIR`. Find out which
    /// component is to blame, for an accurate error. Only `lstat`s prefixes
    /// in order and stops at the first symlink, so nothing is followed.
    fn classify_beneath_error(&self, err: Errno, rel: &RelPath, follow: bool) -> ToolError {
        if follow || !matches!(err, Errno::LOOP | Errno::NOTDIR) {
            return resolve_error(err, follow);
        }
        let mut prefix = PathBuf::new();
        for part in rel.components() {
            prefix.push(part);
            match rustix::fs::statat(&self.dir, &prefix, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
                    return ToolError::denied("symlinks are not followed");
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        resolve_error(err, false)
    }

    fn open_flags(want: Want, follow: bool) -> OFlags {
        let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK;
        if want == Want::Dir {
            flags |= OFlags::DIRECTORY;
        }
        if !follow {
            flags |= OFlags::NOFOLLOW;
        }
        flags
    }

    #[cfg(target_os = "linux")]
    fn open_beneath(&self, rel: &RelPath, want: Want, follow: bool) -> Result<OwnedFd, Errno> {
        use rustix::fs::ResolveFlags;
        let mut resolve = ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS;
        if !follow {
            resolve |= ResolveFlags::NO_SYMLINKS;
        }
        rustix::fs::openat2(
            &self.dir,
            rel.fs_path(),
            Self::open_flags(want, follow),
            Mode::empty(),
            resolve,
        )
    }

    #[cfg(not(target_os = "linux"))]
    fn open_beneath(&self, _rel: &RelPath, _want: Want, _follow: bool) -> Result<OwnedFd, Errno> {
        Err(Errno::NOSYS)
    }

    /// Portable resolution: one component at a time with `O_NOFOLLOW`, then a
    /// check that the handle really is inside the root.
    fn open_walk(&self, rel: &RelPath, want: Want, follow: bool) -> Result<OwnedFd, ToolError> {
        let fd = if follow {
            // Let the kernel follow links, then insist the result is inside.
            rustix::fs::openat(
                &self.dir,
                rel.fs_path(),
                Self::open_flags(want, true),
                Mode::empty(),
            )
            .map_err(|e| resolve_error(e, true))?
        } else {
            let parts = rel.components();
            let mut current: Option<OwnedFd> = None;
            if parts.is_empty() {
                current = Some(
                    rustix::fs::openat(
                        &self.dir,
                        ".",
                        Self::open_flags(want, false),
                        Mode::empty(),
                    )
                    .map_err(|e| resolve_error(e, false))?,
                );
            }
            for (i, part) in parts.iter().enumerate() {
                let last = i + 1 == parts.len();
                let flags = if last {
                    Self::open_flags(want, false)
                } else {
                    Self::open_flags(Want::Dir, false)
                };
                let parent = current.as_ref().map_or(self.dir.as_fd(), |fd| fd.as_fd());
                let next = rustix::fs::openat(parent, part.as_str(), flags, Mode::empty())
                    .map_err(|e| classify_walk_error(e, parent, part, last))?;
                current = Some(next);
            }
            current.expect("at least one component was opened")
        };

        match handle_path(&fd) {
            Some(real) if is_within(&self.path, &real) => Ok(fd),
            Some(_) => Err(ToolError::new(
                ErrorCode::OutsideRoot,
                "this path resolves outside the root",
            )),
            None => Err(ToolError::internal(
                "can't verify where this path resolves on this platform",
            )),
        }
    }
}

/// A directory inside a shared folder, opened for a change. Every change
/// happens relative to this handle, so nothing that moves around it later
/// can redirect it.
#[derive(Debug)]
pub struct ChangeDir {
    /// The directory's logical absolute path. Local use only (the deny
    /// list, trash records, logs).
    pub path: PathBuf,
    fd: OwnedFd,
    dev: u64,
}

/// A new file written next to its final name, not yet in place.
#[derive(Debug)]
pub struct Staged {
    name: OsString,
}

impl ChangeDir {
    /// The entry `name`, without following it if it's a link. `None` if
    /// nothing has that name.
    pub fn entry(&self, name: &OsStr) -> Result<Option<EntryStat>, ToolError> {
        match rustix::fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(Some(entry_stat(&stat))),
            Err(Errno::NOENT) => Ok(None),
            Err(e) => Err(change_error(e, "checking the path")),
        }
    }

    /// Read the regular file `name`, at most `cap` bytes.
    pub fn read(&self, name: &OsStr, cap: u64) -> Result<(Vec<u8>, EntryStat), ToolError> {
        let before = self.entry(name)?.ok_or_else(not_found)?;
        if before.kind != EntryKind::File {
            return Err(ToolError::new(
                ErrorCode::NotAFile,
                "This path is not a regular file.",
            ));
        }
        if before.size > cap {
            return Err(too_large(before.size, cap));
        }
        let fd = rustix::fs::openat(
            &self.fd,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|e| change_error(e, "opening the file"))?;
        let stat = entry_stat(&rustix::fs::fstat(&fd).map_err(|e| change_error(e, "stat"))?);
        if stat.identity != before.identity || stat.kind != EntryKind::File {
            return Err(ToolError::new(
                ErrorCode::Conflict,
                "The file changed while it was being read. Try again.",
            ));
        }
        let mut bytes = Vec::new();
        File::from(fd)
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| ToolError::internal(format!("reading the file: {e}")))?;
        if bytes.len() as u64 > cap {
            return Err(too_large(bytes.len() as u64, cap));
        }
        Ok((bytes, stat))
    }

    /// Write `bytes` to a new hidden file in this directory, with `mode`,
    /// and flush it to disk. [`ChangeDir::commit`] puts it in place.
    pub fn stage(&self, bytes: &[u8], mode: u32) -> Result<Staged, ToolError> {
        for _ in 0..16 {
            let name = OsString::from(temp_name());
            let fd = match rustix::fs::openat(
                &self.fd,
                &name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            ) {
                Ok(fd) => fd,
                Err(Errno::EXIST) => continue,
                Err(e) => return Err(change_error(e, "creating the file")),
            };
            let staged = Staged { name };
            let mut file = File::from(fd);
            let written = file
                .write_all(bytes)
                .map_err(|e| e.raw_os_error().map_or(Errno::IO, Errno::from_raw_os_error))
                .and_then(|()| {
                    rustix::fs::fchmod(
                        &file,
                        Mode::from_raw_mode((mode & 0o666) as rustix::fs::RawMode),
                    )
                })
                .and_then(|()| rustix::fs::fsync(&file));
            if let Err(e) = written {
                drop(file);
                self.discard(staged);
                return Err(change_error(e, "writing the file"));
            }
            return Ok(staged);
        }
        Err(ToolError::internal("no free name for a temporary file"))
    }

    /// Put a staged file in place as `name`, which must not exist.
    pub fn commit(&self, staged: Staged, name: &OsStr) -> Result<(), ToolError> {
        match rename_noreplace(self.fd.as_fd(), &staged.name, self.fd.as_fd(), name) {
            Ok(()) => {
                let _ = rustix::fs::fsync(&self.fd);
                Ok(())
            }
            Err(e) => {
                self.discard(staged);
                Err(match e {
                    Errno::EXIST => exists(),
                    other => change_error(other, "putting the file in place"),
                })
            }
        }
    }

    /// Remove a staged file that won't be used.
    pub fn discard(&self, staged: Staged) {
        let _ = rustix::fs::unlinkat(&self.fd, &staged.name, AtFlags::empty());
    }

    /// Make the directory `name` with `mode`. Fails if anything has that
    /// name.
    pub fn make_dir(&self, name: &OsStr, mode: u32) -> Result<(), ToolError> {
        rustix::fs::mkdirat(&self.fd, name, Mode::from_raw_mode(0o700)).map_err(|e| match e {
            Errno::EXIST => exists(),
            other => change_error(other, "making the folder"),
        })?;
        // The daemon's umask is strict; give the folder the mode any other
        // program would, on the handle of the folder just made.
        if let Ok(dir) = self.subdir(name) {
            let _ = rustix::fs::fchmod(
                &dir.fd,
                Mode::from_raw_mode((mode & 0o777) as rustix::fs::RawMode),
            );
        }
        let _ = rustix::fs::fsync(&self.fd);
        Ok(())
    }

    /// Open the subdirectory `name`, never through a link.
    pub fn subdir(&self, name: &OsStr) -> Result<ChangeDir, ToolError> {
        let fd = rustix::fs::openat(
            &self.fd,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| match e {
            Errno::LOOP | Errno::MLINK => ToolError::denied("Symlinks are never followed."),
            other => change_error(other, "opening the folder"),
        })?;
        let stat = rustix::fs::fstat(&fd).map_err(|e| change_error(e, "stat"))?;
        Ok(ChangeDir {
            path: self.path.join(name),
            fd,
            dev: stat.st_dev as u64,
        })
    }

    /// Everything in this directory, links included and not followed.
    pub fn entries(&self) -> Result<Vec<(OsString, EntryStat)>, ToolError> {
        let dir = rustix::fs::Dir::read_from(&self.fd)
            .map_err(|e| change_error(e, "reading the folder"))?;
        let mut out = Vec::new();
        for entry in dir {
            let entry = entry.map_err(|e| change_error(e, "reading the folder"))?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name == "." || name == ".." {
                continue;
            }
            if let Some(stat) = self.entry(name)? {
                out.push((name.to_os_string(), stat));
            }
        }
        Ok(out)
    }

    /// Move the entry `name` to `to`/`to_name`, never replacing anything.
    pub fn rename(&self, name: &OsStr, to: &ChangeDir, to_name: &OsStr) -> Result<(), RenameError> {
        rename_noreplace(self.fd.as_fd(), name, to.fd.as_fd(), to_name).map_err(|e| match e {
            Errno::XDEV => RenameError::CrossDevice,
            Errno::EXIST | Errno::NOTEMPTY => RenameError::Exists,
            Errno::INVAL => RenameError::Other(ToolError::invalid_argument(
                "A folder can't be moved into itself.",
            )),
            other => RenameError::Other(change_error(other, "moving")),
        })
    }

    /// Move the entry `name` to the system trash. Returns where it went.
    pub fn trash(
        &self,
        name: &OsStr,
        trash: &Trash,
        stat: &EntryStat,
    ) -> Result<PathBuf, ToolError> {
        let name = name
            .to_str()
            .ok_or_else(|| ToolError::invalid_path("This name is not valid UTF-8."))?;
        let went = trash.put(
            self.fd.as_fd(),
            name,
            &self.path.join(name),
            stat.identity.0,
            stat.kind == EntryKind::Dir,
        )?;
        let _ = rustix::fs::fsync(&self.fd);
        Ok(went)
    }

    /// Remove `name` and everything under it, for a copy this daemon made
    /// and abandoned. Never used on the user's files.
    pub fn remove_own(&self, name: &OsStr) {
        match self.subdir(name) {
            Ok(sub) => {
                if let Ok(entries) = sub.entries() {
                    for (child, _) in entries {
                        sub.remove_own(&child);
                    }
                }
                let _ = rustix::fs::unlinkat(&self.fd, name, AtFlags::REMOVEDIR);
            }
            Err(_) => {
                let _ = rustix::fs::unlinkat(&self.fd, name, AtFlags::empty());
            }
        }
    }

    /// The device the directory is on.
    pub fn device(&self) -> u64 {
        self.dev
    }
}

fn entry_stat(stat: &Stat) -> EntryStat {
    let kind = match FileType::from_raw_mode(stat.st_mode) {
        FileType::RegularFile => EntryKind::File,
        FileType::Directory => EntryKind::Dir,
        FileType::Symlink => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    EntryStat {
        kind,
        size: stat.st_size as u64,
        modified: stat.st_mtime as i64,
        links: stat.st_nlink as u64,
        mode: stat.st_mode as u32 & 0o7777,
        foreign: stat.st_uid != rustix::process::geteuid().as_raw(),
        readonly: kind == EntryKind::File && stat.st_mode as u32 & 0o222 == 0,
        identity: (stat.st_dev as u64, stat.st_ino as u64),
    }
}

/// Rename without replacing whatever is at the target:
/// `renameat2(RENAME_NOREPLACE)` on Linux, `renameatx_np(RENAME_EXCL)` on
/// macOS. On a Linux filesystem without it, look first; changes run one at
/// a time, so only another program could slip in between.
pub fn rename_noreplace<P: rustix::path::Arg + Copy, Q: rustix::path::Arg + Copy>(
    from_dir: BorrowedFd<'_>,
    from: P,
    to_dir: BorrowedFd<'_>,
    to: Q,
) -> Result<(), Errno> {
    match rustix::fs::renameat_with(from_dir, from, to_dir, to, RenameFlags::NOREPLACE) {
        Err(Errno::INVAL | Errno::NOSYS) if cfg!(target_os = "linux") => {
            match rustix::fs::statat(to_dir, to, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => Err(Errno::EXIST),
                Err(Errno::NOENT) => rustix::fs::renameat(from_dir, from, to_dir, to),
                Err(e) => Err(e),
            }
        }
        other => other,
    }
}

fn not_found() -> ToolError {
    ToolError::new(ErrorCode::NotFound, "Nothing exists at this path.")
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

fn change_error(err: Errno, what: &str) -> ToolError {
    match err {
        Errno::NOENT => not_found(),
        Errno::LOOP | Errno::MLINK => ToolError::denied("Symlinks are never followed."),
        Errno::ACCESS | Errno::PERM | Errno::ROFS => {
            ToolError::denied("The operating system denied the change.")
        }
        Errno::NOSPC | Errno::DQUOT => ToolError::internal("The disk is full."),
        Errno::NAMETOOLONG => ToolError::invalid_path("The name is too long."),
        Errno::NOTDIR => ToolError::new(
            ErrorCode::NotADirectory,
            "A part of the path is not a folder.",
        ),
        other => ToolError::internal(format!("{what}: {other}")),
    }
}

/// The path the kernel reports for an open handle.
pub fn handle_path(fd: &OwnedFd) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd())).ok()
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        use std::os::unix::ffi::OsStringExt;
        rustix::fs::getpath(fd)
            .ok()
            .map(|c| PathBuf::from(std::ffi::OsString::from_vec(c.into_bytes())))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
    {
        let _ = fd;
        None
    }
}

fn openat2_missing() -> bool {
    #[cfg(target_os = "linux")]
    {
        // Probe with a trivially valid call. Only a missing syscall (or a
        // seccomp filter that blocks it) makes this fail.
        rustix::fs::openat2(
            rustix::fs::CWD,
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            rustix::fs::ResolveFlags::empty(),
        )
        .is_err()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

fn classify_walk_error(
    err: Errno,
    parent: std::os::fd::BorrowedFd<'_>,
    name: &str,
    last: bool,
) -> ToolError {
    if matches!(err, Errno::LOOP | Errno::NOTDIR | Errno::MLINK)
        && let Ok(stat) = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
    {
        return match FileType::from_raw_mode(stat.st_mode) {
            FileType::Symlink => ToolError::denied("symlinks are not followed"),
            FileType::Directory => resolve_error(err, false),
            _ if !last => ToolError::new(
                ErrorCode::NotADirectory,
                "a path component is not a directory",
            ),
            _ => resolve_error(err, false),
        };
    }
    resolve_error(err, false)
}

fn resolve_error(err: Errno, follow: bool) -> ToolError {
    match err {
        Errno::NOENT => ToolError::new(ErrorCode::NotFound, "no such file or directory"),
        Errno::LOOP | Errno::MLINK if follow => ToolError::denied("too many levels of symlinks"),
        Errno::LOOP | Errno::MLINK => ToolError::denied("symlinks are not followed"),
        Errno::XDEV => ToolError::new(
            ErrorCode::OutsideRoot,
            "this path resolves outside the root",
        ),
        Errno::NOTDIR => ToolError::new(
            ErrorCode::NotADirectory,
            "a path component is not a directory",
        ),
        Errno::ACCESS | Errno::PERM => ToolError::denied("the operating system denied access"),
        Errno::NAMETOOLONG => ToolError::invalid_path("path is too long"),
        other => io_error(other, "opening path"),
    }
}

fn io_error(err: Errno, what: &str) -> ToolError {
    ToolError::internal(format!("{what}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::DenyList;
    use std::os::unix::fs::symlink;

    fn deny() -> DenyList {
        DenyList::new(crate::policy::DEFAULT_DENY.iter().map(|s| s.to_string())).unwrap()
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: RootHandle,
        outside: PathBuf,
    }

    fn fixture(follow: bool) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root_path = base.join("root");
        let outside = base.join("outside");
        let sibling = base.join("root2");
        for d in [&root_path, &outside, &sibling, &root_path.join("sub")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(root_path.join("notes.md"), "hello").unwrap();
        std::fs::write(root_path.join("sub/deep.txt"), "deep").unwrap();
        std::fs::write(outside.join("secret.txt"), "outside secret").unwrap();
        std::fs::write(sibling.join("file.txt"), "sibling").unwrap();
        std::fs::write(root_path.join(".env"), "TOKEN=1").unwrap();
        symlink(outside.join("secret.txt"), root_path.join("escape.txt")).unwrap();
        symlink(&outside, root_path.join("escape_dir")).unwrap();
        symlink(&sibling, root_path.join("prefix_trick")).unwrap();
        symlink("notes.md", root_path.join("inside_link.md")).unwrap();
        symlink(
            format!("/proc/self/root{}", outside.join("secret.txt").display()),
            root_path.join("magic"),
        )
        .unwrap();
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
            outside,
        }
    }

    fn open(f: &Fixture, rel: &str, strategy: Strategy) -> Result<String, ToolError> {
        let deny = deny();
        let policy = OpenPolicy {
            deny: &deny,
            allow_hardlinks: false,
        };
        let rel = RelPath::parse(rel)?;
        let mut opened = f.root.open_file_with(&rel, policy, strategy)?;
        let mut s = String::new();
        std::io::Read::read_to_string(&mut opened.file, &mut s).unwrap();
        Ok(s)
    }

    const STRATEGIES: [Strategy; 2] = [Strategy::Auto, Strategy::Walk];

    #[test]
    fn reads_inside_root() {
        let f = fixture(false);
        for s in STRATEGIES {
            assert_eq!(open(&f, "notes.md", s).unwrap(), "hello");
            assert_eq!(open(&f, "sub/deep.txt", s).unwrap(), "deep");
        }
    }

    #[test]
    fn refuses_symlink_escapes() {
        let f = fixture(false);
        for s in STRATEGIES {
            for rel in [
                "escape.txt",
                "escape_dir/secret.txt",
                "prefix_trick/file.txt",
                "inside_link.md",
                "magic",
            ] {
                let err = open(&f, rel, s).unwrap_err();
                assert_eq!(err.code, ErrorCode::Denied, "{rel} with {s:?}: {err:?}");
            }
        }
    }

    #[test]
    fn follow_mode_still_refuses_escapes() {
        let f = fixture(true);
        for s in STRATEGIES {
            assert_eq!(open(&f, "inside_link.md", s).unwrap(), "hello");
            // `magic` goes through /proc, which only Linux has.
            let magic = if cfg!(target_os = "linux") {
                "magic"
            } else {
                "escape.txt"
            };
            for rel in [
                "escape.txt",
                "escape_dir/secret.txt",
                "prefix_trick/file.txt",
                magic,
            ] {
                let err = open(&f, rel, s).unwrap_err();
                assert!(
                    matches!(err.code, ErrorCode::OutsideRoot | ErrorCode::Denied),
                    "{rel} with {s:?}: {err:?}"
                );
            }
        }
    }

    #[test]
    fn refuses_hard_links_and_deny_list() {
        let f = fixture(false);
        for s in STRATEGIES {
            assert_eq!(
                open(&f, "hardlink.txt", s).unwrap_err().code,
                ErrorCode::Denied
            );
            assert_eq!(open(&f, ".env", s).unwrap_err().code, ErrorCode::Denied);
        }
        assert!(f.outside.exists());
    }

    #[test]
    fn refuses_fifos_and_directories() {
        let f = fixture(false);
        let status = std::process::Command::new("mkfifo")
            .arg(f.root.path.join("pipe"))
            .status()
            .unwrap();
        assert!(status.success());
        for s in STRATEGIES {
            assert_eq!(open(&f, "pipe", s).unwrap_err().code, ErrorCode::Denied);
            assert_eq!(open(&f, "sub", s).unwrap_err().code, ErrorCode::NotAFile);
            assert_eq!(
                open(&f, "missing.txt", s).unwrap_err().code,
                ErrorCode::NotFound
            );
            assert_eq!(
                open(&f, "notes.md/x", s).unwrap_err().code,
                ErrorCode::NotADirectory
            );
        }
    }

    #[test]
    fn change_dirs_never_follow_links() {
        // Even a root that follows links for reads.
        for follow in [false, true] {
            let f = fixture(follow);
            let deny = deny();
            let policy = OpenPolicy {
                deny: &deny,
                allow_hardlinks: false,
            };
            for s in STRATEGIES {
                let sub = f
                    .root
                    .change_dir_with(&RelPath::parse("sub").unwrap(), policy, s)
                    .unwrap();
                assert_eq!(sub.path, f.root.path.join("sub"));
                for rel in ["escape_dir", "prefix_trick", "escape_dir/x", "notes.md"] {
                    let err = f
                        .root
                        .change_dir_with(&RelPath::parse(rel).unwrap(), policy, s)
                        .unwrap_err();
                    assert!(
                        matches!(
                            err.code,
                            ErrorCode::Denied | ErrorCode::NotADirectory | ErrorCode::OutsideRoot
                        ),
                        "{rel} with {s:?}, follow {follow}: {err:?}"
                    );
                }
                // The entry itself is reported, never followed.
                let top = f
                    .root
                    .change_dir_with(&RelPath::default(), policy, s)
                    .unwrap();
                let link = top.entry(OsStr::new("escape_dir")).unwrap().unwrap();
                assert_eq!(link.kind, EntryKind::Symlink);
                assert!(top.subdir(OsStr::new("escape_dir")).is_err());
                let hard = top.entry(OsStr::new("hardlink.txt")).unwrap().unwrap();
                assert_eq!(hard.links, 2);
            }
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
        let link = entries.iter().find(|e| e.name == "escape_dir").unwrap();
        assert_eq!(link.kind, EntryKind::Symlink);
        let err = f
            .root
            .list_dir(&RelPath::parse("escape_dir").unwrap(), policy)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Denied);
    }
}
