use std::path::Path;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::{Index, IndexWriter, ReloadPolicy, TantivyDocument, Term, doc};

use crate::db::Reflection;
use crate::error::{LegionError, Result};

/// Maximum number of retries when acquiring the Tantivy index writer.
/// Another process (e.g., a concurrent hook) may hold the lock briefly.
const WRITER_RETRIES: u32 = 3;

/// Base delay between writer acquisition retries (doubles each attempt).
const WRITER_RETRY_BASE_MS: u64 = 100;

/// Full-text search index backed by Tantivy with BM25 scoring.
///
/// Indexes reflection text for retrieval by keyword similarity.
/// Documents can optionally be filtered by repo (exact match) and
/// are ranked by BM25 score on the text field (tokenized, stemmed).
pub struct SearchIndex {
    index: Index,
    id_field: Field,
    repo_field: Field,
    text_field: Field,
}

/// A single search result with its document ID and BM25 relevance score.
pub struct SearchResult {
    pub id: String,
    pub score: f32,
}

impl SearchIndex {
    /// Open or create a Tantivy index at the given directory path.
    ///
    /// Uses a three-stage fallback: try to open an existing index, try to
    /// create a new one, or wipe corrupted files and recreate. After a
    /// wipe-and-recreate, the index starts empty -- run `legion reindex`
    /// to repopulate from the database.
    ///
    /// Schema fields:
    /// - `id`: STRING | STORED -- exact match, retrievable after search
    /// - `repo`: STRING -- exact match filtering per repository
    /// - `text`: TEXT -- tokenized with English stemmer, BM25 scored
    pub fn open(path: &Path) -> Result<Self> {
        let mut schema_builder = Schema::builder();

        let id_field = schema_builder.add_text_field("id", STRING | STORED);
        let repo_field = schema_builder.add_text_field("repo", STRING);

        let text_options = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("en_stem")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );
        let text_field = schema_builder.add_text_field("text", text_options);

        let schema = schema_builder.build();

        std::fs::create_dir_all(path).map_err(|e| LegionError::Search(e.to_string()))?;

        let index = match Index::open_in_dir(path) {
            Ok(idx) => idx,
            Err(open_err) => {
                // Directory may be empty (new) or corrupted -- either way, create fresh.
                match Index::create_in_dir(path, schema.clone()) {
                    Ok(idx) => idx,
                    Err(_create_err) => {
                        // Creation failed on existing corrupt files -- wipe and retry.
                        eprintln!("[legion] search index corrupted, rebuilding: {}", open_err);
                        Self::recreate_index(path, schema.clone())?
                    }
                }
            }
        };

