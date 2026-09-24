//! Walk a root with ripgrep's `ignore` crate.
//!
//! The walk never follows symlinks, respects `.gitignore` and `.ignore`,
//! skips hidden files, and prunes denied directories so their contents are
//! never visited. Files are yielded by relative path; callers open them
//! through [`RootHandle`](super::safe_fs::RootHandle), never by the walked
//! path, so a swap between walk and open can't escape the root.
//!
//! A walk can be limited to a [`Scope`]: a few directories that changed.
//! It still starts at the root and only descends along the way to them, so
//! ignore files in the directories above apply exactly as in a full walk.

use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

use ignore::WalkBuilder;

use super::safe_fs::{RelPath, RootHandle};
use crate::policy::DenyList;

pub struct WalkedFile {
    pub rel: RelPath,
    pub size: u64,
    pub modified: i64,
}

/// Part of a root to walk.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// Everything below this directory (or this file).
    Recursive(RelPath),
    /// Only the files directly inside this directory.
    Shallow(RelPath),
}

impl Target {
    /// Whether an entry at `rel` is visited: files inside the target, and
    /// the directories on the way there.
    fn admits(&self, rel: &[String], is_dir: bool) -> bool {
        let (target, recursive) = match self {
            Self::Recursive(t) => (t.components(), true),
            Self::Shallow(t) => (t.components(), false),
        };
        if is_dir && target.starts_with(rel) {
            return true;
        }
        if recursive {
            rel.starts_with(target)
        } else {
            !is_dir && rel.len() == target.len() + 1 && rel.starts_with(target)
        }
    }
}

/// A set of targets. An empty scope means nothing; use `None` for the whole
/// root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope(pub Vec<Target>);

impl Scope {
    /// Whether an indexed file at `rel` (a `/`-separated relative path) is
    /// covered by this scope.
    pub fn covers_file(&self, rel: &str) -> bool {
        let parts: Vec<String> = rel.split('/').map(str::to_string).collect();
        self.0.iter().any(|t| t.admits(&parts, false))
    }

    fn admits(&self, rel: &[String], is_dir: bool) -> bool {
        self.0.iter().any(|t| t.admits(rel, is_dir))
    }
}

pub fn walk_files(
    root: &RootHandle,
    deny: &Arc<DenyList>,
    visit: impl FnMut(WalkedFile) -> ControlFlow<()>,
) {
    walk_scoped(root, deny, None, visit);
}

pub fn walk_scoped(
    root: &RootHandle,
    deny: &Arc<DenyList>,
    scope: Option<Arc<Scope>>,
    mut visit: impl FnMut(WalkedFile) -> ControlFlow<()>,
) {
    let filter_deny = Arc::clone(deny);
    let root_path = root.path.clone();
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
        .filter_entry(move |entry| {
            if filter_deny.is_denied(entry.path()) {
                return false;
            }
            let Some(scope) = &scope else {
                return true;
            };
            let Some(rel) = relative(&root_path, entry.path()) else {
                return false;
            };
            if rel.is_empty() {
                return true;
            }
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            scope.admits(&rel, is_dir)
        })
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

fn relative(root: &Path, path: &Path) -> Option<Vec<String>> {
    let rel = path.strip_prefix(root).ok()?;
    rel.components()
        .map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    fn parts(s: &str) -> Vec<String> {
        s.split('/').map(str::to_string).collect()
    }

    #[test]
    fn targets_admit_their_files_and_the_way_there() {
        let deep = Target::Recursive(rel("a/b"));
        assert!(deep.admits(&parts("a"), true));
        assert!(deep.admits(&parts("a/b"), true));
        assert!(deep.admits(&parts("a/b/c/d.txt"), false));
        assert!(!deep.admits(&parts("a/x.txt"), false));
        assert!(!deep.admits(&parts("a/c"), true));

        let flat = Target::Shallow(rel("a"));
        assert!(flat.admits(&parts("a"), true));
        assert!(flat.admits(&parts("a/x.txt"), false));
        assert!(!flat.admits(&parts("a/sub"), true));
        assert!(!flat.admits(&parts("a/sub/y.txt"), false));
        assert!(!flat.admits(&parts("b.txt"), false));

        let top = Target::Shallow(RelPath::default());
        assert!(top.admits(&parts("x.txt"), false));
        assert!(!top.admits(&parts("a/x.txt"), false));

        let scope = Scope(vec![deep, flat]);
        assert!(scope.covers_file("a/b/c.txt"));
        assert!(scope.covers_file("a/x.txt"));
        assert!(!scope.covers_file("z.txt"));
    }

    #[test]
    fn scoped_walks_visit_only_the_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        for dir in ["a/b", "a/c", "d"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        for file in [
            "top.txt",
            "a/one.txt",
            "a/b/two.txt",
            "a/c/three.txt",
            "d/four.txt",
        ] {
            std::fs::write(base.join(file), "x").unwrap();
        }
        std::fs::write(base.join(".gitignore"), "ignored/\n").unwrap();
        std::fs::create_dir_all(base.join("a/b/ignored")).unwrap();
        std::fs::write(base.join("a/b/ignored/five.txt"), "x").unwrap();
        let root = RootHandle::open(&crate::config::Root {
            id: "docs".into(),
            label: "Docs".into(),
            path: base,
            follow_symlinks: false,
        })
        .unwrap();
        let deny = Arc::new(DenyList::new(Vec::<String>::new()).unwrap());
        let collect = |scope: Option<Scope>| {
            let mut seen = Vec::new();
            walk_scoped(&root, &deny, scope.map(Arc::new), |f| {
                seen.push(f.rel.as_string());
                ControlFlow::Continue(())
            });
            seen.sort();
            seen
        };
        assert_eq!(
            collect(None),
            [
                "a/b/two.txt",
                "a/c/three.txt",
                "a/one.txt",
                "d/four.txt",
                "top.txt"
            ]
        );
        assert_eq!(
            collect(Some(Scope(vec![Target::Recursive(rel("a/b"))]))),
            ["a/b/two.txt"],
            "the root's .gitignore still applies"
        );
        assert_eq!(
            collect(Some(Scope(vec![
                Target::Shallow(rel("a")),
                Target::Shallow(RelPath::default())
            ]))),
            ["a/one.txt", "top.txt"]
        );
    }
}
