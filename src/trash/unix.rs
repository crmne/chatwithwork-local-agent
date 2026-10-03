//! The trash on Linux (freedesktop.org) and macOS.
//!
//! Every trash directory is opened as a handle, never through a link at
//! any step cww makes or checks, and must belong to the user; records are
//! written and items renamed relative to those handles, so nothing that
//! swaps a directory for a link later can redirect them.

use std::fs;
use std::io::Write;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;

use super::{Trash, numbered, randomized, unavailable};
use crate::error::ToolError;
use crate::reader::safe_fs::rename_noreplace;

/// Numbered names tried before random ones.
const NUMBERED_TRIES: u32 = 1000;
/// Random names tried after that.
const RANDOM_TRIES: u32 = 64;

/// Where items from one filesystem go.
#[derive(Debug)]
struct Location {
    /// The directory the sandbox must allow changes in.
    base: PathBuf,
    /// Where items are moved to.
    files: PathBuf,
    files_fd: OwnedFd,
    /// Where their `.trashinfo` records go (freedesktop only).
    info: Option<(PathBuf, OwnedFd)>,
    /// The top of the filesystem, for trash directories outside the home
    /// trash: records there name the item relative to it.
    topdir: Option<PathBuf>,
}

/// Set up the trash for items in each of `roots` (creating its
/// directories if needed) and return the directories it lives in, for the
/// sandbox. Runs before the sandbox is applied. A root whose trash can't be
/// set up is left out: changes there that need the trash are refused.
pub fn prepare(trash: &Trash, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for root in roots {
        let Ok(meta) = fs::symlink_metadata(root) else {
            continue;
        };
        match location(trash, meta.dev(), root) {
            Ok(location) => {
                if !dirs.contains(&location.base) {
                    dirs.push(location.base);
                }
            }
            Err(e) => tracing::warn!("no trash for {}: {e}", root.display()),
        }
    }
    dirs
}

impl Trash {
    /// Move the entry `name` of the open directory `dir` to the trash.
    /// `original` is its absolute path (for the record), `dev` the device
    /// it lives on. Returns where it went.
    pub fn put(
        &self,
        dir: BorrowedFd<'_>,
        name: &str,
        original: &Path,
        dev: u64,
        is_dir: bool,
    ) -> Result<PathBuf, ToolError> {
        let near = original.parent().unwrap_or(original);
        let location = location(self, dev, near).map_err(unavailable)?;
        let shown = original
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(name);
        let max = if location.info.is_some() {
            255 - ".trashinfo".len()
        } else {
            255
        };
        for n in 0..NUMBERED_TRIES + RANDOM_TRIES {
            let candidate = if n < NUMBERED_TRIES {
                numbered(shown, n, is_dir, max)
            } else {
                randomized(shown, is_dir, max)
            };
            let record = match &location.info {
                Some((_, info)) => {
                    match write_record(info.as_fd(), &candidate, original, &location) {
                        Ok(record) => Some(record),
                        Err(Errno::EXIST) => continue,
                        Err(e) => {
                            return Err(unavailable(format!("writing the trash record: {e}")));
                        }
                    }
                }
                None => None,
            };
            let moved = rename_noreplace(dir, name, location.files_fd.as_fd(), &*candidate);
            if moved.is_err()
                && let (Some(record), Some((_, info))) = (&record, &location.info)
            {
                let _ = rustix::fs::unlinkat(info, record.as_str(), AtFlags::empty());
            }
            match moved {
                Ok(()) => return Ok(location.files.join(&candidate)),
                Err(Errno::EXIST | Errno::NOTEMPTY) => continue,
                Err(e) => return Err(unavailable(format!("moving to the trash: {e}"))),
            }
        }
        Err(unavailable("no free name in the trash"))
    }
}

