//! The questions asked in the composer, kept between runs in
//! `<state dir>/history` (`~/.local/state/cww/history`): one JSON string per
//! line, newest last, only readable by you. Up recalls them, as a shell
//! does.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::composer::HISTORY_KEEP;
use crate::paths::Paths;

pub fn path(paths: &Paths) -> PathBuf {
    paths.state_dir.join("history")
}

/// The questions kept, oldest first. A missing or unreadable file is an
/// empty history, and lines that don't parse are skipped.
pub fn load(path: &Path) -> Vec<String> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut entries: Vec<String> = BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<String>(&line).ok())
        .collect();
    if entries.len() > HISTORY_KEEP {
        entries.drain(..entries.len() - HISTORY_KEEP);
    }
    entries
}

/// Add a question. The file is trimmed to the newest entries now and then,
/// so it never grows much past what's kept.
pub fn append(path: &Path, entry: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        crate::paths::ensure_private_dir(dir)?;
    }
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');
    private(OpenOptions::new().create(true).append(true))
        .open(path)?
        .write_all(line.as_bytes())?;
    let kept = fs::read_to_string(path)?.lines().count();
    if kept > HISTORY_KEEP * 2 {
        let entries = load(path);
        let mut out = String::new();
        for entry in entries {
            out.push_str(&serde_json::to_string(&entry)?);
            out.push('\n');
        }
        let tmp = path.with_extension("tmp");
        private(OpenOptions::new().create(true).write(true).truncate(true))
            .open(&tmp)?
            .write_all(out.as_bytes())?;
        fs::rename(&tmp, path)?;
    }
    restrict(path)
}

#[cfg(unix)]
fn private(options: &mut OpenOptions) -> &mut OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600)
}

#[cfg(not(unix))]
fn private(options: &mut OpenOptions) -> &mut OpenOptions {
    // The state directory is under %LOCALAPPDATA%, only the user's.
    options
}

/// Owner-only, even for a file made before.
#[cfg(unix)]
fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_questions_between_runs_for_the_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("state").join("history");
        assert!(load(&file).is_empty());
        append(&file, "What did we budget for Q3?").unwrap();
        append(&file, "Two\nlines").unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(b"not json\n")
            .unwrap();
        assert_eq!(load(&file), ["What did we budget for Q3?", "Two\nlines"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn trims_the_file_to_the_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("history");
        for i in 0..HISTORY_KEEP * 2 + 1 {
            append(&file, &i.to_string()).unwrap();
        }
        let lines = fs::read_to_string(&file).unwrap().lines().count();
        assert_eq!(lines, HISTORY_KEEP);
        assert_eq!(
            load(&file).last().map(String::as_str),
            Some((HISTORY_KEEP * 2).to_string().as_str())
        );
    }
}
