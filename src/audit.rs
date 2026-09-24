//! The local audit log: one JSON object per line in
//! `~/.local/state/cww/audit.jsonl`.
//!
//! Every tool call is appended with its decision, whether it was answered or
//! refused. Connection events are logged too. The file is 0600, opened in
//! append mode, rotated at a size cap, and never uploaded.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const MAX_BYTES: u64 = 10 * 1024 * 1024;
const KEEP_ROTATED: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allowed,
    Denied,
    Error,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AuditEntry {
    pub ts: String,
    /// `tool` for tool calls; `connected`, `disconnected`, `paused`, ... for
    /// daemon events.
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Bytes of tool result sent to the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl AuditEntry {
    pub fn event(event: &str) -> Self {
        Self {
            ts: now(),
            event: event.to_string(),
            ..Self::default()
        }
    }
}

pub fn now() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

pub struct AuditLog {
    path: PathBuf,
    file: Mutex<File>,
}

impl AuditLog {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            crate::paths::ensure_private_dir(dir)?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(open_append(path)?),
        })
    }

    pub fn append(&self, entry: &AuditEntry) {
        if let Err(e) = self.try_append(entry) {
            tracing::error!("can't write the audit log: {e:#}");
        }
    }

    fn try_append(&self, entry: &AuditEntry) -> Result<()> {
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        let mut file = self.file.lock().expect("audit lock");
        if file.metadata().map(|m| m.len()).unwrap_or(0) + line.len() as u64 > MAX_BYTES {
            rotate(&self.path)?;
            *file = open_append(&self.path)?;
        }
        file.write_all(&line)?;
        Ok(())
    }
}

fn open_append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

fn rotated(path: &Path, n: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

fn rotate(path: &Path) -> Result<()> {
    for n in (1..KEEP_ROTATED).rev() {
        let from = rotated(path, n);
        if from.exists() {
            std::fs::rename(&from, rotated(path, n + 1))?;
        }
    }
    std::fs::rename(path, rotated(path, 1))?;
    Ok(())
}

/// One human-readable line for `cww log`.
pub fn format_line(line: &str) -> String {
    let Ok(e) = serde_json::from_str::<AuditEntry>(line) else {
        return line.to_string();
    };
    let mut out = format!("{} ", e.ts);
    if e.event == "tool" {
        let decision = match e.decision {
            Some(Decision::Allowed) => "ok",
            Some(Decision::Denied) => "DENIED",
            Some(Decision::Error) => "error",
            None => "?",
        };
        out.push_str(&format!(
            "{:<6} {:<6}",
            decision,
            e.tool.as_deref().unwrap_or("?")
        ));
        if let Some(path) = &e.path {
            out.push_str(&format!(" {path}"));
        }
        if let Some(query) = &e.query {
            out.push_str(&format!(" query={query:?}"));
        }
        if let Some(n) = e.results {
            out.push_str(&format!(" results={n}"));
        }
        if let Some(b) = e.bytes {
            out.push_str(&format!(" bytes={b}"));
        }
        if let Some(code) = &e.code {
            out.push_str(&format!(" code={code}"));
        }
        if let Some(reason) = &e.reason {
            out.push_str(&format!(" ({reason})"));
        }
        if let Some(chat) = &e.chat_id {
            out.push_str(&format!(" chat={chat}"));
        }
    } else {
        out.push_str(&e.event);
        if let Some(detail) = &e.detail {
            out.push_str(&format!(" {detail}"));
        }
    }
    out
}

/// Print the last `lines` entries, then keep printing new ones if `follow`.
pub fn print_log(path: &Path, lines: usize, follow: bool, raw: bool) -> Result<()> {
    let show = |line: &str| {
        if raw {
            println!("{line}");
        } else {
            println!("{}", format_line(line));
        }
    };
    let mut file = loop {
        match File::open(path) {
            Ok(f) => break f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && follow => {
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("No audit log yet at {}", path.display());
                return Ok(());
            }
            Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
        }
    };
    let all: Vec<String> = BufReader::new(&file)
        .lines()
        .map_while(Result::ok)
        .collect();
    for line in &all[all.len().saturating_sub(lines)..] {
        show(line);
    }
    if !follow {
        return Ok(());
    }
    let mut pos = file.seek(SeekFrom::End(0))?;
    let mut inode = file.metadata()?.ino();
    let mut partial = String::new();
    loop {
        std::thread::sleep(Duration::from_millis(300));
        // Reopen after rotation or truncation.
        if let Ok(meta) = std::fs::metadata(path)
            && (meta.ino() != inode || meta.len() < pos)
            && let Ok(f) = File::open(path)
        {
            file = f;
            inode = meta.ino();
            pos = 0;
        }
        file.seek(SeekFrom::Start(pos))?;
        let mut reader = BufReader::new(&file);
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf)?;
            if n == 0 {
                break;
            }
            pos += n as u64;
            if buf.ends_with('\n') {
                partial.push_str(buf.trim_end_matches('\n'));
                show(&partial);
                partial.clear();
            } else {
                partial.push_str(&buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn appends_private_json_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state/audit.jsonl");
        let log = AuditLog::open(&path).unwrap();
        let mut entry = AuditEntry::event("tool");
        entry.tool = Some("read".into());
        entry.path = Some("docs:a.md".into());
        entry.decision = Some(Decision::Denied);
        entry.code = Some("denied".into());
        log.append(&entry);
        log.append(&AuditEntry::event("connected"));

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let parsed: AuditEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed, entry);
        assert!(format_line(lines[0]).contains("DENIED read   docs:a.md"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