/// Write `<name>.trashinfo` in the open `info` directory, refusing to
/// overwrite one. Returns the record's name.
fn write_record(
    info: BorrowedFd<'_>,
    name: &str,
    original: &Path,
    location: &Location,
) -> Result<String, Errno> {
    let record_name = format!("{name}.trashinfo");
    let fd = rustix::fs::openat(
        info,
        record_name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?;
    let shown = match &location.topdir {
        Some(top) => original.strip_prefix(top).unwrap_or(original),
        None => original,
    };
    let record = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        encode_path(shown),
        deletion_date()
    );
    let mut file = fs::File::from(fd);
    let written = file
        .write_all(record.as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(e) = written {
        drop(file);
        let _ = rustix::fs::unlinkat(info, record_name.as_str(), AtFlags::empty());
        return Err(Errno::from_io_error(&e).unwrap_or(Errno::IO));
    }
    Ok(record_name)
}

/// A path as the trash spec wants it: percent-encoded, slashes kept.
fn encode_path(path: &Path) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_encode};
    use std::os::unix::ffi::OsStrExt;
    const KEEP: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'/')
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    percent_encode(path.as_os_str().as_bytes(), KEEP).to_string()
}

/// Local time without a zone, as the trash spec wants it.
fn deletion_date() -> String {
    let now = time::OffsetDateTime::now_utc().to_offset(super::local_offset());
    let format = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");
    now.format(&format).unwrap_or_default()
}

fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// How trash directories are opened: never through a link. On Linux as
/// `O_PATH` handles, which are enough to create, rename and check relative
/// to them, and which Landlock lets the daemon open without the right to
/// list the trash (it only takes items in).
#[cfg(target_os = "linux")]
const DIR_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
#[cfg(not(target_os = "linux"))]
const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Insist an open directory is ours.
fn check_own(fd: &OwnedFd, shown: &Path) -> Result<Stat, String> {
    let stat = rustix::fs::fstat(fd).map_err(|e| format!("checking {}: {e}", shown.display()))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(format!("{} is not a directory", shown.display()));
    }
    if stat.st_uid != uid() {
        return Err(format!("{} belongs to another user", shown.display()));
    }
    Ok(stat)
}

/// A trash directory at `path`: made (0700, with its parents when
/// `recursive`) if it's missing, then opened without following a link and
/// checked to be a directory of ours.
fn open_own_dir(path: &Path, recursive: bool) -> Result<OwnedFd, String> {
    if fs::symlink_metadata(path).is_err() {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).recursive(recursive);
        if let Err(e) = builder.create(path)
            && e.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(format!("creating {}: {e}", path.display()));
        }
    }
    let fd = rustix::fs::open(path, DIR_FLAGS, Mode::empty()).map_err(|e| match e {
        Errno::LOOP | Errno::NOTDIR => format!("{} is not a directory", path.display()),
        e => format!("opening {}: {e}", path.display()),
    })?;
    check_own(&fd, path)?;
    Ok(fd)
}

/// The directory `name` in the open `parent`: made (0700) if it's missing,
/// opened without following a link, and checked to be ours.
fn open_own_subdir(parent: &OwnedFd, name: &str, shown: &Path) -> Result<OwnedFd, String> {
    match rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(e) => return Err(format!("creating {}: {e}", shown.display())),
    }
    let fd = rustix::fs::openat(parent, name, DIR_FLAGS, Mode::empty()).map_err(|e| match e {
        Errno::LOOP | Errno::NOTDIR => format!("{} is not a directory", shown.display()),
        e => format!("opening {}: {e}", shown.display()),
    })?;
    check_own(&fd, shown)?;
    Ok(fd)
}

fn device(path: &Path) -> Result<u64, String> {
    fs::symlink_metadata(path)
        .map(|m| m.dev())
        .map_err(|e| format!("checking {}: {e}", path.display()))
}

/// The trash for items on device `dev`, found from `near`, a directory on
/// that device.
#[cfg(not(target_os = "macos"))]
fn location(trash: &Trash, dev: u64, near: &Path) -> Result<Location, String> {
    let home = trash.home.as_ref().ok_or("no home trash")?;
    let home_fd = open_own_dir(home, true)?;
    if device(home)? == dev {
        return freedesktop(home.clone(), home_fd, None);
    }
    let top = topdir(near, dev).ok_or("the item's filesystem has no top directory")?;
    let (base, base_fd) = topdir_trash(&top, uid())?;
    freedesktop(base, base_fd, Some(top))
}

