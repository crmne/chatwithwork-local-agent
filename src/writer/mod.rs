//! Changes to shared files: create, write, edit, mkdir, move and delete,
//! in folders whose owner turned on "Allow changes".
//!
//! Every change runs one at a time and passes, in order: the folder allows
//! changes, the path rules reads follow (`root_id:relative/path`, no `..`,
//! no absolute paths), the deny list and the list of paths that are never
//! changed, the rules on names and kinds of file ([`kinds`]), the checks on
//! what is there now (no links, no hard links, no executables, no special
//! files, no other user's files), then the change budgets. The change itself
//! happens relative to a handle on the folder (see `safe_fs::ChangeDir`):
//! new content is written to a hidden file beside the target and flushed,
//! the old version goes to the system trash, and the new file is renamed
//! into place without replacing anything. Nothing is ever deleted for good.

pub mod kinds;
#[cfg(all(test, unix))]
mod tests;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, ToolError};
use crate::limits::Limiter;
use crate::reader::safe_fs::{
    ChangeDir, EntryKind, EntryStat, OpenPolicy, RelPath, RenameError, RootHandle, ToolPath,
    temp_name,
};
use crate::reader::{ChangeView, Reader, format_time};
use crate::trash::Trash;

/// The longest diff a dry run returns, in bytes.
const MAX_DIFF_BYTES: usize = 32 * 1024;

/// The umask the daemon started with, before it tightened its own: new
/// files and folders get the modes any other program would give them.
static UMASK: OnceLock<u32> = OnceLock::new();

/// Remember the umask the process had before the daemon changed it.
pub fn remember_umask(mask: u32) {
    let _ = UMASK.set(mask & 0o777);
}

fn new_file_mode() -> u32 {
    0o666 & !UMASK.get().copied().unwrap_or(0o022)
}

fn new_dir_mode() -> u32 {
    0o777 & !UMASK.get().copied().unwrap_or(0o022)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// A new file.
    Created,
    /// An existing file's content replaced; the old version is in the trash.
    Replaced,
    /// Text added at the end of a file; the old version is in the trash.
    Appended,
    /// One span of a file replaced; the old version is in the trash.
    Edited,
    /// A new folder.
    CreatedFolder,
    /// Moved or renamed.
    Moved,
    /// Moved to the system trash.
    Trashed,
    /// Nothing to do: the folder was already there.
    Unchanged,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Replaced => "replaced",
            Self::Appended => "appended",
            Self::Edited => "edited",
            Self::CreatedFolder => "created_folder",
            Self::Moved => "moved",
            Self::Trashed => "trashed",
            Self::Unchanged => "unchanged",
        }
    }
}

/// What was at a path before a change.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Previous {
    pub size: u64,
    pub modified: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// It went to the system trash on the user's computer.
    pub in_trash: bool,
}

/// The result of a change, as the server sees it, plus what only the local
/// audit log gets.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChangeResult {
    /// The path the change produced (the destination, for a move).
    pub path: String,
    pub effect: Effect,
    /// Nothing changed: this is what the call would do.
    pub dry_run: bool,
    /// Where a move came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// `file` or `dir`.
    pub kind: EntryKind,
    /// The file's size after the change, in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// SHA-256 of the file's content after the change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// What was replaced, edited or deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<Previous>,
    /// For dry runs of text changes: a unified diff of the change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// For folders moved or deleted: how many files and folders they hold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<usize>,
    /// Bytes written to disk. Local only.
    #[serde(skip)]
    pub written: u64,
    /// Where the old version went in the trash. Local only.
    #[serde(skip)]
    pub trashed_to: Option<PathBuf>,
}

