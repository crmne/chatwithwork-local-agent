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
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};

use super::{Trash, unavailable};
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
    let bin = root.join(sid);
    // Explorer makes it on the first delete; the inherited ACL is the one
    // Explorer's gets.
    if std::fs::symlink_metadata(&bin).is_err()
        && let Err(e) = std::fs::create_dir(&bin)
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(format!("creating {}: {e}", bin.display()));
    }
    let meta = std::fs::symlink_metadata(&bin).map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(format!("{} is not a folder", bin.display()));
    }
    Ok(bin)
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