#[cfg(not(target_os = "macos"))]
fn freedesktop(
    base: PathBuf,
    base_fd: OwnedFd,
    topdir: Option<PathBuf>,
) -> Result<Location, String> {
    let files = base.join("files");
    let info = base.join("info");
    let files_fd = open_own_subdir(&base_fd, "files", &files)?;
    let info_fd = open_own_subdir(&base_fd, "info", &info)?;
    Ok(Location {
        base,
        files,
        files_fd,
        info: Some((info, info_fd)),
        topdir,
    })
}

/// The mount point of the filesystem `near` is on: its highest ancestor on
/// the same device.
#[cfg(not(target_os = "macos"))]
fn topdir(near: &Path, dev: u64) -> Option<PathBuf> {
    let mut top = None;
    for dir in near.ancestors() {
        match fs::symlink_metadata(dir) {
            Ok(meta) if meta.dev() == dev && meta.file_type().is_dir() => top = Some(dir),
            _ => break,
        }
    }
    top.map(Path::to_path_buf)
}

/// Whether a `$topdir/.Trash` may hold users' trashes, as the spec asks:
/// a real directory (not a link), with the sticky bit, and, so no other
/// user can have made it to collect what is deleted, owned by root.
#[cfg(not(target_os = "macos"))]
fn shared_trash_ok(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::Directory
        && stat.st_mode & 0o1000 != 0
        && stat.st_uid == 0
}

/// `$topdir/.Trash/$uid` if an administrator set up `.Trash` as the spec
/// asks ([`shared_trash_ok`]), else `$topdir/.Trash-$uid`. Every step is
/// opened relative to the one before, never through a link.
#[cfg(not(target_os = "macos"))]
fn topdir_trash(top: &Path, uid: u32) -> Result<(PathBuf, OwnedFd), String> {
    let top_fd = rustix::fs::open(top, DIR_FLAGS, Mode::empty())
        .map_err(|e| format!("opening {}: {e}", top.display()))?;
    let shared = top.join(".Trash");
    if let Ok(shared_fd) = rustix::fs::openat(&top_fd, ".Trash", DIR_FLAGS, Mode::empty())
        && rustix::fs::fstat(&shared_fd).is_ok_and(|s| shared_trash_ok(&s))
    {
        let mine = shared.join(uid.to_string());
        if let Ok(fd) = open_own_subdir(&shared_fd, &uid.to_string(), &mine) {
            return Ok((mine, fd));
        }
    }
    let name = format!(".Trash-{uid}");
    let mine = top.join(&name);
    let fd = open_own_subdir(&top_fd, &name, &mine)?;
    Ok((mine, fd))
}

#[cfg(target_os = "macos")]
fn location(trash: &Trash, dev: u64, near: &Path) -> Result<Location, String> {
    let home = trash.home.as_ref().ok_or("no home trash")?;
    let home_fd = open_own_dir(home, true)?;
    if device(home)? == dev {
        return Ok(Location {
            base: home.clone(),
            files: home.clone(),
            files_fd: home_fd,
            info: None,
            topdir: None,
        });
    }
    let stat = rustix::fs::statfs(near).map_err(|e| format!("statfs: {e}"))?;
    // SAFETY: the kernel fills f_mntonname with a NUL-terminated path.
    let mount = unsafe { std::ffi::CStr::from_ptr(stat.f_mntonname.as_ptr()) };
    let mount = PathBuf::from(std::ffi::OsStr::from_bytes(mount.to_bytes()));
    if device(&mount)? != dev {
        return Err(format!("{} is not the item's volume", mount.display()));
    }
    let mount_fd = rustix::fs::open(&mount, DIR_FLAGS, Mode::empty())
        .map_err(|e| format!("opening {}: {e}", mount.display()))?;
    let trashes = mount.join(".Trashes");
    let trashes_fd = rustix::fs::openat(&mount_fd, ".Trashes", DIR_FLAGS, Mode::empty())
        .map_err(|e| format!("{} has no .Trashes: {e}", mount.display()))?;
    let mine = trashes.join(uid().to_string());
    let fd = open_own_subdir(&trashes_fd, &uid().to_string(), &mine)?;
    Ok(Location {
        base: mine.clone(),
        files: mine,
        files_fd: fd,
        info: None,
        topdir: None,
    })
}

