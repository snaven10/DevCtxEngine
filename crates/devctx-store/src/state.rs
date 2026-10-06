//! Incremental-indexing state: `index_state` and `file_state` operations.
//!
//! Keyed by `repo_path` (absolute) + `branch`, distinct from the short `repo`
//! name used on the `vectors` table.

use duckdb::params;

use crate::error::Result;
use crate::store::Store;

/// The `index_meta` key under which the extractor fingerprint is stored.
pub const EXTRACTOR_META_KEY: &str = "extractor";

/// `index_meta` key holding the fingerprint of the embedding setup the vectors
/// of a branch were made with (see `devctx_embed::embedding_fingerprint`).
pub const EMBED_FP_META_KEY: &str = "embedding_fingerprint";

/// One `index_state` row: what was last indexed for a (repo_path, branch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRecord {
    /// Absolute repo path.
    pub repo_path: String,
    /// Branch.
    pub branch: String,
    /// Commit that was indexed.
    pub last_commit: String,
    /// Embedding model name.
    pub model_name: String,
    /// Embedding dimension (drives full-reindex on change).
    pub model_dimension: i64,
    /// Files indexed.
    pub file_count: i64,
    /// Symbols indexed.
    pub symbol_count: i64,
    /// Chunks indexed.
    pub chunk_count: i64,
    /// ISO-8601 timestamp.
    pub indexed_at: String,
}

/// One `file_state` row: the last-indexed content hash for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileState {
    /// Absolute repo path.
    pub repo_path: String,
    /// Branch.
    pub branch: String,
    /// File path (repo-relative).
    pub file_path: String,
    /// sha256[:16] of the file content at index time.
    pub content_hash: String,
    /// Detected language.
    pub language: String,
    /// Symbols found.
    pub symbol_count: i64,
    /// Chunks produced.
    pub chunk_count: i64,
}

