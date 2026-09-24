//! The reader: everything that touches shared files.
//!
//! This module has a deliberately narrow interface, four operations with
//! plain serializable requests and responses ([`Reader::roots`],
//! [`Reader::search`], [`Reader::list`], [`Reader::read`]). In phase 3 it
//! moves into its own sandboxed process (Landlock/seccomp, Seatbelt) with no
//! network access, and the network side talks to it over a socketpair using
//! these same types. Nothing else in the daemon opens user files.

pub mod extract;
pub mod grep;
pub mod index;
pub mod indexer;
pub mod safe_fs;
pub mod walk;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};

use crate::config::{Config, Limits};
use crate::error::{ErrorCode, ToolError};
use crate::paths::Paths;
use crate::policy::DenyList;
use crate::status::Changes;
use indexer::{IndexState, Indexer, IndexerSettings};
use safe_fs::{EntryKind, OpenPolicy, RootHandle, ToolPath};

/// Characters of extracted text kept per file for paged reads.
const MAX_EXTRACTED_CHARS: usize = 20_000_000;
const CACHE_ENTRIES: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RootInfo {
    pub id: String,
    pub label: String,
    /// False if the folder is missing or unreadable right now.
    pub available: bool,
    /// `pending`, `indexing`, `ready`, `error` or `disabled`.
    pub index: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_files: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub path_glob: Option<String>,
    #[serde(default)]
    pub modified_after: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub modified: String,
    pub size: u64,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    /// `index`, `grep`, or `index+grep`.
    pub engine: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListRequest {
    pub path: String,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListEntry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListResponse {
    pub path: String,
    pub entries: Vec<ListEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    pub path: String,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub max_chars: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadResponse {
    pub path: String,
    pub text: String,
    pub offset: usize,
    /// Offset of the next chunk, or null at the end of the file.
    pub next_offset: Option<usize>,
    pub total_chars: usize,
    pub modified: String,
    pub size: u64,
}

struct State {
    roots: Vec<Arc<RootHandle>>,
    configured: Vec<crate::config::Root>,
    deny: Arc<DenyList>,
    allow_hardlinks: bool,
    limits: Limits,
    index_enabled: bool,
}

type CacheKey = (String, (u64, u64), i64, u64);

pub struct Reader {
    state: RwLock<State>,
    index: Option<Arc<index::SearchIndex>>,
    indexer: Option<Indexer>,
    cache: Mutex<VecDeque<(CacheKey, Arc<String>)>>,
}

impl Reader {
    pub fn new(config: &Config, paths: &Paths, changes: Changes) -> Result<Self> {
        let state = build_state(config, paths)?;
        let (index, indexer) = if config.index.enabled {
            let index = Arc::new(index::SearchIndex::open(&paths.index_dir())?);
            let indexer = Indexer::start(
                Arc::clone(&index),
                indexer_settings(&state, config),
                changes,
            );
            (Some(index), Some(indexer))
        } else {
            (None, None)
        };
        Ok(Self {
            state: RwLock::new(state),
            index,
            indexer,
            cache: Mutex::new(VecDeque::new()),
        })
    }

    /// Apply a changed config (roots, deny list, limits).
    pub fn reload(&self, config: &Config, paths: &Paths) -> Result<()> {
        let mut state = build_state(config, paths)?;
        state.index_enabled = self.index.is_some();
        if let Some(indexer) = &self.indexer {
            indexer.configure(indexer_settings(&state, config));
        }
        *self.state.write().expect("reader state") = state;
        self.cache.lock().expect("cache").clear();
        Ok(())
    }

    pub fn roots(&self) -> Vec<RootInfo> {
        let state = self.state.read().expect("reader state");
        let status = self
            .indexer
            .as_ref()
            .map(Indexer::status)
            .unwrap_or_default();
        state
            .configured
            .iter()
            .map(|root| {
                let available = state.roots.iter().any(|r| r.id == root.id);
                let s = status.get(&root.id);
                RootInfo {
                    id: root.id.clone(),
                    label: root.label.clone(),
                    available,
                    index: match (state.index_enabled, s.map(|s| s.state)) {
                        (false, _) => "disabled".into(),
                        (true, Some(IndexState::Ready)) => "ready".into(),
                        (true, Some(IndexState::Indexing)) => "indexing".into(),
                        (true, Some(IndexState::Error)) => "error".into(),
                        (true, _) => "pending".into(),
                    },
                    indexed_files: s.map(|s| s.files),
                }
            })
            .collect()
    }

    pub fn search(&self, req: &SearchRequest) -> Result<SearchResponse, ToolError> {
        let state = self.state.read().expect("reader state");
        let query = req.query.trim();
        if query.is_empty() {
            return Err(ToolError::invalid_argument("query must not be empty"));
        }
        if query.chars().count() > 1000 {
            return Err(ToolError::invalid_argument("query is too long"));
        }
        let limit = req
            .limit
            .unwrap_or(state.limits.search_hits)
            .clamp(1, state.limits.search_hits);
        let roots: Vec<Arc<RootHandle>> = match &req.root {
            Some(id) => vec![root_by_id(&state, id)?],
            None => state.roots.clone(),
        };
        let modified_after = req.modified_after.as_deref().map(parse_time).transpose()?;
        let glob = req.path_glob.as_deref().map(compile_glob).transpose()?;
        let glob_matches = |rel: &str| match &glob {
            None => true,
            Some((matcher, whole_path)) => {
                if *whole_path {
                    matcher.is_match(rel)
                } else {
                    matcher.is_match(rel.rsplit('/').next().unwrap_or(rel))
                }
            }
        };

        let mut hits: Vec<SearchHit> = Vec::new();
        let mut engines = Vec::new();
        if let Some(index) = &self.index {
            let ids: Vec<String> = roots.iter().map(|r| r.id.clone()).collect();
            let fetch = if glob.is_some() {
                (limit * 10).min(500)
            } else {
                limit * 2
            };
            let found = index
                .search(&index::SearchQuery {
                    text: query,
                    roots: &ids,
                    modified_after,
                    limit: fetch,
                    snippet_chars: state.limits.snippet_chars,
                })
                .map_err(|e| ToolError::internal(format!("search failed: {e}")))?;
            engines.push("index");
            for hit in found {
                if hits.len() >= limit {
                    break;
                }
                let Ok(path) = ToolPath::parse(&hit.path) else {
                    continue;
                };
                let Some(root) = roots.iter().find(|r| r.id == path.root_id) else {
                    continue;
                };
                // The deny list may have grown since the file was indexed.
                if state.deny.is_denied(&root.abs_path(&path.rel))
                    || !glob_matches(&path.rel.as_string())
                {
                    continue;
                }
                hits.push(SearchHit {
                    path: hit.path,
                    title: hit.title,
                    modified: format_time(hit.modified),
                    size: hit.size,
                    snippet: hit.snippet,
                });
            }
        }

        let not_ready = self
            .indexer
            .as_ref()
            .is_none_or(|ix| roots.iter().any(|r| !ix.is_ready(&r.id)));
        if hits.len() < limit && (hits.is_empty() || not_ready) {
            let file_glob = glob.as_ref().filter(|(_, whole)| *whole).map(|(m, _)| m);
            let name_glob = glob.as_ref().filter(|(_, whole)| !*whole).map(|(m, _)| m);
            let found = grep::grep(
                &roots,
                &state.deny,
                state.allow_hardlinks,
                &grep::GrepQuery {
                    text: query,
                    glob: file_glob,
                    modified_after,
                    limit: limit * 2,
                    snippet_chars: state.limits.snippet_chars,
                    max_file_bytes: state.limits.max_index_file_bytes,
                    deadline: Instant::now() + Duration::from_millis(state.limits.grep_timeout_ms),
                },
            );
            engines.push("grep");
            for hit in found {
                if hits.len() >= limit {
                    break;
                }
                if name_glob.is_some_and(|g| !g.is_match(&hit.title))
                    || hits.iter().any(|h| h.path == hit.path)
                {
                    continue;
                }
                hits.push(SearchHit {
                    path: hit.path,
                    title: hit.title,
                    modified: format_time(hit.modified),
                    size: hit.size,
                    snippet: hit.snippet,
                });
            }
        }
        Ok(SearchResponse {
            hits,
            engine: engines.join("+"),
        })
    }

    pub fn list(&self, req: &ListRequest) -> Result<ListResponse, ToolError> {
        let state = self.state.read().expect("reader state");
        let path = ToolPath::parse(&req.path)?;
        let root = root_by_id(&state, &path.root_id)?;
        let entries = root.list_dir(&path.rel, policy(&state))?;
        let start = match &req.cursor {
            None => 0,
            Some(c) => c
                .parse::<usize>()
                .map_err(|_| ToolError::invalid_argument("invalid cursor"))?,
        };
        let page = state.limits.list_page;
        let end = (start + page).min(entries.len());
        let next_cursor = (end < entries.len()).then(|| end.to_string());
        let entries = entries
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .map(|e| ListEntry {
                path: ToolPath::display(&root.id, &path.rel.join(&e.name)),
                name: e.name.clone(),
                kind: e.kind,
                size: e.size,
                modified: e.modified.map(format_time),
            })
            .collect();
        Ok(ListResponse {
            path: ToolPath::display(&root.id, &path.rel),
            entries,
            next_cursor,
        })
    }

    pub fn read(&self, req: &ReadRequest) -> Result<ReadResponse, ToolError> {
        let state = self.state.read().expect("reader state");
        let path = ToolPath::parse(&req.path)?;
        if path.rel.is_root() {
            return Err(ToolError::new(
                ErrorCode::NotAFile,
                "this path is a directory",
            ));
        }
        let root = root_by_id(&state, &path.root_id)?;
        let max_chars = req
            .max_chars
            .unwrap_or(state.limits.read_chars_per_call)
            .clamp(1, state.limits.read_chars_per_call);
        let offset = req.offset.unwrap_or(0);

        let mut opened = root.open_file(&path.rel, policy(&state))?;
        if opened.size > state.limits.max_file_bytes {
            return Err(ToolError::new(
                ErrorCode::TooLarge,
                format!(
                    "the file is {} bytes; the limit is {}",
                    opened.size, state.limits.max_file_bytes
                ),
            ));
        }
        let key: CacheKey = (
            path.to_string(),
            opened.identity,
            opened.modified,
            opened.size,
        );
        let text = match self.cached(&key) {
            Some(text) => text,
            None => {
                let bytes = extract::read_capped(&mut opened.file, state.limits.max_file_bytes)?;
                let name = path.rel.file_name().unwrap_or_default();
                let text = Arc::new(extract::extract(name, &bytes, MAX_EXTRACTED_CHARS)?);
                self.store(key, Arc::clone(&text));
                text
            }
        };

        let total_chars = text.chars().count();
        if offset > total_chars {
            return Err(ToolError::invalid_argument(format!(
                "offset {offset} is past the end ({total_chars} characters)"
            )));
        }
        let chunk: String = text.chars().skip(offset).take(max_chars).collect();
        let end = offset + chunk.chars().count();
        Ok(ReadResponse {
            path: path.to_string(),
            text: chunk,
            offset,
            next_offset: (end < total_chars).then_some(end),
            total_chars,
            modified: format_time(opened.modified),
            size: opened.size,
        })
    }

    fn cached(&self, key: &CacheKey) -> Option<Arc<String>> {
        let cache = self.cache.lock().expect("cache");
        cache
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| Arc::clone(v))
    }

    fn store(&self, key: CacheKey, text: Arc<String>) {
        let mut cache = self.cache.lock().expect("cache");
        cache.retain(|(k, _)| k.0 != key.0);
        cache.push_front((key, text));
        cache.truncate(CACHE_ENTRIES);
    }
}

fn build_state(config: &Config, paths: &Paths) -> Result<State> {
    let deny = Arc::new(DenyList::from_config(&config.deny, paths)?);
    let mut roots = Vec::new();
    for root in &config.roots {
        match RootHandle::open(root) {
            Ok(handle) => roots.push(Arc::new(handle)),
            Err(e) => tracing::warn!(root = %root.id, "root unavailable: {e}"),
        }
    }
    Ok(State {
        roots,
        configured: config.roots.clone(),
        deny,
        allow_hardlinks: config.deny.allow_hardlinks,
        limits: config.limits.clone(),
        index_enabled: config.index.enabled,
    })
}

fn indexer_settings(state: &State, config: &Config) -> IndexerSettings {
    IndexerSettings {
        roots: state.roots.clone(),
        deny: Arc::clone(&state.deny),
        allow_hardlinks: state.allow_hardlinks,
        max_file_bytes: state.limits.max_index_file_bytes,
        watch: config.index.watch,
        rescan: Duration::from_secs(config.index.rescan_secs.max(60)),
    }
}

fn policy(state: &State) -> OpenPolicy<'_> {
    OpenPolicy {
        deny: &state.deny,
        allow_hardlinks: state.allow_hardlinks,
    }
}

fn root_by_id(state: &State, id: &str) -> Result<Arc<RootHandle>, ToolError> {
    if let Some(root) = state.roots.iter().find(|r| r.id == id) {
        return Ok(Arc::clone(root));
    }
    if state.configured.iter().any(|r| r.id == id) {
        return Err(ToolError::new(
            ErrorCode::NotFound,
            "this folder is not available on the computer right now",
        ));
    }
    Err(ToolError::new(
        ErrorCode::UnknownRoot,
        format!("no shared folder has the ID {id:?}; call roots for the IDs"),
    ))
}

/// Compile a `path_glob`. Globs without `/` match the file name; globs with
/// `/` match the whole relative path.
fn compile_glob(glob: &str) -> Result<(GlobMatcher, bool), ToolError> {
    if glob.len() > 256 {
        return Err(ToolError::invalid_argument("path_glob is too long"));
    }
    let whole_path = glob.contains('/');
    let matcher = GlobBuilder::new(glob)
        .case_insensitive(true)
        .literal_separator(true)
        .build()
        .map_err(|e| ToolError::invalid_argument(format!("invalid path_glob: {e}")))?
        .compile_matcher();
    Ok((matcher, whole_path))
}

/// Accepts RFC 3339 timestamps and plain `YYYY-MM-DD` dates (UTC).
pub fn parse_time(s: &str) -> Result<i64, ToolError> {
    use time::format_description::well_known::Rfc3339;
    if let Ok(t) = time::OffsetDateTime::parse(s, &Rfc3339) {
        return Ok(t.unix_timestamp());
    }
    let format = time::macros::format_description!("[year]-[month]-[day]");
    time::Date::parse(s, &format)
        .map(|d| d.midnight().assume_utc().unix_timestamp())
        .map_err(|_| {
            ToolError::invalid_argument("modified_after must be an RFC 3339 time or YYYY-MM-DD")
        })
}

pub fn format_time(secs: i64) -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_times() {
        assert_eq!(parse_time("1970-01-02").unwrap(), 86_400);
        assert_eq!(parse_time("1970-01-01T00:01:00Z").unwrap(), 60);
        assert!(parse_time("yesterday").is_err());
        assert_eq!(format_time(60), "1970-01-01T00:01:00Z");
    }

    #[test]
    fn globs() {
        let (m, whole) = compile_glob("*.MD").unwrap();
        assert!(!whole && m.is_match("notes.md"));
        let (m, whole) = compile_glob("plans/**/*.pdf").unwrap();
        assert!(whole && m.is_match("plans/2026/q3.pdf") && !m.is_match("other/q3.pdf"));
    }
}