#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt;

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    fn read_dir(path: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn dir_fd(path: &Path) -> std::os::fd::OwnedFd {
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap()
    }

    #[test]
    fn moves_items_to_the_home_trash_with_a_record() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("data/Trash");
        let work = base.join("work");
        fs::create_dir_all(work.join("sub")).unwrap();
        fs::write(work.join("notes 1.md"), "one").unwrap();
        let trash = Trash::new(Some(home.clone()));
        assert_eq!(
            prepare(&trash, std::slice::from_ref(&work)),
            vec![home.clone()]
        );

        let dev = fs::metadata(&work).unwrap().dev();
        let fd = dir_fd(&work);
        let went = trash
            .put(
                fd.as_fd(),
                "notes 1.md",
                &work.join("notes 1.md"),
                dev,
                false,
            )
            .unwrap();
        assert!(!work.join("notes 1.md").exists());
        assert_eq!(fs::read_to_string(&went).unwrap(), "one");

        // A second item with the same name gets a number.
        fs::write(work.join("notes 1.md"), "two").unwrap();
        let again = trash
            .put(
                fd.as_fd(),
                "notes 1.md",
                &work.join("notes 1.md"),
                dev,
                false,
            )
            .unwrap();
        assert_ne!(went, again);
        assert_eq!(fs::read_to_string(&again).unwrap(), "two");
        let folder = trash
            .put(fd.as_fd(), "sub", &work.join("sub"), dev, true)
            .unwrap();
        assert!(folder.is_dir());

        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(
                read_dir(&home.join("files")),
                ["notes 1.2.md", "notes 1.md", "sub"]
            );
            assert_eq!(
                read_dir(&home.join("info")),
                [
                    "notes 1.2.md.trashinfo",
                    "notes 1.md.trashinfo",
                    "sub.trashinfo"
                ]
            );
            let record = fs::read_to_string(home.join("info/notes 1.md.trashinfo")).unwrap();
            let mut lines = record.lines();
            assert_eq!(lines.next(), Some("[Trash Info]"));
            let path = lines.next().unwrap();
            assert_eq!(
                path,
                format!("Path={}/notes%201.md", encode_path(&work)),
                "{record}"
            );
            let date = lines.next().unwrap();
            assert!(date.starts_with("DeletionDate=20"), "{record}");
            assert_eq!(date.len(), "DeletionDate=2026-10-03T12:00:00".len());
            let mode = fs::metadata(home.join("files")).unwrap().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        #[cfg(target_os = "macos")]
        assert_eq!(read_dir(&home), ["notes 1 2.md", "notes 1.md", "sub"]);
    }

    /// After a thousand items of the same name, names get a random part
    /// instead of failing.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn finds_a_name_after_a_thousand_of_the_same() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("Trash");
        let work = base.join("work");
        fs::create_dir_all(home.join("info")).unwrap();
        fs::create_dir_all(home.join("files")).unwrap();
        fs::create_dir_all(&work).unwrap();
        for n in 0..NUMBERED_TRIES {
            let name = numbered("notes.md", n, false, 245);
            fs::write(home.join("info").join(format!("{name}.trashinfo")), "").unwrap();
        }
        fs::write(work.join("notes.md"), "last").unwrap();
        let trash = Trash::new(Some(home.clone()));
        let fd = dir_fd(&work);
        let dev = fs::metadata(&work).unwrap().dev();
        let went = trash
            .put(fd.as_fd(), "notes.md", &work.join("notes.md"), dev, false)
            .unwrap();
        assert_eq!(fs::read_to_string(&went).unwrap(), "last");
        let name = went.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("notes.") && name.ends_with(".md"),
            "{name}"
        );
        assert!(
            home.join("info").join(format!("{name}.trashinfo")).exists(),
            "{name}"
        );
    }

    #[test]
    fn refuses_a_trash_that_is_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let elsewhere = base.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, base.join("Trash")).unwrap();
        let work = base.join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("a.txt"), "a").unwrap();
        let trash = Trash::new(Some(base.join("Trash")));
        assert!(prepare(&trash, std::slice::from_ref(&work)).is_empty());
        let fd = dir_fd(&work);
        let dev = fs::metadata(&work).unwrap().dev();
        let err = trash
            .put(fd.as_fd(), "a.txt", &work.join("a.txt"), dev, false)
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::TrashUnavailable);
        assert!(work.join("a.txt").exists(), "nothing moved");
    }

    #[test]
    fn refuses_items_on_another_device_without_a_trash() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let work = base.join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("a.txt"), "a").unwrap();
        let trash = Trash::new(Some(base.join("Trash")));
        let fd = dir_fd(&work);
        // No ancestor is on this made-up device, so there is no topdir.
        let err = trash
            .put(fd.as_fd(), "a.txt", &work.join("a.txt"), u64::MAX, false)
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::TrashUnavailable);
        assert!(work.join("a.txt").exists());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn topdir_trash_uses_only_a_shared_trash_root_set_up() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path().canonicalize().unwrap();
        let private = top.join(format!(".Trash-{}", uid()));
        // Without .Trash: a private .Trash-$uid.
        assert_eq!(topdir_trash(&top, uid()).unwrap().0, private);
        // A .Trash without the sticky bit is ignored, and so is a sticky
        // one any user could have made: only root's counts.
        fs::create_dir(top.join(".Trash")).unwrap();
        assert_eq!(topdir_trash(&top, uid()).unwrap().0, private);
        fs::set_permissions(top.join(".Trash"), fs::Permissions::from_mode(0o1777)).unwrap();
        if uid() != 0 {
            assert_eq!(topdir_trash(&top, uid()).unwrap().0, private);
        }
        // A link named .Trash is never used.
        let other = tempfile::tempdir().unwrap();
        let top2 = other.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(top.join(".Trash"), top2.join(".Trash")).unwrap();
        assert_eq!(
            topdir_trash(&top2, uid()).unwrap().0,
            top2.join(format!(".Trash-{}", uid()))
        );
        // Nor a .Trash-$uid that is a link.
        let third = tempfile::tempdir().unwrap();
        let top3 = third.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(&top, top3.join(format!(".Trash-{}", uid()))).unwrap();
        assert!(topdir_trash(&top3, uid()).is_err());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_shared_trash_must_be_roots_sticky_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mut stat = rustix::fs::stat(tmp.path()).unwrap();
        stat.st_mode = (stat.st_mode & !0o7777) | 0o1777;
        stat.st_uid = 0;
        assert!(shared_trash_ok(&stat));
        let mut not_root = stat;
        not_root.st_uid = 1000;
        assert!(!shared_trash_ok(&not_root));
        let mut not_sticky = stat;
        not_sticky.st_mode &= !0o1000;
        assert!(!shared_trash_ok(&not_sticky));
    }

    /// The files and info directories are used by handle: one that is a
    /// link to elsewhere is refused, never followed.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn refuses_trash_subdirectories_that_are_links() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("Trash");
        let elsewhere = base.join("elsewhere");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, home.join("files")).unwrap();
        let work = base.join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("a.txt"), "a").unwrap();
        let trash = Trash::new(Some(home));
        let fd = dir_fd(&work);
        let dev = fs::metadata(&work).unwrap().dev();
        let err = trash
            .put(fd.as_fd(), "a.txt", &work.join("a.txt"), dev, false)
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::TrashUnavailable);
        assert!(work.join("a.txt").exists());
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn topdir_is_the_highest_ancestor_on_the_device() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let dev = fs::metadata(&dir).unwrap().dev();
        let top = topdir(&dir, dev).unwrap();
        assert!(dir.starts_with(&top));
        assert_eq!(fs::metadata(&top).unwrap().dev(), dev);
        if let Some(parent) = top.parent() {
            assert_ne!(fs::metadata(parent).unwrap().dev(), dev);
        }
    }
}
