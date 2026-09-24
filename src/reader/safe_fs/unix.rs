//! Path resolution on Linux and macOS: `openat2` or an `O_NOFOLLOW` walk.

// `stat` field types differ between Linux and macOS, so some casts are only
// no-ops on one of them.
#![allow(clippy::unnecessary_cast)]

use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;

use super::{
    DirEntryInfo, EntryKind, OpenPolicy, OpenedFile, RelPath, RootHandle, Strategy, Want, is_within,
};
use crate::config::Root;
use crate::error::{ErrorCode, ToolError};

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
            dir,
        })
    }

    pub fn open_file_with(
        &self,
        rel: &RelPath,
        policy: OpenPolicy<'_>,
        strategy: Strategy,
    ) -> Result<OpenedFile, ToolError> {
        let (fd, stat) = self.resolve(rel, Want::File, policy, strategy)?;
        Ok(OpenedFile {
            file: File::from(fd),
            size: stat.st_size as u64,
            modified: stat.st_mtime as i64,
            identity: (stat.st_dev as u64, stat.st_ino as u64),
        })
    }

    pub fn open_dir(&self, rel: &RelPath, policy: OpenPolicy<'_>) -> Result<OwnedFd, ToolError> {
        self.resolve(rel, Want::Dir, policy, Strategy::Auto)
            .map(|(fd, _)| fd)
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
                match self.open_beneath(rel, want) {
                    Ok(fd) => fd,
                    Err(Errno::NOSYS) | Err(Errno::INVAL) | Err(Errno::PERM)
                        if !OPENAT2_UNAVAILABLE.load(Ordering::Relaxed) && openat2_missing() =>
                    {
                        OPENAT2_UNAVAILABLE.store(true, Ordering::Relaxed);
                        self.open_walk(rel, want)?
                    }
                    Err(e) => return Err(self.classify_beneath_error(e, rel)),
                }
            }
            _ => self.open_walk(rel, want)?,
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
        if self.follow_symlinks {
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
    fn classify_beneath_error(&self, err: Errno, rel: &RelPath) -> ToolError {
        if self.follow_symlinks || !matches!(err, Errno::LOOP | Errno::NOTDIR) {
            return resolve_error(err, self.follow_symlinks);
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

    fn open_flags(&self, want: Want) -> OFlags {
        let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK;
        if want == Want::Dir {
            flags |= OFlags::DIRECTORY;
        }
        if !self.follow_symlinks {
            flags |= OFlags::NOFOLLOW;
        }
        flags
    }

    #[cfg(target_os = "linux")]
    fn open_beneath(&self, rel: &RelPath, want: Want) -> Result<OwnedFd, Errno> {
        use rustix::fs::ResolveFlags;
        let mut resolve = ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS;
        if !self.follow_symlinks {
            resolve |= ResolveFlags::NO_SYMLINKS;
        }
        rustix::fs::openat2(
            &self.dir,
            rel.fs_path(),
            self.open_flags(want),
            Mode::empty(),
            resolve,
        )
    }

    #[cfg(not(target_os = "linux"))]
    fn open_beneath(&self, _rel: &RelPath, _want: Want) -> Result<OwnedFd, Errno> {
        Err(Errno::NOSYS)
    }

    /// Portable resolution: one component at a time with `O_NOFOLLOW`, then a
    /// check that the handle really is inside the root.
    fn open_walk(&self, rel: &RelPath, want: Want) -> Result<OwnedFd, ToolError> {
        let fd = if self.follow_symlinks {
            // Let the kernel follow links, then insist the result is inside.
            rustix::fs::openat(
                &self.dir,
                rel.fs_path(),
                self.open_flags(want),
                Mode::empty(),
            )
            .map_err(|e| resolve_error(e, true))?
        } else {
            let parts = rel.components();
            let mut current: Option<OwnedFd> = None;
            if parts.is_empty() {
                current = Some(
                    rustix::fs::openat(&self.dir, ".", self.open_flags(want), Mode::empty())
                        .map_err(|e| resolve_error(e, false))?,
                );
            }
            for (i, part) in parts.iter().enumerate() {
                let last = i + 1 == parts.len();
                let flags = if last {
                    self.open_flags(want)
                } else {
                    self.open_flags(Want::Dir)
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