impl Store {
    /// What the index currently holds for a (repo_path, branch): files, symbols
    /// and chunks, summed across `file_state`.
    ///
    /// These are *totals*, unlike an [`IndexResult`](crate::IndexRecord)'s
    /// per-run counts. An incremental run that finds nothing changed touches
    /// zero files — reporting that as the project's size would read as "this
    /// project is empty" rather than "nothing moved".
    pub fn index_totals(&self, repo_path: &str, branch: &str) -> Result<(i64, i64, i64)> {
        let mut stmt = self.conn.prepare(
            "SELECT count(*), coalesce(sum(symbol_count), 0), coalesce(sum(chunk_count), 0)
             FROM file_state WHERE repo_path = ? AND branch = ?",
        )?;
        let row = stmt.query_row(params![repo_path, branch], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        Ok(row)
    }

    /// Fetch the index record for a (repo_path, branch), if any.
    pub fn get_index_record(&self, repo_path: &str, branch: &str) -> Result<Option<IndexRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT last_commit, model_name, model_dimension, file_count, symbol_count,
                    chunk_count, indexed_at
             FROM index_state WHERE repo_path = ? AND branch = ?",
        )?;
        let row = stmt.query_row(params![repo_path, branch], |r| {
            Ok(IndexRecord {
                repo_path: repo_path.to_string(),
                branch: branch.to_string(),
                last_commit: r.get(0)?,
                model_name: r.get(1)?,
                model_dimension: r.get::<_, i32>(2)? as i64,
                file_count: r.get::<_, i32>(3)? as i64,
                symbol_count: r.get::<_, i32>(4)? as i64,
                chunk_count: r.get::<_, i32>(5)? as i64,
                indexed_at: r.get(6)?,
            })
        });
        match row {
            Ok(rec) => Ok(Some(rec)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Insert or replace an index record.
    pub fn save_index_record(&self, rec: &IndexRecord) -> Result<()> {
        self.w()?.execute(
            "DELETE FROM index_state WHERE repo_path = ? AND branch = ?",
            params![rec.repo_path, rec.branch],
        )?;
        self.w()?.execute(
            "INSERT INTO index_state (repo_path, branch, last_commit, model_name,
                model_dimension, file_count, symbol_count, chunk_count, indexed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                rec.repo_path,
                rec.branch,
                rec.last_commit,
                rec.model_name,
                rec.model_dimension as i32,
                rec.file_count as i32,
                rec.symbol_count as i32,
                rec.chunk_count as i32,
                rec.indexed_at,
            ],
        )?;
        Ok(())
    }

    /// One `index_meta` value for a (repo_path, branch), if recorded.
    pub fn get_index_meta(
        &self,
        repo_path: &str,
        branch: &str,
        key: &str,
    ) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT value FROM index_meta WHERE repo_path = ? AND branch = ? AND key = ?",
        )?;
        match stmt.query_row(params![repo_path, branch, key], |r| r.get::<_, String>(0)) {
            Ok(v) => Ok(Some(v)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Insert or replace one `index_meta` value.
    pub fn set_index_meta(
        &self,
        repo_path: &str,
        branch: &str,
        key: &str,
        value: &str,
    ) -> Result<()> {
        self.w()?.execute(
            "DELETE FROM index_meta WHERE repo_path = ? AND branch = ? AND key = ?",
            params![repo_path, branch, key],
        )?;
        self.w()?.execute(
            "INSERT INTO index_meta (repo_path, branch, key, value) VALUES (?, ?, ?, ?)",
            params![repo_path, branch, key, value],
        )?;
        Ok(())
    }

    /// Forget one `index_meta` value (a branch whose record is missing, as
    /// every index made before the extractor fingerprint existed).
    pub fn delete_index_meta(&self, repo_path: &str, branch: &str, key: &str) -> Result<()> {
        self.w()?.execute(
            "DELETE FROM index_meta WHERE repo_path = ? AND branch = ? AND key = ?",
            params![repo_path, branch, key],
        )?;
        Ok(())
    }

    /// Whether the index of a (repo_path, branch) was built by an extractor
    /// other than `current` (an [`extractor fingerprint`]). An index with no
    /// recorded extractor — every index made before this existed — counts as
    /// stale: its extractor is unknown, and "unknown" must not read as "fine".
    ///
    /// [`extractor fingerprint`]: EXTRACTOR_META_KEY
    pub fn extractor_stale(&self, repo_path: &str, branch: &str, current: &str) -> Result<bool> {
        Ok(self
            .get_index_meta(repo_path, branch, EXTRACTOR_META_KEY)?
            .as_deref()
            != Some(current))
    }

    /// The last-indexed content hash for a file, if recorded.
    pub fn get_file_hash(
        &self,
        repo_path: &str,
        branch: &str,
        file: &str,
    ) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT content_hash FROM file_state
             WHERE repo_path = ? AND branch = ? AND file_path = ?",
        )?;
        match stmt.query_row(params![repo_path, branch, file], |r| r.get::<_, String>(0)) {
            Ok(h) => Ok(Some(h)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The recorded state of one file, if any.
    pub fn get_file_state(
        &self,
        repo_path: &str,
        branch: &str,
        file: &str,
    ) -> Result<Option<FileState>> {
        let mut stmt = self.conn.prepare(
            "SELECT content_hash, language, symbol_count, chunk_count FROM file_state
             WHERE repo_path = ? AND branch = ? AND file_path = ?",
        )?;
        let row = stmt.query_row(params![repo_path, branch, file], |r| {
            Ok(FileState {
                repo_path: repo_path.to_string(),
                branch: branch.to_string(),
                file_path: file.to_string(),
                content_hash: r.get::<_, String>(0)?,
                language: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                symbol_count: r.get::<_, i64>(2)?,
                chunk_count: r.get::<_, i64>(3)?,
            })
        });
        match row {
            Ok(f) => Ok(Some(f)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Insert or replace a file-state row.
    pub fn save_file_state(&self, fs: &FileState) -> Result<()> {
        self.w()?.execute(
            "DELETE FROM file_state WHERE repo_path = ? AND branch = ? AND file_path = ?",
            params![fs.repo_path, fs.branch, fs.file_path],
        )?;
        self.w()?.execute(
            "INSERT INTO file_state (repo_path, branch, file_path, content_hash, language,
                symbol_count, chunk_count)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                fs.repo_path,
                fs.branch,
                fs.file_path,
                fs.content_hash,
                fs.language,
                fs.symbol_count as i32,
                fs.chunk_count as i32,
            ],
        )?;
        Ok(())
    }

    /// Delete a file-state row (on file deletion).
    /// Another branch that already has this exact file content indexed.
    ///
    /// Branches share commits — a feature branch differs from its base in a
    /// handful of files and is identical in the other thousand. Embedding is
    /// the expensive half of indexing (measured in tens of minutes for a large
    /// corpus, against milliseconds to copy a row), so a second branch should
    /// pay only for what actually differs.
    ///
    /// `content_hash` is what makes that safe: identical bytes give an
    /// identical hash, so a hit means the chunks already in the store are the
    /// chunks this branch would have produced — *provided the same extractor
    /// produced them*. Rows also carry symbols and edges, so only branches
    /// whose `index_meta` records `extractor` (the current fingerprint) are
    /// offered; a branch from an older extractor, or one with no record, would
    /// smuggle its stale symbols into a branch about to be stamped fresh.
    pub fn branch_with_same_content(
        &self,
        repo_path: &str,
        file: &str,
        content_hash: &str,
        except_branch: &str,
        extractor: &str,
    ) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.branch FROM file_state f
             JOIN index_meta m
               ON m.repo_path = f.repo_path AND m.branch = f.branch AND m.key = ?
             WHERE f.repo_path = ? AND f.file_path = ? AND f.content_hash = ?
               AND f.branch <> ? AND m.value = ?
             LIMIT 1",
        )?;
        match stmt.query_row(
            duckdb::params![
                EXTRACTOR_META_KEY,
                repo_path,
                file,
                content_hash,
                except_branch,
                extractor
            ],
            |r| r.get::<_, String>(0),
        ) {
            Ok(b) => Ok(Some(b)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Every branch this repository has rows for, so a caller can tell which
    /// ones are no longer wanted.
    pub fn indexed_branches(&self, repo_path: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT branch FROM file_state WHERE repo_path = ?")?;
        let rows = stmt.query_map(duckdb::params![repo_path], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Branches with an `index_state` record for this repository, most recently
    /// indexed first. What "the last indexed branch" means when neither the
    /// checked-out nor the default branch has anything.
    pub fn branches_by_recency(&self, repo_path: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT branch FROM index_state WHERE repo_path = ? ORDER BY indexed_at DESC, branch",
        )?;
        let rows = stmt.query_map(duckdb::params![repo_path], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn delete_file_state(&self, repo_path: &str, branch: &str, file: &str) -> Result<()> {
        self.w()?.execute(
            "DELETE FROM file_state WHERE repo_path = ? AND branch = ? AND file_path = ?",
            params![repo_path, branch, file],
        )?;
        Ok(())
    }

    /// Files whose state row says they hold chunks while `vectors` holds none
    /// for them, as `(branch, file)`.
    ///
    /// The signature of a file whose writes were cut half-way: its content
    /// hash recorded (so every incremental run skips it as unchanged) over
    /// vectors already deleted. One transaction per file makes it impossible;
    /// this is how a test — or a curious operator — proves it. Vectors are
    /// keyed by the repository's short name and state rows by its path, so the
    /// match is on branch and file, which is exact for a store holding one
    /// repository (every project store does).
    pub fn files_missing_vectors(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.branch, f.file_path FROM file_state f
             WHERE f.chunk_count > 0
               AND NOT EXISTS (SELECT 1 FROM vectors v
                               WHERE v.branch = f.branch AND v.file = f.file_path)
             ORDER BY f.branch, f.file_path",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// All file paths with recorded state for a (repo_path, branch). Used to
    /// prune files that vanished since the last index (full-reindex cleanup).
    pub fn list_file_states(&self, repo_path: &str, branch: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT file_path FROM file_state WHERE repo_path = ? AND branch = ?")?;
        let rows = stmt.query_map(params![repo_path, branch], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_record_round_trip() {
        let store = Store::open_in_memory(3).unwrap();
        assert!(store.get_index_record("/repo", "main").unwrap().is_none());
        let rec = IndexRecord {
            repo_path: "/repo".into(),
            branch: "main".into(),
            last_commit: "abc123".into(),
            model_name: "minilm-l6".into(),
            model_dimension: 384,
            file_count: 10,
            symbol_count: 42,
            chunk_count: 55,
            indexed_at: "2026-08-05T00:00:00Z".into(),
        };
        store.save_index_record(&rec).unwrap();
        assert_eq!(store.get_index_record("/repo", "main").unwrap(), Some(rec));
    }

    #[test]
    fn a_store_without_index_meta_reports_a_stale_extractor() {
        let store = Store::open_in_memory(3).unwrap();
        // Simulates a database created before the table existed.
        store.conn.execute_batch("DROP TABLE index_meta;").unwrap();
        crate::schema::init_schema(&store.conn, 3).unwrap();
        assert!(store.extractor_stale("/repo", "main", "v1-x").unwrap());
        store
            .set_index_meta("/repo", "main", EXTRACTOR_META_KEY, "v1-x")
            .unwrap();
        assert!(!store.extractor_stale("/repo", "main", "v1-x").unwrap());
        assert!(store.extractor_stale("/repo", "main", "v2-y").unwrap());
        assert!(store.extractor_stale("/repo", "dev", "v1-x").unwrap());
        // Replacing, not appending.
        store
            .set_index_meta("/repo", "main", EXTRACTOR_META_KEY, "v2-y")
            .unwrap();
        assert!(!store.extractor_stale("/repo", "main", "v2-y").unwrap());
    }

    /// Totals must survive a run that changed nothing: an incremental index
    /// touching zero files does not mean the project is empty.
    #[test]
    fn index_totals_sum_the_whole_index() {
        let store = Store::open_in_memory(4).unwrap();
        assert_eq!(store.index_totals("/repo", "main").unwrap(), (0, 0, 0));

        for (file, symbols, chunks) in [("a.rs", 3, 7), ("b.rs", 2, 5)] {
            store
                .save_file_state(&FileState {
                    repo_path: "/repo".into(),
                    branch: "main".into(),
                    file_path: file.into(),
                    content_hash: "h".into(),
                    language: "rust".into(),
                    symbol_count: symbols,
                    chunk_count: chunks,
                })
                .unwrap();
        }
        assert_eq!(store.index_totals("/repo", "main").unwrap(), (2, 5, 12));

        // Scoped to the branch and repository, not global.
        assert_eq!(store.index_totals("/repo", "other").unwrap(), (0, 0, 0));
        assert_eq!(store.index_totals("/elsewhere", "main").unwrap(), (0, 0, 0));

        // A deleted file leaves the totals correct.
        store.delete_file_state("/repo", "main", "a.rs").unwrap();
        assert_eq!(store.index_totals("/repo", "main").unwrap(), (1, 2, 5));
    }

    #[test]
    fn file_state_round_trip_and_delete() {
        let store = Store::open_in_memory(3).unwrap();
        let fs = FileState {
            repo_path: "/repo".into(),
            branch: "main".into(),
            file_path: "src/a.rs".into(),
            content_hash: "deadbeef".into(),
            language: "rust".into(),
            symbol_count: 3,
            chunk_count: 4,
        };
        store.save_file_state(&fs).unwrap();
        assert_eq!(
            store.get_file_hash("/repo", "main", "src/a.rs").unwrap(),
            Some("deadbeef".to_string())
        );
        store
            .delete_file_state("/repo", "main", "src/a.rs")
            .unwrap();
        assert!(store
            .get_file_hash("/repo", "main", "src/a.rs")
            .unwrap()
            .is_none());
    }
}
