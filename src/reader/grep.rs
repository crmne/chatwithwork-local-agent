//! Live grep fallback, for when the index has no answer yet.
//!
//! Matches the query as a case-insensitive literal, line by line, in text
//! files only. Every file is opened through the root handle with the same
//! policy as `read`, and the whole search stops at a time budget.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Instant;

use globset::GlobMatcher;
use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, SearcherBuilder, Sink, SinkMatch};

use super::extract::{Format, format_for, truncate_chars};
use super::safe_fs::{OpenPolicy, RootHandle, ToolPath};
use super::walk::walk_files;
use crate::policy::DenyList;

#[derive(Debug, Clone)]
pub struct GrepHit {
    pub path: String,
    pub title: String,
    pub modified: i64,
    pub size: u64,
    pub snippet: String,
}

pub struct GrepQuery<'a> {
    pub text: &'a str,
    pub glob: Option<&'a GlobMatcher>,
    pub modified_after: Option<i64>,
    pub limit: usize,
    pub snippet_chars: usize,
    pub max_file_bytes: u64,
    pub deadline: Instant,
}

pub fn grep(
    roots: &[Arc<RootHandle>],
    deny: &Arc<DenyList>,
    allow_hardlinks: bool,
    q: &GrepQuery<'_>,
) -> Vec<GrepHit> {
    let text = q.text.trim();
    if text.is_empty() || q.limit == 0 {
        return Vec::new();
    }
    let Ok(matcher) = RegexMatcherBuilder::new()
        .case_insensitive(true)
        .fixed_strings(true)
        .build(text)
    else {
        return Vec::new();
    };
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(b'\0'))
        .line_number(false)
        .build();
    let policy = OpenPolicy {
        deny,
        allow_hardlinks,
    };

    let mut hits = Vec::new();
    for root in roots {
        walk_files(root, deny, |file| {
            if Instant::now() >= q.deadline || hits.len() >= q.limit {
                return ControlFlow::Break(());
            }
            let rel = file.rel.as_string();
            if file.size > q.max_file_bytes
                || q.modified_after.is_some_and(|after| file.modified <= after)
                || q.glob.is_some_and(|g| !g.is_match(&rel))
                || format_for(&rel) != Some(Format::Text)
            {
                return ControlFlow::Continue(());
            }
            let Ok(opened) = root.open_file(&file.rel, policy) else {
                return ControlFlow::Continue(());
            };
            let mut sink = FirstMatch {
                matcher: &matcher,
                line: None,
            };
            if searcher
                .search_file(&matcher, &opened.file, &mut sink)
                .is_ok()
                && let Some(line) = sink.line
            {
                hits.push(GrepHit {
                    path: ToolPath::display(&root.id, &file.rel),
                    title: file.rel.file_name().unwrap_or_default().to_string(),
                    modified: file.modified,
                    size: file.size,
                    snippet: snippet_around(&line, text, q.snippet_chars),
                });
            }
            ControlFlow::Continue(())
        });
        if Instant::now() >= q.deadline || hits.len() >= q.limit {
            break;
        }
    }
    hits
}

struct FirstMatch<'a, M> {
    matcher: &'a M,
    line: Option<String>,
}

impl<M: Matcher> Sink for FirstMatch<'_, M> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &grep_searcher::Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        let _ = self.matcher;
        self.line = Some(String::from_utf8_lossy(mat.bytes()).trim().to_string());
        Ok(false)
    }
}

/// Cut `line` to `max_chars` characters around the first match.
fn snippet_around(line: &str, needle: &str, max_chars: usize) -> String {
    let lower = line.to_lowercase();
    let start_byte = lower.find(&needle.to_lowercase()).unwrap_or(0);
    // `to_lowercase` can change byte lengths; fall back to the start.
    let start_char = if lower.len() == line.len() {
        line[..start_byte.min(line.len())].chars().count()
    } else {
        0
    };
    let skip = start_char.saturating_sub(max_chars / 3);
    let tail: String = line.chars().skip(skip).collect();
    let mut out = truncate_chars(&tail, max_chars).to_string();
    if skip > 0 && !out.is_empty() {
        out.insert(0, '…');
        out = truncate_chars(&out, max_chars).to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_center_on_the_match() {
        let line = format!("{}needle and more", "x".repeat(500));
        let s = snippet_around(&line, "NEEDLE", 60);
        assert!(s.contains("needle"));
        assert!(s.chars().count() <= 60);
        assert_eq!(
            snippet_around("short needle", "needle", 300),
            "short needle"
        );
    }
}
