//! The local full-text index (Tantivy, BM25).
//!
//! One document per file: `path` (`root_id:relative`), `root`, `title` (the
//! file name), `body` (extracted text, capped), `mtime` and `size`. The index
//! lives in `~/.local/share/cww/index` (0700) and never leaves the machine.

use std::collections::HashMap;
use std::ops::Bound;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use tantivy::collector::{Count, DocSetCollector, TopDocs};
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, RangeQuery, TermQuery};
use tantivy::schema::{
    Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TEXT, TantivyDocument, Value,
};
use tantivy::snippet::SnippetGenerator;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, Term, doc};

/// Bump when the schema changes; an index with another version is rebuilt.
const SCHEMA_VERSION: &str = "1";
/// Characters of extracted text stored per file.
pub const MAX_BODY_CHARS: usize = 2_000_000;
const WRITER_MEMORY: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy)]
struct Fields {
    path: Field,
    root: Field,
    title: Field,
    body: Field,
    mtime: Field,
    size: Field,
}

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
    fields: Fields,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexHit {
    pub path: String,
    pub title: String,
    pub modified: i64,
    pub size: u64,
    pub snippet: String,
    pub score: f32,
}

pub struct SearchQuery<'a> {
    pub text: &'a str,
    pub roots: &'a [String],
    pub modified_after: Option<i64>,
    pub limit: usize,
    pub snippet_chars: usize,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let fields = Fields {
        path: b.add_text_field("path", STRING | STORED),
        root: b.add_text_field("root", STRING),
        title: b.add_text_field("title", TEXT | STORED),
        body: b.add_text_field("body", TEXT | STORED),
        mtime: b.add_i64_field("mtime", INDEXED | STORED),
        size: b.add_u64_field("size", STORED),
    };
    (b.build(), fields)
}

impl SearchIndex {
    /// Open the index in `dir`, creating or rebuilding it if needed.
    pub fn open(dir: &Path) -> Result<Self> {
        crate::paths::ensure_private_dir(dir)?;
        let (schema, fields) = schema();
        let version_file = dir.join("cww-schema-version");
        let current = std::fs::read_to_string(&version_file).unwrap_or_default();
        let index = if current.trim() == SCHEMA_VERSION {
            Index::open_in_dir(dir).or_else(|_| recreate(dir, &schema))?
        } else {
            recreate(dir, &schema)?
        };
        std::fs::write(&version_file, SCHEMA_VERSION)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .context("opening index reader")?;
        let writer = index
            .writer_with_num_threads(1, WRITER_MEMORY)
            .context("opening index writer (is another cww daemon running?)")?;
        Ok(Self {
            index,
            reader,
            writer: Mutex::new(writer),
            fields,
        })
    }

    /// `relative path -> (mtime, size)` for every indexed file of a root.
    pub fn indexed_files(&self, root_id: &str) -> Result<HashMap<String, (i64, u64)>> {
        let searcher = self.reader.searcher();
        let query = TermQuery::new(
            Term::from_field_text(self.fields.root, root_id),
            IndexRecordOption::Basic,
        );
        let docs = searcher.search(&query, &DocSetCollector)?;
        let prefix = format!("{root_id}:");
        let mut out = HashMap::with_capacity(docs.len());
        for address in docs {
            let doc: TantivyDocument = searcher.doc(address)?;
            let Some(path) = doc.get_first(self.fields.path).and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(rel) = path.strip_prefix(&prefix) else {
                continue;
            };
            let mtime = doc
                .get_first(self.fields.mtime)
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let size = doc
                .get_first(self.fields.size)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            out.insert(rel.to_string(), (mtime, size));
        }
        Ok(out)
    }

    pub fn upsert(
        &self,
        root_id: &str,
        rel: &str,
        body: &str,
        mtime: i64,
        size: u64,
    ) -> Result<()> {
        let path = format!("{root_id}:{rel}");
        let title = rel.rsplit('/').next().unwrap_or(rel);
        let body = crate::reader::extract::truncate_chars(body, MAX_BODY_CHARS);
        let writer = self.writer.lock().expect("index writer lock");
        writer.delete_term(Term::from_field_text(self.fields.path, &path));
        writer.add_document(doc!(
            self.fields.path => path.as_str(),
            self.fields.root => root_id,
            self.fields.title => title,
            self.fields.body => body,
            self.fields.mtime => mtime,
            self.fields.size => size,
        ))?;
        Ok(())
    }

    pub fn delete(&self, root_id: &str, rel: &str) {
        let path = format!("{root_id}:{rel}");
        let writer = self.writer.lock().expect("index writer lock");
        writer.delete_term(Term::from_field_text(self.fields.path, &path));
    }

    pub fn delete_root(&self, root_id: &str) {
        let writer = self.writer.lock().expect("index writer lock");
        writer.delete_term(Term::from_field_text(self.fields.root, root_id));
    }

