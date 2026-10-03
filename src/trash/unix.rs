//! The trash on Linux (freedesktop.org) and macOS.

use std::fs;
use std::io::Write;
use std::os::fd::BorrowedFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;

use super::{Trash, numbered, unavailable};
use crate::error::ToolError;
use crate::reader::safe_fs::rename_noreplace;

/// Where items from one filesystem go.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Location {
    /// The directory the sandbox must allow changes in.
    base: PathBuf,
    /// Where items are moved to.
    files: PathBuf,
    /// Where their `.trashinfo` records go (freedesktop only).
    info: Option<PathBuf>,
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
        let max = if location.info.is_some() {
            255 - ".trashinfo".len()
        } else {
            255
        };
        for n in 0..1000 {
            let candidate = numbered(name, n, is_dir, max);
            let record = match &location.info {
                Some(info) => match write_record(info, &candidate, original, &location) {
                    Ok(path) => Some(path),
                    Err(Errno::EXIST) => continue,
                    Err(e) => return Err(unavailable(format!("writing the trash record: {e}"))),
                },
                None => None,
            };
            let target = location.files.join(&candidate);
            let moved = rename_noreplace(dir, name, rustix::fs::CWD, &target);
            if moved.is_err()
                && let Some(record) = &record
            {
                let _ = fs::remove_file(record);
            }
            match moved {
                Ok(()) => return Ok(target),
                Err(Errno::EXIST | Errno::NOTEMPTY) => continue,
                Err(e) => return Err(unavailable(format!("moving to the trash: {e}"))),
            }
        }
        Err(unavailable("no free name in the trash"))
    }
}

/// Write `<info>/<name>.trashinfo`, refusing to overwrite one.
fn write_record(
    info: &Path,
    name: &str,
    original: &Path,
    location: &Location,
) -> Result<PathBuf, Errno> {
    let path = info.join(format!("{name}.trashinfo"));
    let fd = rustix::fs::open(
        &path,
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
        let _ = fs::remove_file(&path);
        return Err(Errno::from_io_error(&e).unwrap_or(Errno::IO));
    }
    Ok(path)
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

/// A trash directory: make it (0700) if it's missing, then insist it is a
/// real directory of ours, not a link someone left there.
fn ensure_dir(path: &Path, recursive: bool) -> Result<(), String> {
    if fs::symlink_metadata(path).is_err() {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).recursive(recursive);
        if let Err(e) = builder.create(path)
            && e.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(format!("creating {}: {e}", path.display()));
        }
    }
    let meta =
        fs::symlink_metadata(path).map_err(|e| format!("checking {}: {e}", path.display()))?;
    if !meta.file_type().is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    if meta.uid() != uid() {
        return Err(format!("{} belongs to another user", path.display()));
    }
    Ok(())
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
    ensure_dir(home, true)?;
    if device(home)? == dev {
        return freedesktop(home.clone(), None);
    }
    let top = topdir(near, dev).ok_or("the item's filesystem has no top directory")?;
    let base = topdir_trash(&top, uid())?;
    freedesktop(base, Some(top))
}

#[cfg(not(target_os = "macos"))]
fn freedesktop(base: PathBuf, topdir: Option<PathBuf>) -> Result<Location, String> {
    let files = base.join("files");
    let info = base.join("info");
    ensure_dir(&files, false)?;
    ensure_dir(&info, false)?;
    Ok(Location {
        base,
        files,
        info: Some(info),
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

/// `$topdir/.Trash/$uid` if an administrator set up a sticky `.Trash`
/// (and it isn't a link), else `$topdir/.Trash-$uid`.
#[cfg(not(target_os = "macos"))]
fn topdir_trash(top: &Path, uid: u32) -> Result<PathBuf, String> {
    let shared = top.join(".Trash");
    if let Ok(meta) = fs::symlink_metadata(&shared)
        && meta.file_type().is_dir()
        && meta.mode() & 0o1000 != 0
    {
        let mine = shared.join(uid.to_string());
        if ensure_dir(&mine, false).is_ok() {
            return Ok(mine);
        }
    }
    let mine = top.join(format!(".Trash-{uid}"));
    ensure_dir(&mine, false)?;
    Ok(mine)
}

#[cfg(target_os = "macos")]
fn location(trash: &Trash, dev: u64, near: &Path) -> Result<Location, String> {
    let home = trash.home.as_ref().ok_or("no home trash")?;
    ensure_dir(home, true)?;
    if device(home)? == dev {
        return Ok(Location {
            base: home.clone(),
            files: home.clone(),
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
    let trashes = mount.join(".Trashes");
    let meta = fs::symlink_metadata(&trashes)
        .map_err(|e| format!("{} has no .Trashes: {e}", mount.display()))?;
    if !meta.file_type().is_dir() {
        return Err(format!("{} is not a directory", trashes.display()));
    }
    let mine = trashes.join(uid().to_string());
    ensure_dir(&mine, false)?;
    Ok(Location {
        base: mine.clone(),
        files: mine,
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
    fn topdir_trash_prefers_a_sticky_shared_trash() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path().canonicalize().unwrap();
        // Without .Trash: a private .Trash-$uid.
        assert_eq!(
            topdir_trash(&top, uid()).unwrap(),
            top.join(format!(".Trash-{}", uid()))
        );
        // A .Trash without the sticky bit is ignored.
        fs::create_dir(top.join(".Trash")).unwrap();
        assert_eq!(
            topdir_trash(&top, uid()).unwrap(),
            top.join(format!(".Trash-{}", uid()))
        );
        // With it, $uid inside it.
        fs::set_permissions(top.join(".Trash"), fs::Permissions::from_mode(0o1777)).unwrap();
        assert_eq!(
            topdir_trash(&top, uid()).unwrap(),
            top.join(".Trash").join(uid().to_string())
        );
        // A link named .Trash is never used.
        let other = tempfile::tempdir().unwrap();
        let top2 = other.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(top.join(".Trash"), top2.join(".Trash")).unwrap();
        assert_eq!(
            topdir_trash(&top2, uid()).unwrap(),
            top2.join(format!(".Trash-{}", uid()))
        );
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