        Ok(Self {
            index,
            id_field,
            repo_field,
            text_field,
        })
    }

    /// Remove all files in the index directory and create a fresh index.
    ///
    /// Used when the existing index is corrupted (e.g., truncated meta.json)
    /// and cannot be opened. The caller is responsible for repopulating the
    /// index from the database afterward.
    fn recreate_index(path: &Path, schema: Schema) -> Result<Index> {
        std::fs::remove_dir_all(path).map_err(|e| LegionError::Search(e.to_string()))?;
        std::fs::create_dir_all(path).map_err(|e| LegionError::Search(e.to_string()))?;
        Index::create_in_dir(path, schema).map_err(|e| LegionError::Search(e.to_string()))
    }

    /// Add a document to the search index and commit immediately.
    ///
    /// Each document consists of an id (stored for retrieval), a repo name
    /// (for filtering), and the reflection text (for BM25 scoring).
    ///
    /// Retries up to [`WRITER_RETRIES`] times with exponential backoff when
    /// the writer lock is held by another process (e.g., a concurrent hook).
    /// Commits after each write. The reflection corpus is tiny, so the
    /// per-write commit overhead is negligible.
    pub fn add(&self, id: &str, repo: &str, text: &str) -> Result<()> {
        let mut writer: IndexWriter = self.acquire_writer()?;

        writer
            .add_document(doc!(
                self.id_field => id,
                self.repo_field => repo,
                self.text_field => text,
            ))
            .map_err(|e| LegionError::Search(e.to_string()))?;

        writer
            .commit()
            .map_err(|e| LegionError::Search(e.to_string()))?;

        Ok(())
    }

    /// Acquire the index writer with retry on lock contention.
    ///
    /// Tantivy allows only one writer at a time. When multiple legion
    /// processes run concurrently (common with hooks), the writer lock
    /// may be temporarily held. This retries with exponential backoff
    /// before giving up.
    fn acquire_writer(&self) -> Result<IndexWriter> {
        let mut last_err = None;
        for attempt in 0..=WRITER_RETRIES {
            match self.index.writer(15_000_000) {
                Ok(writer) => return Ok(writer),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < WRITER_RETRIES {
                        let delay_ms = WRITER_RETRY_BASE_MS * 2u64.pow(attempt);
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    }
                }
            }
        }
        Err(LegionError::Search(
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "failed to acquire writer".to_string()),
        ))
    }

    /// Delete a document from the search index by reflection id.
    ///
    /// Constructs a term matching the exact `id` field value and removes
    /// every document with that term (there should be at most one, since
    /// `id` is the primary key in the reflections table). Commits
    /// immediately so a subsequent recall does not return the deleted
    /// document.
    ///
    /// No error if the id is not present in the index -- tantivy's
    /// `delete_term` is a no-op when nothing matches. The caller's
    /// database-layer check is the authoritative "does this reflection
    /// exist" source; this method's job is to remove any trace from the
    /// index regardless.
    pub fn delete(&self, id: &str) -> Result<()> {
        let mut writer: IndexWriter = self.acquire_writer()?;
        let term = Term::from_field_text(self.id_field, id);
        writer.delete_term(term);
        writer
            .commit()
            .map_err(|e| LegionError::Search(e.to_string()))?;
        Ok(())
    }

    /// Rebuild the index from a set of reflections in a single commit.
    ///
    /// Clears the existing index contents first, then bulk-inserts all
    /// provided reflections. Used by the `reindex` command to recover
    /// from index/database desync or corruption.
    pub fn rebuild(&self, reflections: &[Reflection]) -> Result<()> {
        let mut writer: IndexWriter = self.acquire_writer()?;

        writer
            .delete_all_documents()
            .map_err(|e| LegionError::Search(e.to_string()))?;

        for r in reflections {
            writer
                .add_document(doc!(
                    self.id_field => r.id.as_str(),
                    self.repo_field => r.repo.as_str(),
                    self.text_field => r.text.as_str(),
                ))
                .map_err(|e| LegionError::Search(e.to_string()))?;
        }

        writer
            .commit()
            .map_err(|e| LegionError::Search(e.to_string()))?;

        Ok(())
    }

    /// Search for reflections matching a query within a specific repo.
    ///
    /// Combines an exact-match filter on `repo` with a BM25-scored query
    /// on the `text` field. Returns up to `limit` results ordered by
    /// descending relevance score.
    ///
    /// Returns an empty vec if the query string is empty or whitespace-only.
    pub fn search(&self, repo: &str, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.execute_query(query, Some(repo), limit)
    }

    /// Search for reflections matching a query across ALL repositories.
    ///
    /// Unlike `search`, this method does not filter by repo. It runs a
    /// BM25-scored query on the `text` field across every indexed document.
    /// Returns up to `limit` results ordered by descending relevance score.
    ///
    /// Returns an empty vec if the query string is empty or whitespace-only.
    pub fn search_all(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.execute_query(query, None, limit)
    }

    /// Shared search implementation. When `repo` is Some, results are filtered
    /// to that repository; when None, all repositories are searched.
    fn execute_query(
        &self,
        query: &str,
        repo: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|e: tantivy::TantivyError| LegionError::Search(e.to_string()))?;

        let searcher = reader.searcher();

        let query_parser = QueryParser::for_index(&self.index, vec![self.text_field]);
        let text_query = query_parser
            .parse_query(trimmed)
            .map_err(|e| LegionError::Search(e.to_string()))?;

        let final_query: Box<dyn tantivy::query::Query> = match repo {
            Some(repo_name) => {
                let repo_term = Term::from_field_text(self.repo_field, repo_name);
                let repo_query = TermQuery::new(repo_term, IndexRecordOption::Basic);
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, Box::new(repo_query)),
                    (Occur::Must, text_query),
                ]))
            }
            None => text_query,
        };

        let top_docs = searcher
            .search(&*final_query, &TopDocs::with_limit(limit))
            .map_err(|e| LegionError::Search(e.to_string()))?;

        let mut results: Vec<SearchResult> = Vec::with_capacity(top_docs.len());

        for (score, doc_address) in top_docs {
            let retrieved_doc: TantivyDocument = searcher
                .doc(doc_address)
                .map_err(|e| LegionError::Search(e.to_string()))?;

            if let Some(id_str) = retrieved_doc
                .get_first(self.id_field)
                .and_then(|v| v.as_str())
            {
                results.push(SearchResult {
                    id: id_str.to_string(),
                    score,
                });
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a SearchIndex backed by a temporary directory.
    ///
    /// Returns both the index and the TempDir handle. The TempDir must
    /// outlive the index to keep the mmap-backed files accessible.
    fn test_index() -> (SearchIndex, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        let idx = SearchIndex::open(dir.path()).expect("failed to open index");
        (idx, dir)
    }

    #[test]
    fn add_and_search() {
        let (idx, _dir) = test_index();
        idx.add(
            "id-1",
            "kelex",
            "mapping rules are fragile when adding new Zod types",
        )
        .unwrap();
        idx.add(
            "id-2",
            "kelex",
            "discriminated unions inside arrays are where complexity hides",
        )
        .unwrap();
        idx.add("id-3", "kelex", "the CLI flag parser is straightforward")
            .unwrap();
        let results = idx.search("kelex", "Zod type mapping", 5).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "id-1");
    }

    #[test]
    fn search_filters_by_repo() {
        let (idx, _dir) = test_index();
        idx.add("id-1", "kelex", "schema introspection is complex")
            .unwrap();
        idx.add("id-2", "rafters", "schema tokens need attention")
            .unwrap();
        let results = idx.search("kelex", "schema", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "id-1");
    }

    #[test]
    fn search_respects_limit() {
        let (idx, _dir) = test_index();
        for i in 0..10 {
            idx.add(
                &format!("id-{i}"),
                "test",
                &format!("reflection about testing {i}"),
            )
            .unwrap();
        }
        let results = idx.search("test", "testing", 3).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn search_empty_query_returns_empty() {
        let (idx, _dir) = test_index();
        idx.add("id-1", "kelex", "some reflection").unwrap();
        let results = idx.search("kelex", "", 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn stemming_matches_variants() {
        let (idx, _dir) = test_index();
        idx.add("id-1", "test", "nested arrays in the codegen templates")
            .unwrap();
        let results = idx.search("test", "nesting array codegen", 5).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "id-1");
    }

    #[test]
    fn search_all_returns_results_from_multiple_repos() {
        let (idx, _dir) = test_index();
        idx.add("id-1", "kelex", "schema introspection is complex")
            .unwrap();
        idx.add("id-2", "rafters", "schema tokens need attention")
            .unwrap();
        idx.add("id-3", "platform", "schema validation with Zod")
            .unwrap();
        let results = idx.search_all("schema", 10).unwrap();
        assert_eq!(results.len(), 3);
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"id-1"));
        assert!(ids.contains(&"id-2"));
        assert!(ids.contains(&"id-3"));
    }

    #[test]
    fn search_all_ranks_by_relevance() {
        let (idx, _dir) = test_index();
        idx.add("id-weak", "kelex", "the CLI flag parser is straightforward")
            .unwrap();
        idx.add(
            "id-strong",
            "rafters",
            "mapping rules are fragile when adding new Zod types for mapping",
        )
        .unwrap();
        let results = idx.search_all("mapping", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "id-strong");
        // BM25 scores must be in descending order
        for pair in results.windows(2) {
            assert!(pair[0].score >= pair[1].score);
        }
    }

    #[test]
    fn search_all_empty_query_returns_empty() {
        let (idx, _dir) = test_index();
        idx.add("id-1", "kelex", "some reflection").unwrap();
        let results = idx.search_all("", 5).unwrap();
        assert!(results.is_empty());
        let results = idx.search_all("   ", 5).unwrap();
        assert!(results.is_empty());
    }

    fn test_reflection(id: &str, repo: &str, text: &str) -> Reflection {
        Reflection {
            id: id.into(),
            repo: repo.into(),
            text: text.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: None,
            audience: "self".into(),
            domain: None,
            tags: None,
            recall_count: 0,
            last_recalled_at: None,
            parent_id: None,
        }
    }

    #[test]
    fn delete_removes_document_from_index() {
        let (idx, _dir) = test_index();
        idx.add(
            "id-keep",
            "kelex",
            "keep this reflection about mapping rules",
        )
        .unwrap();
        idx.add(
            "id-gone",
            "kelex",
            "doomed reflection about mapping rules that should vanish",
        )
        .unwrap();

        // Both documents visible before the delete.
        let before = idx.search("kelex", "mapping rules", 10).unwrap();
        assert_eq!(before.len(), 2);

        idx.delete("id-gone").unwrap();

        // After delete, only the kept document surfaces -- no ghost for id-gone.
        let after = idx.search("kelex", "mapping rules", 10).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, "id-keep");
    }

    #[test]
    fn delete_nonexistent_id_is_noop() {
        let (idx, _dir) = test_index();
        idx.add("id-1", "kelex", "reflection one").unwrap();
        // Deleting a term that never existed should not error.
        idx.delete("id-does-not-exist").unwrap();
        // Existing document still retrievable.
        let results = idx.search("kelex", "reflection", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "id-1");
    }

    #[test]
    fn rebuild_replaces_index_contents() {
        let (idx, _dir) = test_index();
        idx.add("id-old", "test", "old reflection that should be gone")
            .unwrap();

        let reflections = vec![
            test_reflection("id-1", "kelex", "new reflection one"),
            test_reflection("id-2", "rafters", "new reflection two"),
        ];
        idx.rebuild(&reflections).unwrap();

        // Old document should be gone
        let old = idx.search("test", "old reflection", 5).unwrap();
        assert!(old.is_empty());

        // New documents should be present
        let results = idx.search_all("reflection", 10).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn rebuild_empty_clears_index() {
        let (idx, _dir) = test_index();
        idx.add("id-1", "test", "something searchable").unwrap();

        idx.rebuild(&[]).unwrap();

        let results = idx.search_all("searchable", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn corrupted_index_recovers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        // Create a valid index first
        let _idx = SearchIndex::open(path).expect("initial open");
        drop(_idx);

        // Corrupt meta.json
        std::fs::write(path.join("meta.json"), b"not valid json").expect("corrupt");

        // Should recover by recreating
        let idx = SearchIndex::open(path).expect("recovery open");
        idx.add("id-1", "test", "works after recovery").unwrap();
        let results = idx.search("test", "recovery", 5).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_all_respects_limit() {
        let (idx, _dir) = test_index();
        for i in 0..10 {
            idx.add(
                &format!("id-{i}"),
                &format!("repo-{}", i % 3),
                &format!("reflection about testing {i}"),
            )
            .unwrap();
        }
        let results = idx.search_all("testing", 3).unwrap();
        assert_eq!(results.len(), 3);
    }
}
