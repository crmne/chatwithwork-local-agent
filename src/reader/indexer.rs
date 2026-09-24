//! Background indexing: an initial scan per root, file watching with
//! `notify`, and a periodic rescan to catch missed events.
//!
//! A scan compares each file's mtime and size with the index and only
//! extracts files that changed, so rescans are cheap. Watch events mark a
//! root dirty; dirty roots are rescanned after a short debounce.

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::extract::{self, format_for, read_capped};
use super::index::{MAX_BODY_CHARS, SearchIndex};
use super::safe_fs::{OpenPolicy, RootHandle};
use super::walk::walk_files;
use crate::policy::DenyList;

const DEBOUNCE: Duration = Duration::from_secs(3);
const COMMIT_EVERY: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    Pending,
    Indexing,
    Ready,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct RootIndexStatus {
    pub state: IndexState,
    pub files: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

pub type StatusMap = Arc<Mutex<HashMap<String, RootIndexStatus>>>;

pub struct IndexerSettings {
    pub roots: Vec<Arc<RootHandle>>,
    pub deny: Arc<DenyList>,
    pub allow_hardlinks: bool,
    pub max_file_bytes: u64,
    pub watch: bool,
    pub rescan: Duration,
}

enum Command {
    Configure(IndexerSettings),
    Changed(Vec<PathBuf>),
    Shutdown,
}

pub struct Indexer {
    tx: mpsc::Sender<Command>,
    status: StatusMap,
    thread: Option<JoinHandle<()>>,
}

impl Indexer {
    pub fn start(index: Arc<SearchIndex>, settings: IndexerSettings) -> Self {
        let (tx, rx) = mpsc::channel();
        let status: StatusMap = Arc::default();
        let worker = Worker {
            index,
            status: Arc::clone(&status),
            tx: tx.clone(),
            settings,
            watcher: None,
            dirty: HashSet::new(),
            last_event: None,
            last_full: Instant::now(),
        };
        let thread = std::thread::Builder::new()
            .name("cww-indexer".into())
            .spawn(move || worker.run(rx))
            .expect("spawning indexer thread");
        Self {
            tx,
            status,
            thread: Some(thread),
        }
    }

    pub fn configure(&self, settings: IndexerSettings) {
        let _ = self.tx.send(Command::Configure(settings));
    }

    pub fn status(&self) -> HashMap<String, RootIndexStatus> {
        self.status.lock().expect("status lock").clone()
    }

    /// True once every configured root has been scanned at least once.
    pub fn is_ready(&self, root_id: &str) -> bool {
        self.status
            .lock()
            .expect("status lock")
            .get(root_id)
            .is_some_and(|s| s.state == IndexState::Ready)
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Worker {
    index: Arc<SearchIndex>,
    status: StatusMap,
    tx: mpsc::Sender<Command>,
    settings: IndexerSettings,
    watcher: Option<notify::RecommendedWatcher>,
    dirty: HashSet<String>,
    last_event: Option<Instant>,
    last_full: Instant,
}

impl Worker {
    fn run(mut self, rx: mpsc::Receiver<Command>) {
        self.apply_settings();
        loop {
            let timeout = match self.last_event {
                Some(at) => DEBOUNCE.saturating_sub(at.elapsed()),
                None => self
                    .settings
                    .rescan
                    .saturating_sub(self.last_full.elapsed()),
            };
            match rx.recv_timeout(timeout) {
                Ok(Command::Configure(settings)) => {
                    self.settings = settings;
                    self.apply_settings();
                }
                Ok(Command::Changed(paths)) => {
                    for path in paths {
                        // Our own index and logs are denied; ignoring them
                        // also stops commits from retriggering scans.
                        if self.settings.deny.is_denied(&path) {
                            continue;
                        }
                        if let Some(root) = self
                            .settings
                            .roots
                            .iter()
                            .find(|r| path.starts_with(&r.path))
                        {
                            self.dirty.insert(root.id.clone());
                        }
                    }
                    if !self.dirty.is_empty() && self.last_event.is_none() {
                        self.last_event = Some(Instant::now());
                    }
                }
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    if self.last_event.take().is_some() {
                        let dirty: Vec<String> = self.dirty.drain().collect();
                        for id in dirty {
                            if let Some(root) = self.root(&id) {
                                self.scan(&root);
                            }
                        }
                    } else {
                        self.scan_all();
                    }
                }
            }
        }
        let _ = self.index.commit();
    }

    fn root(&self, id: &str) -> Option<Arc<RootHandle>> {
        self.settings.roots.iter().find(|r| r.id == id).cloned()
    }

    fn apply_settings(&mut self) {
        {
            let mut status = self.status.lock().expect("status lock");
            status.retain(|id, _| self.settings.roots.iter().any(|r| &r.id == id));
            for root in &self.settings.roots {
                status.entry(root.id.clone()).or_insert(RootIndexStatus {
                    state: IndexState::Pending,
                    files: 0,
                    last_error: None,
                });
            }
        }
        // Drop documents of roots that are no longer shared.
        if let Ok(indexed) = self.index.indexed_roots() {
            for id in indexed {
                if !self.settings.roots.iter().any(|r| r.id == id) {
                    self.index.delete_root(&id);
                }
            }
            let _ = self.index.commit();
        }
        self.watcher = None;
        if self.settings.watch {
            self.watcher = self.start_watcher();
        }
        self.scan_all();
    }

    fn start_watcher(&self) -> Option<notify::RecommendedWatcher> {
        use notify::{RecursiveMode, Watcher};

        let tx = self.tx.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if let Ok(event) = event
                    && !event.kind.is_access()
                {
                    let _ = tx.send(Command::Changed(event.paths));
                }
            })
            .map_err(|e| tracing::warn!("file watching unavailable: {e}"))
            .ok()?;
        for root in &self.settings.roots {
            if let Err(e) = watcher.watch(&root.path, RecursiveMode::Recursive) {
                tracing::warn!(root = %root.id, "can't watch root, relying on rescans: {e}");
            }
        }
        Some(watcher)
    }

    fn scan_all(&mut self) {
        self.last_full = Instant::now();
        self.dirty.clear();
        self.last_event = None;
        for root in self.settings.roots.clone() {
            self.scan(&root);
        }
    }

    fn set_status(&self, id: &str, state: IndexState, files: Option<u64>, error: Option<String>) {
        let mut status = self.status.lock().expect("status lock");
        if let Some(s) = status.get_mut(id) {
            s.state = state;
            if let Some(files) = files {
                s.files = files;
            }
            s.last_error = error;
        }
    }

    /// Bring the index for one root in line with the disk.
    fn scan(&self, root: &RootHandle) {
        let first = !matches!(
            self.status
                .lock()
                .expect("status lock")
                .get(&root.id)
                .map(|s| s.state),
            Some(IndexState::Ready)
        );
        if first {
            self.set_status(&root.id, IndexState::Indexing, None, None);
        }
        let mut known = match self.index.indexed_files(&root.id) {
            Ok(known) => known,
            Err(e) => {
                self.set_status(&root.id, IndexState::Error, None, Some(e.to_string()));
                return;
            }
        };
        let policy = OpenPolicy {
            deny: &self.settings.deny,
            allow_hardlinks: self.settings.allow_hardlinks,
        };
        let mut seen = 0u64;
        let mut pending = 0usize;
        walk_files(root, &self.settings.deny, |file| {
            let rel = file.rel.as_string();
            seen += 1;
            let unchanged = known
                .remove(&rel)
                .is_some_and(|(mtime, size)| mtime == file.modified && size == file.size);
            if unchanged {
                return ControlFlow::Continue(());
            }
            // Files the policy refuses to open (hard links, special files)
            // are left out entirely; files that can't be turned into text
            // are still findable by name.
            let body = if file.size <= self.settings.max_file_bytes && format_for(&rel).is_some() {
                match root.open_file(&file.rel, policy) {
                    Err(e) if e.code.is_denial() => {
                        self.index.delete(&root.id, &rel);
                        return ControlFlow::Continue(());
                    }
                    Err(_) => String::new(),
                    Ok(mut opened) => read_capped(&mut opened.file, self.settings.max_file_bytes)
                        .and_then(|bytes| extract::extract(&rel, &bytes, MAX_BODY_CHARS))
                        .unwrap_or_default(),
                }
            } else {
                String::new()
            };
            if let Err(e) = self
                .index
                .upsert(&root.id, &rel, &body, file.modified, file.size)
            {
                tracing::warn!(root = %root.id, "indexing failed: {e}");
            }
            pending += 1;
            if pending >= COMMIT_EVERY {
                let _ = self.index.commit();
                pending = 0;
            }
            ControlFlow::Continue(())
        });
        for rel in known.keys() {
            self.index.delete(&root.id, rel);
        }
        match self.index.commit() {
            Ok(()) => self.set_status(&root.id, IndexState::Ready, Some(seen), None),
            Err(e) => self.set_status(&root.id, IndexState::Error, Some(seen), Some(e.to_string())),
        }
    }
}