    /// Roots that have documents in the index.
    pub fn indexed_roots(&self) -> Result<Vec<String>> {
        let searcher = self.reader.searcher();
        let mut roots = Vec::new();
        for segment in searcher.segment_readers() {
            let inverted = segment.inverted_index(self.fields.root)?;
            let mut terms = inverted.terms().stream()?;
            while terms.advance() {
                if let Ok(root) = std::str::from_utf8(terms.key())
                    && !roots.iter().any(|r| r == root)
                {
                    roots.push(root.to_string());
                }
            }
        }
        // The term dictionary keeps deleted terms until segments merge.
        roots.retain(|root| {
            let query = TermQuery::new(
                Term::from_field_text(self.fields.root, root),
                IndexRecordOption::Basic,
            );
            searcher.search(&query, &Count).unwrap_or(0) > 0
        });
        Ok(roots)
    }

    pub fn commit(&self) -> Result<()> {
        self.writer.lock().expect("index writer lock").commit()?;
        self.reader.reload()?;
        Ok(())
    }

    pub fn doc_count(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    pub fn search(&self, q: &SearchQuery<'_>) -> Result<Vec<IndexHit>> {
        if q.roots.is_empty() || q.limit == 0 {
            return Ok(Vec::new());
        }
        let searcher = self.reader.searcher();
        let mut parser =
            QueryParser::for_index(&self.index, vec![self.fields.title, self.fields.body]);
        parser.set_field_boost(self.fields.title, 2.0);
        let (text_query, _errors) = parser.parse_query_lenient(q.text);

        let root_filter: Vec<(Occur, Box<dyn Query>)> = q
            .roots
            .iter()
            .map(|r| {
                let term = Term::from_field_text(self.fields.root, r);
                (
                    Occur::Should,
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                )
            })
            .collect();
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![
            (Occur::Must, text_query.box_clone()),
            (Occur::Must, Box::new(BooleanQuery::new(root_filter))),
        ];
        if let Some(after) = q.modified_after {
            clauses.push((
                Occur::Must,
                Box::new(RangeQuery::new(
                    Bound::Excluded(Term::from_field_i64(self.fields.mtime, after)),
                    Bound::Unbounded,
                )),
            ));
        }
        let query = BooleanQuery::new(clauses);
        let top = searcher.search(&query, &TopDocs::with_limit(q.limit).order_by_score())?;

        let mut snippets = SnippetGenerator::create(&searcher, &*text_query, self.fields.body)?;
        snippets.set_max_num_chars(q.snippet_chars);
        let mut hits = Vec::with_capacity(top.len());
        for (score, address) in top {
            let doc: TantivyDocument = searcher.doc(address)?;
            let get_str = |f: Field| {
                doc.get_first(f)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            let snippet = snippets.snippet_from_doc(&doc);
            let mut text = snippet.fragment().trim().to_string();
            if text.is_empty() {
                let body = get_str(self.fields.body);
                text = crate::reader::extract::truncate_chars(body.trim(), q.snippet_chars)
                    .to_string();
            }
            hits.push(IndexHit {
                path: get_str(self.fields.path),
                title: get_str(self.fields.title),
                modified: doc
                    .get_first(self.fields.mtime)
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
                size: doc
                    .get_first(self.fields.size)
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                snippet: crate::reader::extract::truncate_chars(&text, q.snippet_chars).to_string(),
                score,
            });
        }
        Ok(hits)
    }
}

fn recreate(dir: &Path, schema: &Schema) -> Result<Index> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Index::create_in_dir(dir, schema.clone()).context("creating search index")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_searches_and_deletes() {
        let tmp = tempfile::tempdir().unwrap();
        let index = SearchIndex::open(tmp.path()).unwrap();
        index
            .upsert(
                "docs",
                "plans/q3.md",
                "The quarterly budget is approved.",
                100,
                10,
            )
            .unwrap();
        index
            .upsert("docs", "old.md", "An old quarterly note.", 10, 5)
            .unwrap();
        index
            .upsert("other", "budget.txt", "Budget for other root.", 100, 5)
            .unwrap();
        index.commit().unwrap();

        let roots = vec!["docs".to_string()];
        let hits = index
            .search(&SearchQuery {
                text: "quarterly budget",
                roots: &roots,
                modified_after: None,
                limit: 10,
                snippet_chars: 300,
            })
            .unwrap();
        assert_eq!(hits[0].path, "docs:plans/q3.md");
        assert!(hits[0].snippet.contains("quarterly"));
        assert!(hits.iter().all(|h| h.path.starts_with("docs:")));

        let recent = index
            .search(&SearchQuery {
                text: "quarterly",
                roots: &roots,
                modified_after: Some(50),
                limit: 10,
                snippet_chars: 300,
            })
            .unwrap();
        assert_eq!(recent.len(), 1);

        let files = index.indexed_files("docs").unwrap();
        assert_eq!(files.get("old.md"), Some(&(10, 5)));

        index.delete("docs", "old.md");
        index.delete_root("other");
        index.commit().unwrap();
        assert_eq!(index.doc_count(), 1);
        assert_eq!(index.indexed_roots().unwrap(), vec!["docs".to_string()]);
    }

    #[test]
    fn reopens_existing_index() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let index = SearchIndex::open(tmp.path()).unwrap();
            index.upsert("docs", "a.md", "alpha", 1, 1).unwrap();
            index.commit().unwrap();
        }
        let index = SearchIndex::open(tmp.path()).unwrap();
        assert_eq!(index.doc_count(), 1);
    }
}
