//! Walk a root with ripgrep's `ignore` crate.
//!
//! The walk never follows symlinks, respects `.gitignore` and `.ignore`,
//! skips hidden files, and prunes denied directories so their contents are
//! never visited. Files are yielded by relative path; callers open them
//! through [`RootHandle`](super::safe_fs::RootHandle), never by the walked
//! path, so a swap between walk and open can't escape the root.

use std::ops::ControlFlow;
use std::sync::Arc;

use ignore::WalkBuilder;

use super::safe_fs::{RelPath, RootHandle};
use crate::policy::DenyList;

pub struct WalkedFile {
    pub rel: RelPath,
    pub size: u64,
    pub modified: i64,
}

pub fn walk_files(
    root: &RootHandle,
    deny: &Arc<DenyList>,
    mut visit: impl FnMut(WalkedFile) -> ControlFlow<()>,
) {
    let filter_deny = Arc::clone(deny);
    let walker = WalkBuilder::new(&root.path)
        .follow_links(false)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .ignore(true)
        .parents(false)
        .require_git(false)
        .same_file_system(false)
        .filter_entry(move |entry| !filter_deny.is_denied(entry.path()))
        .build();
    for entry in walker.flatten() {
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(&root.path) else {
            continue;
        };
        let Some(rel) = RelPath::from_relative_path(rel) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        let file = WalkedFile {
            rel,
            size: meta.len(),
            modified,
        };
        if visit(file).is_break() {
            break;
        }
    }
}