impl ChangeResult {
    fn new(path: String, effect: Effect, kind: EntryKind, dry_run: bool) -> Self {
        Self {
            path,
            effect,
            dry_run,
            from: None,
            kind,
            size: None,
            sha256: None,
            previous: None,
            diff: None,
            entries: None,
            written: 0,
            trashed_to: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    #[default]
    Replace,
    Append,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteRequest {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub mode: WriteMode,
    #[serde(default)]
    pub expected_sha256: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditRequest {
    pub path: String,
    pub old_text: String,
    pub new_text: String,
    #[serde(default)]
    pub expected_sha256: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MkdirRequest {
    pub path: String,
    #[serde(default)]
    pub parents: bool,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveRequest {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub replace: bool,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteRequest {
    pub path: String,
    #[serde(default)]
    pub dry_run: bool,
}

/// Makes changes, one at a time.
pub struct Writer {
    reader: Arc<Reader>,
    limiter: Arc<Limiter>,
    trash: Trash,
    lock: Mutex<()>,
}

/// A path a change acts on: its folder, opened, and its name there.
struct Target {
    tool_path: String,
    name: String,
    /// The logical absolute path. Local use only.
    abs: PathBuf,
    dir: ChangeDir,
}

impl Target {
    fn name(&self) -> &OsStr {
        OsStr::new(&self.name)
    }
}

/// What a folder about to be moved or deleted holds.
#[derive(Debug, Default)]
struct Tree {
    entries: usize,
    bytes: u64,
    /// Anything a copy can't carry: links, special files, executables.
    uncopyable: bool,
}

fn sha256(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn not_writable(root: &RootHandle) -> ToolError {
    ToolError::new(
        ErrorCode::NotWritable,
        format!(
            "Changes aren't allowed in the shared folder “{}”. The person can turn on Allow \
             changes for it in the Local Agent on their computer.",
            root.label
        ),
    )
}

/// A unified diff of `old` to `new`, cut to [`MAX_DIFF_BYTES`].
fn unified_diff(path: &str, old: &str, new: &str) -> String {
    let diff = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_millis(500))
        .diff_lines(old, new);
    let mut text = diff
        .unified_diff()
        .context_radius(3)
        .header(path, path)
        .to_string();
    if text.len() > MAX_DIFF_BYTES {
        let mut cut = MAX_DIFF_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n… (the diff goes on)\n");
    }
    text
}

impl Writer {
    pub fn new(reader: Arc<Reader>, limiter: Arc<Limiter>, trash: Trash) -> Self {
        Self {
            reader,
            limiter,
            trash,
            lock: Mutex::new(()),
        }
    }

    /// Hold the writer: changes run one at a time. A change that panicked
    /// left nothing half done that the next one relies on.
    fn one_at_a_time(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether any shared folder allows changes, which decides whether the
    /// change tools are offered at all.
    pub fn enabled(&self) -> bool {
        self.reader.any_writable()
    }

    fn policy(view: &ChangeView) -> OpenPolicy<'_> {
        OpenPolicy {
            deny: &view.deny,
            allow_hardlinks: false,
        }
    }

    /// Both deny lists, for a path a change touches.
    fn check_denied(view: &ChangeView, abs: &Path) -> Result<(), ToolError> {
        if let Some(pattern) = view.deny.denied_by(abs) {
            return Err(ToolError::denied(format!(
                "This path is on the deny list ({pattern}), so it can't be read or changed."
            )));
        }
        if let Some(pattern) = view.write_deny.denied_by(abs) {
            return Err(ToolError::denied(format!(
                "cww never changes this path ({pattern}): a change there could make programs \
                 run on the computer."
            )));
        }
        Ok(())
    }

    /// Parse `path`, check its folder allows changes and both deny lists,
    /// and open the folder it is in.
    fn target(&self, view: &ChangeView, path: &str) -> Result<Target, ToolError> {
        let parsed = ToolPath::parse(path)?;
        let root = view.root(&parsed.root_id)?;
        if !root.writable {
            return Err(not_writable(&root));
        }
        let Some(name) = parsed.rel.file_name().map(str::to_string) else {
            return Err(ToolError::invalid_path(
                "The shared folder itself can't be changed; name something inside it.",
            ));
        };
        let abs = root.abs_path(&parsed.rel);
        Self::check_denied(view, &abs)?;
        let parent = RelPath::parse(
            &parsed.rel.components()[..parsed.rel.components().len() - 1].join("/"),
        )?;
        let dir = root
            .change_dir(&parent, Self::policy(view))
            .map_err(|e| match e.code {
                ErrorCode::NotFound => ToolError::new(
                    ErrorCode::NotFound,
                    format!(
                        "The folder {} doesn't exist. Make it with mkdir first.",
                        ToolPath::display(&root.id, &parent)
                    ),
                ),
                _ => e,
            })?;
        Ok(Target {
            tool_path: ToolPath::display(&root.id, &parsed.rel),
            name,
            abs,
            dir,
        })
    }

    /// Refuse an existing file a change would replace, edit or move.
    fn check_existing_file(target: &Target, stat: &EntryStat) -> Result<(), ToolError> {
        Self::check_file(target, stat, false)
    }

    /// The same, but programs and read-only files may go to the trash:
    /// their content isn't changed.
    fn check_file(target: &Target, stat: &EntryStat, trashing: bool) -> Result<(), ToolError> {
        match stat.kind {
            EntryKind::File => {}
            EntryKind::Dir => {
                return Err(ToolError::new(
                    ErrorCode::NotAFile,
                    format!("{} is a folder.", target.tool_path),
                ));
            }
            EntryKind::Symlink => {
                return Err(ToolError::denied(
                    "This path is a symlink, and cww never changes or follows links.",
                ));
            }
            EntryKind::Other => {
                return Err(ToolError::denied(
                    "Only regular files and folders can be changed.",
                ));
            }
        }
        if stat.links > 1 {
            return Err(ToolError::denied(
                "This file has more than one hard link, so it is never changed: the other \
                 names could be outside the shared folder.",
            ));
        }
        if stat.executable() && !trashing {
            return Err(ToolError::new(
                ErrorCode::NotChangeable,
                "This file is executable, and cww never changes programs.",
            ));
        }
        if stat.foreign {
            return Err(ToolError::denied(
                "This file belongs to another user on this computer, so it isn't changed.",
            ));
        }
        if stat.readonly && !trashing {
            return Err(ToolError::denied(
                "This file is marked read-only on the computer, so it isn't changed.",
            ));
        }
        Ok(())
    }

    fn check_sha(expected: Option<&str>, actual: &str) -> Result<(), ToolError> {
        match expected {
            Some(expected) if !expected.eq_ignore_ascii_case(actual) => Err(ToolError::new(
                ErrorCode::Conflict,
                "The file changed since it was checked (its SHA-256 is different), so nothing \
                 was changed. Look at it again.",
            )),
            _ => Ok(()),
        }
    }

    pub fn create(&self, req: &CreateRequest) -> Result<ChangeResult, ToolError> {
        let _one = self.one_at_a_time();
        let view = self.reader.change_view();
        let t = self.target(&view, &req.path)?;
        kinds::check_new_name(&t.name)?;
        kinds::check_text_name(&t.name)?;
        kinds::check_text(&req.content, view.limits.max_change_file_bytes)?;
        if t.dir.entry(t.name())?.is_some() {
            return Err(ToolError::new(
                ErrorCode::Exists,
                format!(
                    "{} already exists. Use write to replace a file, or pick another name.",
                    t.tool_path
                ),
            ));
        }
        self.create_file(&t, req.content.as_bytes(), req.dry_run)
    }

    /// Write `bytes` as the new file `t`.
    fn create_file(
        &self,
        t: &Target,
        bytes: &[u8],
        dry_run: bool,
    ) -> Result<ChangeResult, ToolError> {
        let mut result = ChangeResult::new(
            t.tool_path.clone(),
            Effect::Created,
            EntryKind::File,
            dry_run,
        );
        result.size = Some(bytes.len() as u64);
        result.sha256 = Some(sha256(bytes));
        if dry_run {
            return Ok(result);
        }
        self.limiter.check_change(bytes.len() as u64)?;
        let staged = t.dir.stage(bytes, new_file_mode())?;
        t.dir.commit(staged, t.name()).map_err(|e| match e.code {
            ErrorCode::Exists => ToolError::new(
                ErrorCode::Exists,
                format!("{} appeared while it was being created.", t.tool_path),
            ),
            _ => e,
        })?;
        result.written = bytes.len() as u64;
        Ok(result)
    }

    /// Replace the existing file `t` (described by `old`) with `bytes`:
    /// write the new version beside it, move the old one to the trash,
    /// then put the new one in place.
    fn replace_file(
        &self,
        t: &Target,
        old: &EntryStat,
        bytes: &[u8],
        result: &mut ChangeResult,
    ) -> Result<(), ToolError> {
        self.limiter.check_change(bytes.len() as u64)?;
        let mode = if old.mode == 0 {
            new_file_mode()
        } else {
            old.mode & 0o666
        };
        let staged = t.dir.stage(bytes, mode)?;
        let trashed = match t.dir.trash(t.name(), &self.trash, old) {
            Ok(path) => path,
            Err(e) => {
                t.dir.discard(staged);
                return Err(e);
            }
        };
        result.trashed_to = Some(trashed);
        t.dir.commit(staged, t.name()).map_err(|e| {
            ToolError::new(
                e.code,
                format!(
                    "{} The previous version is in the trash on the computer.",
                    e.message
                ),
            )
        })?;
        result.written = bytes.len() as u64;
        if let Some(previous) = &mut result.previous {
            previous.in_trash = true;
        }
        Ok(())
    }

    pub fn write(&self, req: &WriteRequest) -> Result<ChangeResult, ToolError> {
        let _one = self.one_at_a_time();
        let view = self.reader.change_view();
        let t = self.target(&view, &req.path)?;
        kinds::check_text_name(&t.name)?;
        kinds::check_text(&req.content, view.limits.max_change_file_bytes)?;
        let Some(stat) = t.dir.entry(t.name())? else {
            kinds::check_new_name(&t.name)?;
            if req.expected_sha256.is_some() {
                return Err(ToolError::new(
                    ErrorCode::Conflict,
                    format!("{} doesn't exist any more.", t.tool_path),
                ));
            }
            return self.create_file(&t, req.content.as_bytes(), req.dry_run);
        };
        Self::check_existing_file(&t, &stat)?;
        let (bytes, stat) = t.dir.read(t.name(), view.limits.max_change_file_bytes)?;
        Self::check_existing_file(&t, &stat)?;
        let old_sha = sha256(&bytes);
        Self::check_sha(req.expected_sha256.as_deref(), &old_sha)?;
        let old = kinds::as_text(bytes, &t.name)?;
        let (new, effect) = match req.mode {
            WriteMode::Replace => (req.content.clone(), Effect::Replaced),
            WriteMode::Append => (format!("{old}{}", req.content), Effect::Appended),
        };
        kinds::check_text(&new, view.limits.max_change_file_bytes)?;
        self.finish_text_change(&t, &stat, old_sha, &old, &new, effect, req.dry_run)
    }

    pub fn edit(&self, req: &EditRequest) -> Result<ChangeResult, ToolError> {
        let _one = self.one_at_a_time();
        if req.old_text.is_empty() {
            return Err(ToolError::invalid_argument(
                "old_text is empty. Quote the exact text to replace.",
            ));
        }
        if req.old_text == req.new_text {
            return Err(ToolError::invalid_argument(
                "old_text and new_text are the same, so there is nothing to change.",
            ));
        }
        let view = self.reader.change_view();
        let t = self.target(&view, &req.path)?;
        kinds::check_text_name(&t.name)?;
        kinds::check_text(&req.new_text, view.limits.max_change_file_bytes)?;
        let stat = t.dir.entry(t.name())?.ok_or_else(|| {
            ToolError::new(
                ErrorCode::NotFound,
                format!("{} doesn't exist.", t.tool_path),
            )
        })?;
        Self::check_existing_file(&t, &stat)?;
        let (bytes, stat) = t.dir.read(t.name(), view.limits.max_change_file_bytes)?;
        Self::check_existing_file(&t, &stat)?;
        let old_sha = sha256(&bytes);
        Self::check_sha(req.expected_sha256.as_deref(), &old_sha)?;
        let old = kinds::as_text(bytes, &t.name)?;
        let (from, to) = match old.matches(req.old_text.as_str()).count() {
            0 if old.contains("\r\n") && !req.old_text.contains('\r') => {
                // The file uses Windows line endings and the text doesn't.
                (
                    req.old_text.replace('\n', "\r\n"),
                    req.new_text.replace('\n', "\r\n"),
                )
            }
            _ => (req.old_text.clone(), req.new_text.clone()),
        };
        match old.matches(from.as_str()).count() {
            0 => {
                return Err(ToolError::invalid_argument(
                    "old_text was not found in the file. Read the file again and quote the text \
                     exactly, whitespace included.",
                ));
            }
            1 => {}
            n => {
                return Err(ToolError::invalid_argument(format!(
                    "old_text appears {n} times in the file. Include more of the surrounding \
                     text so it matches once."
                )));
            }
        }
        let new = old.replacen(from.as_str(), &to, 1);
        kinds::check_text(&new, view.limits.max_change_file_bytes)?;
        self.finish_text_change(&t, &stat, old_sha, &old, &new, Effect::Edited, req.dry_run)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_text_change(
        &self,
        t: &Target,
        stat: &EntryStat,
        old_sha: String,
        old: &str,
        new: &str,
        effect: Effect,
        dry_run: bool,
    ) -> Result<ChangeResult, ToolError> {
        let mut result = ChangeResult::new(t.tool_path.clone(), effect, EntryKind::File, dry_run);
        result.size = Some(new.len() as u64);
        result.sha256 = Some(sha256(new.as_bytes()));
        result.previous = Some(Previous {
            size: stat.size,
            modified: format_time(stat.modified),
            sha256: Some(old_sha),
            in_trash: false,
        });
        if dry_run {
            result.diff = Some(unified_diff(&t.tool_path, old, new));
            return Ok(result);
        }
        self.replace_file(t, stat, new.as_bytes(), &mut result)?;
        Ok(result)
    }

    pub fn mkdir(&self, req: &MkdirRequest) -> Result<ChangeResult, ToolError> {
        let _one = self.one_at_a_time();
        let view = self.reader.change_view();
        if !req.parents {
            let t = self.target(&view, &req.path)?;
            kinds::check_new_name(&t.name)?;
            let mut result = ChangeResult::new(
                t.tool_path.clone(),
                Effect::CreatedFolder,
                EntryKind::Dir,
                req.dry_run,
            );
            match t.dir.entry(t.name())? {
                Some(stat) if stat.kind == EntryKind::Dir => {
                    result.effect = Effect::Unchanged;
                    return Ok(result);
                }
                Some(_) => {
                    return Err(ToolError::new(
                        ErrorCode::Exists,
                        format!("{} already exists and isn't a folder.", t.tool_path),
                    ));
                }
                None => {}
            }
            if !req.dry_run {
                self.limiter.check_change(0)?;
                t.dir.make_dir(t.name(), new_dir_mode())?;
            }
            return Ok(result);
        }
        // With parents: make each missing folder on the way.
        let parsed = ToolPath::parse(&req.path)?;
        let root = view.root(&parsed.root_id)?;
        if !root.writable {
            return Err(not_writable(&root));
        }
        if parsed.rel.is_root() {
            return Err(ToolError::invalid_path(
                "The shared folder itself can't be changed; name something inside it.",
            ));
        }
        let mut dir = root.change_dir(&RelPath::default(), Self::policy(&view))?;
        let mut rel = RelPath::default();
        let mut missing = Vec::new();
        for (i, part) in parsed.rel.components().iter().enumerate() {
            rel = rel.join(part);
            Self::check_denied(&view, &root.abs_path(&rel))?;
            match dir.entry(OsStr::new(part))? {
                Some(stat) if stat.kind == EntryKind::Dir => {
                    dir = dir.subdir(OsStr::new(part))?;
                }
                Some(_) => {
                    return Err(ToolError::new(
                        ErrorCode::Exists,
                        format!(
                            "{} already exists and isn't a folder.",
                            ToolPath::display(&root.id, &rel)
                        ),
                    ));
                }
                None => {
                    // Everything from here down is new.
                    for rest in &parsed.rel.components()[i..] {
                        kinds::check_new_name(rest)?;
                    }
                    for rest in &parsed.rel.components()[i + 1..] {
                        rel = rel.join(rest);
                        Self::check_denied(&view, &root.abs_path(&rel))?;
                    }
                    missing = parsed.rel.components()[i..].to_vec();
                    break;
                }
            }
        }
        let mut result = ChangeResult::new(
            ToolPath::display(&root.id, &parsed.rel),
            if missing.is_empty() {
                Effect::Unchanged
            } else {
                Effect::CreatedFolder
            },
            EntryKind::Dir,
            req.dry_run,
        );
        result.entries = (!missing.is_empty()).then_some(missing.len());
        if req.dry_run || missing.is_empty() {
            return Ok(result);
        }
        self.limiter.check_change(0)?;
        for part in &missing {
            dir.make_dir(OsStr::new(part), new_dir_mode())?;
            dir = dir.subdir(OsStr::new(part))?;
        }
        Ok(result)
    }

    /// Look through a folder about to be moved or deleted: nothing in it may
    /// be on the deny list, and it may hold at most `max_change_entries`.
    fn inspect_tree(view: &ChangeView, dir: &ChangeDir, tree: &mut Tree) -> Result<(), ToolError> {
        for (name, stat) in dir.entries()? {
            tree.entries += 1;
            if tree.entries > view.limits.max_change_entries {
                return Err(ToolError::new(
                    ErrorCode::TooLarge,
                    format!(
                        "The folder holds more than {} files and folders, more than one change \
                         may move or delete.",
                        view.limits.max_change_entries
                    ),
                ));
            }
            let abs = dir.path.join(&name);
            if let Some(pattern) = view.deny.denied_by(&abs) {
                return Err(ToolError::denied(format!(
                    "The folder holds something on the deny list ({pattern}), so it can't be \
                     moved or deleted."
                )));
            }
            match stat.kind {
                EntryKind::Dir => {
                    let sub = dir.subdir(&name)?;
                    Self::inspect_tree(view, &sub, tree)?;
                }
                EntryKind::File => {
                    tree.bytes += stat.size;
                    if stat.links > 1 || stat.executable() || name.to_str().is_none() {
                        tree.uncopyable = true;
                    }
                }
                EntryKind::Symlink | EntryKind::Other => tree.uncopyable = true,
            }
        }
        Ok(())
    }

    /// Check an existing item a move or delete takes away.
    fn check_source(
        &self,
        view: &ChangeView,
        t: &Target,
        verb: &str,
    ) -> Result<(EntryStat, Option<Tree>), ToolError> {
        let stat = t.dir.entry(t.name())?.ok_or_else(|| {
            ToolError::new(
                ErrorCode::NotFound,
                format!("{} doesn't exist.", t.tool_path),
            )
        })?;
        match stat.kind {
            EntryKind::File => {
                Self::check_file(t, &stat, verb == "delete")?;
                Ok((stat, None))
            }
            EntryKind::Dir => {
                if stat.foreign {
                    return Err(ToolError::denied(
                        "This folder belongs to another user on this computer, so it isn't \
                         changed.",
                    ));
                }
                if view.configured.iter().any(|r| r.path.starts_with(&t.abs)) {
                    return Err(ToolError::denied(format!(
                        "This folder holds a shared folder, so it can't be {verb}d. The person \
                         can stop sharing that one first."
                    )));
                }
                let sub = t.dir.subdir(t.name())?;
                let mut tree = Tree::default();
                Self::inspect_tree(view, &sub, &mut tree)?;
                Ok((stat, Some(tree)))
            }
            EntryKind::Symlink => Err(ToolError::denied(
                "This path is a symlink, and cww never changes or follows links.",
            )),
            EntryKind::Other => Err(ToolError::denied(
                "Only regular files and folders can be changed.",
            )),
        }
    }

    pub fn delete(&self, req: &DeleteRequest) -> Result<ChangeResult, ToolError> {
        let _one = self.one_at_a_time();
        let view = self.reader.change_view();
        let t = self.target(&view, &req.path)?;
        let (stat, tree) = self.check_source(&view, &t, "delete")?;
        let mut result =
            ChangeResult::new(t.tool_path.clone(), Effect::Trashed, stat.kind, req.dry_run);
        result.entries = tree.as_ref().map(|t| t.entries);
        result.previous = Some(Previous {
            size: tree.as_ref().map_or(stat.size, |t| t.bytes),
            modified: format_time(stat.modified),
            sha256: None,
            in_trash: false,
        });
        if req.dry_run {
            return Ok(result);
        }
        self.limiter.check_change(0)?;
        result.trashed_to = Some(t.dir.trash(t.name(), &self.trash, &stat)?);
        if let Some(previous) = &mut result.previous {
            previous.in_trash = true;
        }
        Ok(result)
    }

    pub fn move_entry(&self, req: &MoveRequest) -> Result<ChangeResult, ToolError> {
        let _one = self.one_at_a_time();
        let view = self.reader.change_view();
        let src = self.target(&view, &req.from)?;
        let dst = self.target(&view, &req.to)?;
        if src.abs == dst.abs {
            return Err(ToolError::invalid_argument(
                "from and to are the same path.",
            ));
        }
        let (stat, tree) = self.check_source(&view, &src, "move")?;
        if stat.kind == EntryKind::Dir && dst.abs.starts_with(&src.abs) {
            return Err(ToolError::invalid_argument(
                "A folder can't be moved into itself.",
            ));
        }
        kinds::check_new_name(&dst.name)?;
        // The same item under another name, on a case-insensitive disk.
        let same_item = |other: &EntryStat| other.identity == stat.identity;
        let existing = dst.dir.entry(dst.name())?.filter(|e| !same_item(e));
        if let Some(existing) = &existing {
            if !req.replace {
                return Err(ToolError::new(
                    ErrorCode::Exists,
                    format!(
                        "{} already exists. Pass replace: true to move it to the trash and put \
                         this in its place.",
                        dst.tool_path
                    ),
                ));
            }
            if existing.kind != EntryKind::File || stat.kind != EntryKind::File {
                return Err(ToolError::new(
                    ErrorCode::Exists,
                    format!(
                        "{} already exists. Only a file can replace a file.",
                        dst.tool_path
                    ),
                ));
            }
            Self::check_existing_file(&dst, existing)?;
        }
        let mut result =
            ChangeResult::new(dst.tool_path.clone(), Effect::Moved, stat.kind, req.dry_run);
        result.from = Some(src.tool_path.clone());
        result.entries = tree.as_ref().map(|t| t.entries);
        result.size = (stat.kind == EntryKind::File).then_some(stat.size);
        result.previous = existing.as_ref().map(|e| Previous {
            size: e.size,
            modified: format_time(e.modified),
            sha256: None,
            in_trash: false,
        });
        let bytes = tree.as_ref().map_or(stat.size, |t| t.bytes);
        let across = src.dir.device() != dst.dir.device();
        if across && tree.as_ref().is_some_and(|t| t.uncopyable) {
            return Err(ToolError::new(
                ErrorCode::NotChangeable,
                "This folder holds links, special files or programs, so it can't be moved to \
                 another drive. It can be moved within its drive.",
            ));
        }
        if across && bytes > view.limits.max_change_file_bytes && tree.is_none() {
            return Err(ToolError::new(
                ErrorCode::TooLarge,
                format!(
                    "The file is {bytes} bytes; moving it to another drive copies it, and \
                     files up to {} bytes can be copied.",
                    view.limits.max_change_file_bytes
                ),
            ));
        }
        if req.dry_run {
            return Ok(result);
        }
        self.limiter.check_change(if across { bytes } else { 0 })?;
        if let Some(existing) = &existing {
            result.trashed_to = Some(dst.dir.trash(dst.name(), &self.trash, existing)?);
            if let Some(previous) = &mut result.previous {
                previous.in_trash = true;
            }
        }
        match src.dir.rename(src.name(), &dst.dir, dst.name()) {
            Ok(()) => {}
            Err(RenameError::CrossDevice) => {
                self.copy_across(&view, &src, &dst, &stat)?;
                result.written = bytes;
            }
            Err(RenameError::Exists) => {
                return Err(ToolError::new(
                    ErrorCode::Exists,
                    format!("{} appeared while moving.", dst.tool_path),
                ));
            }
            Err(RenameError::Other(e)) => return Err(e),
        }
        Ok(result)
    }

    /// Move to another filesystem: copy, then move the original to the
    /// trash. A copy that fails half way is removed; the original stays.
    fn copy_across(
        &self,
        view: &ChangeView,
        src: &Target,
        dst: &Target,
        stat: &EntryStat,
    ) -> Result<(), ToolError> {
        let cap = view.limits.max_change_file_bytes;
        match stat.kind {
            EntryKind::File => {
                let (bytes, read) = src.dir.read(src.name(), cap)?;
                if read.identity != stat.identity {
                    return Err(ToolError::new(
                        ErrorCode::Conflict,
                        "The file changed while it was being moved. Try again.",
                    ));
                }
                let staged = dst.dir.stage(&bytes, stat.mode & 0o666)?;
                dst.dir.commit(staged, dst.name())?;
            }
            EntryKind::Dir => {
                let temp = temp_name();
                dst.dir.make_dir(OsStr::new(&temp), new_dir_mode())?;
                let copied = (|| {
                    let from = src.dir.subdir(src.name())?;
                    let to = dst.dir.subdir(OsStr::new(&temp))?;
                    copy_tree(&from, &to, cap)?;
                    match dst.dir.rename(OsStr::new(&temp), &dst.dir, dst.name()) {
                        Ok(()) => Ok(()),
                        Err(RenameError::Exists) => Err(ToolError::new(
                            ErrorCode::Exists,
                            format!("{} appeared while moving.", dst.tool_path),
                        )),
                        Err(RenameError::CrossDevice) => {
                            Err(ToolError::internal("the copy landed on another drive"))
                        }
                        Err(RenameError::Other(e)) => Err(e),
                    }
                })();
                if let Err(e) = copied {
                    dst.dir.remove_own(OsStr::new(&temp));
                    return Err(e);
                }
            }
            _ => unreachable!("checked by check_source"),
        }
        if let Err(e) = src.dir.trash(src.name(), &self.trash, stat) {
            dst.dir.remove_own(dst.name());
            return Err(e);
        }
        Ok(())
    }
}

/// Copy the contents of `from` into `to`: regular files and folders only.
fn copy_tree(from: &ChangeDir, to: &ChangeDir, cap: u64) -> Result<(), ToolError> {
    for (name, stat) in from.entries()? {
        match stat.kind {
            EntryKind::File => {
                let (bytes, read) = from.read(&name, cap)?;
                let staged = to.stage(&bytes, read.mode & 0o666)?;
                to.commit(staged, &name)?;
            }
            EntryKind::Dir => {
                to.make_dir(
                    &name,
                    if stat.mode == 0 {
                        new_dir_mode()
                    } else {
                        stat.mode & 0o777
                    },
                )?;
                copy_tree(&from.subdir(&name)?, &to.subdir(&name)?, cap)?;
            }
            _ => {
                return Err(ToolError::new(
                    ErrorCode::NotChangeable,
                    "The folder holds links or special files, so it can't be moved to another \
                     drive.",
                ));
            }
        }
    }
    Ok(())
}
