//! The Recycle Bin on Windows.
//!
//! Explorer keeps each user's deleted items in `<drive>\$Recycle.Bin\<SID>`:
//! the item itself renamed to `$R<id><ext>`, and a `$I<id><ext>` record
//! with its size, the deletion time and its original path (format 2). cww
//! writes the record, then renames the open item into place by its handle,
//! so Explorer lists it and Restore puts it back. Only fixed drives have a
//! Recycle Bin; items on network and removable drives are refused.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, GetDriveTypeW,
    GetVolumePathNameW, READ_CONTROL, SYNCHRONIZE,
};

use super::{Trash, check_private_bin, unavailable};
use crate::error::ToolError;
use crate::reader::safe_fs::rename_handle;

const DRIVE_FIXED: u32 = 3;
/// 100-nanosecond intervals between 1601-01-01 and 1970-01-01.
const EPOCH_DIFF_SECS: i64 = 11_644_473_600;

/// Windows has no sandbox yet, so there is nothing to allow.
pub fn prepare(_trash: &Trash, _roots: &[PathBuf]) -> Vec<PathBuf> {
    Vec::new()
}

impl Trash {
    /// Move the open `item` (opened with `DELETE` access) to the Recycle
    /// Bin. `original` is its full path as the user knows it, `real` the
    /// path its handle reports, `size` the bytes it holds.
    pub fn put(
        &self,
        item: &File,
        original: &Path,
        real: &Path,
        is_dir: bool,
        size: u64,
    ) -> Result<PathBuf, ToolError> {
        let bin = recycle_bin(real).map_err(unavailable)?;
        let ext = if is_dir {
            String::new()
        } else {
            original
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!(".{e}"))
                .unwrap_or_default()
        };
        let path: Vec<u16> = plain(original).as_os_str().encode_wide().collect();
        for _ in 0..100 {
            let id = random_id().map_err(unavailable)?;
            let record = bin.join(format!("$I{id}{ext}"));
            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&record)
            {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(unavailable(format!("writing the record: {e}"))),
            };
            let written = file
                .write_all(&record_bytes(size, &path))
                .and_then(|()| file.sync_all());
            drop(file);
            if let Err(e) = written {
                let _ = std::fs::remove_file(&record);
                return Err(unavailable(format!("writing the record: {e}")));
            }
            let target = bin.join(format!("$R{id}{ext}"));
            match rename_handle(item, &target) {
                Ok(()) => return Ok(target),
                Err(e) => {
                    let _ = std::fs::remove_file(&record);
                    let code = e.raw_os_error().unwrap_or(0) as u32;
                    if code == ERROR_ALREADY_EXISTS || code == ERROR_FILE_EXISTS {
                        continue;
                    }
                    return Err(unavailable(format!("moving to the Recycle Bin: {e}")));
                }
            }
        }
        Err(unavailable("no free name in the Recycle Bin"))
    }
}

/// `C:\...` for a `\\?\C:\...` path; others as they are.
fn plain(path: &Path) -> PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// This user's Recycle Bin on the drive `real` is on.
fn recycle_bin(real: &Path) -> Result<PathBuf, String> {
    let wide: Vec<u16> = real.as_os_str().encode_wide().chain([0]).collect();
    let mut volume = vec![0u16; 1024];
    if unsafe { GetVolumePathNameW(wide.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) } == 0 {
        return Err(format!(
            "finding the drive: {}",
            std::io::Error::last_os_error()
        ));
    }
    let len = volume.iter().position(|&c| c == 0).unwrap_or(volume.len());
    volume.truncate(len + 1);
    if unsafe { GetDriveTypeW(volume.as_ptr()) } != DRIVE_FIXED {
        return Err("only fixed drives have a Recycle Bin".into());
    }
    let volume = PathBuf::from(String::from_utf16_lossy(&volume[..len]));
    let volume = plain(&volume);
    if volume.as_os_str().to_string_lossy().starts_with(r"\\") {
        return Err("network folders have no Recycle Bin".into());
    }
    let root = volume.join("$Recycle.Bin");
    let meta = std::fs::symlink_metadata(&root)
        .map_err(|e| format!("{} is missing: {e}", root.display()))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(format!("{} is not a folder", root.display()));
    }
    let sid = crate::win::current_user_sid().map_err(|e| format!("{e:#}"))?;
    let bin = root.join(&sid);
    // Explorer makes it on the first delete; make it as Explorer does.
    if std::fs::symlink_metadata(&bin).is_err()
        && let Err(e) = create_private_dir(&bin, &sid)
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(format!("creating {}: {e}", bin.display()));
    }
    let meta = std::fs::symlink_metadata(&bin).map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(format!("{} is not a folder", bin.display()));
    }
    // Someone else could have made it first, open to them: what goes in
    // must stay the user's.
    let opened = OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&bin)
        .map_err(|e| format!("opening {}: {e}", bin.display()))?;
    let security = crate::win::file_security(&opened).map_err(|e| format!("{e:#}"))?;
    let grants: Option<Vec<(String, u32)>> = security
        .grants
        .map(|g| g.into_iter().map(|g| (g.sid, g.mask)).collect());
    check_private_bin(&sid, &security.owner, grants.as_deref(), security.unusual)
        .map_err(|why| format!("{} isn't private to this user: {why}", bin.display()))?;
    Ok(bin)
}

/// Make `path` with the access list Explorer gives a Recycle Bin folder:
/// owned by the user, full control for `SYSTEM`, `Administrators` and the
/// user, nothing inherited from above.
fn create_private_dir(path: &Path, sid: &str) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    let sddl = crate::win::wide(&format!(
        "O:{sid}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})"
    ));
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let made = unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) };
    let result = if made == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    };
    unsafe {
        LocalFree(sd);
    }
    result
}

/// Six characters like Explorer's.
fn random_id() -> Result<String, String> {
    use ring::rand::SecureRandom;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut bytes = [0u8; 6];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "no randomness".to_string())?;
    Ok(bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect())
}

/// A `$I` record, format 2: version, size, deletion time (FILETIME),
/// path length in characters with its NUL, then the path in UTF-16.
fn record_bytes(size: u64, path: &[u16]) -> Vec<u8> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let filetime =
        (now.as_secs() as i64 + EPOCH_DIFF_SECS) * 10_000_000 + i64::from(now.subsec_nanos() / 100);
    let mut out = Vec::with_capacity(28 + path.len() * 2 + 2);
    out.extend_from_slice(&2u64.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&filetime.to_le_bytes());
    out.extend_from_slice(&(path.len() as u32 + 1).to_le_bytes());
    for unit in path.iter().chain([&0u16]) {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_format_two() {
        let path: Vec<u16> = "C:\\a.txt".encode_utf16().collect();
        let bytes = record_bytes(5, &path);
        assert_eq!(&bytes[..8], &2u64.to_le_bytes());
        assert_eq!(&bytes[8..16], &5u64.to_le_bytes());
        assert_eq!(&bytes[24..28], &9u32.to_le_bytes());
        assert_eq!(bytes.len(), 28 + 9 * 2);
    }
}
