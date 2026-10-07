//! Incremental-indexing state: `index_state` and `file_state` operations.
//!
//! Keyed by `repo_path` (absolute) + `branch`, distinct from the short `repo`
//! name used on the `vectors` table.

use duckdb::params;

use devctx_core::symbol_id::GRAPH_LANGUAGES;

use crate::error::Result;
use crate::store::Store;

/// The `index_meta` key under which the extractor fingerprint is stored.
pub const EXTRACTOR_META_KEY: &str = "extractor";

/// What a branch must have been indexed with to be a source for a copy: the
/// active extractor and embedding setup.
#[derive(Debug, Clone, Copy)]
pub struct CopySetup<'a> {
    /// The repository's short name, which the graph tables are keyed by.
    pub repo: &'a str,
    pub extractor: &'a str,
    pub embed_fp: &'a str,
    pub model_name: &'a str,
    pub dimension: i64,
}

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
        self.forget_graph_step(Some(&rec.branch));
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
    /// So does one whose graph is out of step with its files
    /// ([`graph_out_of_step`](Self::graph_out_of_step)): the stamp is only
    /// rewritten by a full run, so an older binary's incremental runs leave
    /// it saying "current" over files whose `symbols` rows they never wrote.
    /// `repo` is the short name the graph tables are keyed by. The graph
    /// check is cached (the graph tools ask on every call): see
    /// [`graph_out_of_step_cached`](Self::graph_out_of_step_cached).
    ///
    /// [`extractor fingerprint`]: EXTRACTOR_META_KEY
    pub fn extractor_stale(
        &self,
        repo: &str,
        repo_path: &str,
        branch: &str,
        current: &str,
    ) -> Result<bool> {
        if self
            .get_index_meta(repo_path, branch, EXTRACTOR_META_KEY)?
            .as_deref()
            != Some(current)
        {
            return Ok(true);
        }
        self.graph_out_of_step_cached(repo, repo_path, branch)
    }

    /// Whether `file_state` and the symbol graph disagree on a branch: a
    /// file of a parseable language ([`GRAPH_LANGUAGES`]) with no `file`
    /// symbol row — zero symbols included, since this binary writes the file
    /// symbol for every such file — (added by a binary that does not write
    /// the graph, 0.9.0 after a downgrade), a `file` symbol
    /// row whose file has no state any more (deleted by one), or a `file`
    /// symbol row whose `content_hash` is not the file's (modified by one:
    /// its symbol rows are still there, only older).
    ///
    /// Three anti-joins over one branch. Callers that ask on every tool call
    /// use [`graph_out_of_step_cached`](Self::graph_out_of_step_cached).
    pub fn graph_out_of_step(&self, repo: &str, repo_path: &str, branch: &str) -> Result<bool> {
        let n: bool = self.conn.query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM file_state f
                 WHERE f.repo_path = ? AND f.branch = ?
                   AND list_contains(string_split(?, ','), f.language)
                   AND NOT EXISTS (SELECT 1 FROM symbols s
                       WHERE s.repo = ? AND s.branch = f.branch AND s.kind = 'file'
                         AND s.file = f.file_path)
             ) OR EXISTS (
                 SELECT 1 FROM symbols s
                 WHERE s.repo = ? AND s.branch = ? AND s.kind = 'file'
                   AND NOT EXISTS (SELECT 1 FROM file_state f
                       WHERE f.repo_path = ? AND f.branch = s.branch
                         AND f.file_path = s.file)
             ) OR EXISTS (
                 SELECT 1 FROM symbols s JOIN file_state f
                   ON f.repo_path = ? AND f.branch = s.branch AND f.file_path = s.file
                 WHERE s.repo = ? AND s.branch = ? AND s.kind = 'file'
                   AND s.content_hash IS DISTINCT FROM f.content_hash
             )",
            params![
                repo_path,
                branch,
                GRAPH_LANGUAGES.join(","),
                repo,
                repo,
                branch,
                repo_path,
                repo_path,
                repo,
                branch
            ],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// [`graph_out_of_step`](Self::graph_out_of_step), remembered per
    /// `(repo, repo_path, branch)` and the branch's `index_state.indexed_at`:
    /// the graph tools ask on every call, and a copy between branches on
    /// every file. A run that writes the branch ends by stamping a new
    /// `indexed_at`; this process's own writes to the branch (`file_state`,
    /// the graph tables, `index_state`) drop the answer before they run and
    /// again when their transaction commits — until then another connection
    /// reads, and could cache, the rows from before them — and a rolled-back
    /// transaction drops it too. An answer computed across any of those drops
    /// is returned but not kept; inside a transaction the cache is bypassed
    /// (the connection sees its own uncommitted rows). So it is never older
    /// than the committed rows.
    pub fn graph_out_of_step_cached(
        &self,
        repo: &str,
        repo_path: &str,
        branch: &str,
    ) -> Result<bool> {
        if self.in_transaction_now() {
            // Its own uncommitted rows: neither what the cache holds for
            // everyone else nor something to keep in it.
            return self.graph_out_of_step(repo, repo_path, branch);
        }
        let key = (repo.to_string(), repo_path.to_string(), branch.to_string());
        // The generation is read before the rows: a write that lands in
        // between bumps it, and the answer is not kept.
        let generation = self.graph_step_generation();
        let indexed_at = self
            .get_index_record(repo_path, branch)?
            .map(|r| r.indexed_at);
        if let Some(v) = self.cached_graph_step(&key, &indexed_at) {
            return Ok(v);
        }
        let v = self.graph_out_of_step(repo, repo_path, branch)?;
        self.remember_graph_step(key, indexed_at, v, generation);
        Ok(v)
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
        self.forget_graph_step(Some(&fs.branch));
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
    /// smuggle its stale symbols into a branch about to be stamped fresh. The
    /// seal is not enough on its own: an older binary's incremental runs
    /// leave it saying "current" over a graph they did not write, so a branch
    /// whose graph is [out of step](Self::graph_out_of_step) is not offered
    /// either (answer cached, see [`graph_out_of_step_cached`]).
    ///
    /// [`graph_out_of_step_cached`]: Self::graph_out_of_step_cached
    ///
    /// The same holds for the vectors: a source qualifies only if they were made
    /// by the active embedding setup. Its `EMBED_FP_META_KEY` must equal
    /// `embed_fp` (a branch in "transition" never does), or it must have none —
    /// an index from before fingerprints — while its `index_state` names the
    /// same `model_name` and `dimension`. Without that, a copy would put another
    /// setup's vectors under this one's seal.
    pub fn branch_with_same_content(
        &self,
        repo_path: &str,
        file: &str,
        content_hash: &str,
        except_branch: &str,
        setup: &CopySetup,
    ) -> Result<Option<String>> {
        let CopySetup {
            repo,
            extractor,
            embed_fp,
            model_name,
            dimension,
        } = *setup;
        let mut stmt = self.conn.prepare(
            "SELECT f.branch FROM file_state f
             JOIN index_meta m
               ON m.repo_path = f.repo_path AND m.branch = f.branch AND m.key = ?
             LEFT JOIN index_meta e
               ON e.repo_path = f.repo_path AND e.branch = f.branch AND e.key = ?
             LEFT JOIN index_state s
               ON s.repo_path = f.repo_path AND s.branch = f.branch
             WHERE f.repo_path = ? AND f.file_path = ? AND f.content_hash = ?
               AND f.branch <> ? AND m.value = ?
               AND (e.value = ?
                    OR (e.value IS NULL AND s.model_name = ? AND s.model_dimension = ?))
             ORDER BY f.branch",
        )?;
        let candidates = stmt.query_map(
            duckdb::params![
                EXTRACTOR_META_KEY,
                EMBED_FP_META_KEY,
                repo_path,
                file,
                content_hash,
                except_branch,
                extractor,
                embed_fp,
                model_name,
                dimension
            ],
            |r| r.get::<_, String>(0),
        )?;
        for branch in candidates {
            let branch = branch?;
            if !self.graph_out_of_step_cached(repo, repo_path, &branch)? {
                return Ok(Some(branch));
            }
        }
        Ok(None)
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
        self.forget_graph_step(Some(branch));
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
        let stale = |b: &str, fp: &str| store.extractor_stale("r", "/repo", b, fp).unwrap();
        assert!(stale("main", "v1-x"));
        store
            .set_index_meta("/repo", "main", EXTRACTOR_META_KEY, "v1-x")
            .unwrap();
        assert!(!stale("main", "v1-x"));
        assert!(stale("main", "v2-y"));
        assert!(stale("dev", "v1-x"));
        // Replacing, not appending.
        store
            .set_index_meta("/repo", "main", EXTRACTOR_META_KEY, "v2-y")
            .unwrap();
        assert!(!stale("main", "v2-y"));
    }

    /// A downgrade to a binary that does not write the graph, a few
    /// incremental runs, and back: the stamp still says "current", the
    /// graph has holes and orphans. Both read as a stale extractor.
    #[test]
    fn a_graph_out_of_step_with_its_files_reads_as_stale() {
        use crate::symbols::StoredSymbol;
        let store = Store::open_in_memory(3).unwrap();
        store
            .set_index_meta("/repo", "main", EXTRACTOR_META_KEY, "v2-x")
            .unwrap();
        let state = |file: &str, symbols: i64| FileState {
            repo_path: "/repo".into(),
            branch: "main".into(),
            file_path: file.into(),
            content_hash: "h".into(),
            language: "python".into(),
            symbol_count: symbols,
            chunk_count: 1,
        };
        let file_sym = |file: &str| StoredSymbol {
            id: devctx_core::symbol_id::file_symbol_id("r", file),
            file: file.into(),
            kind: "file".into(),
            name: file.into(),
            qualified: file.into(),
            content_hash: Some("h".into()),
            ..Default::default()
        };
        let stale = || store.extractor_stale("r", "/repo", "main", "v2-x").unwrap();
        store.save_file_state(&state("a.py", 2)).unwrap();
        store
            .replace_file_graph("r", "main", "a.py", &[file_sym("a.py")], &[])
            .unwrap();
        // A file of no parseable language has no graph to miss (raw text).
        store
            .save_file_state(&FileState {
                language: "markdown".into(),
                ..state("README.md", 0)
            })
            .unwrap();
        assert!(!stale(), "in step");

        // Added by the older binary: state, no graph.
        store.save_file_state(&state("b.py", 1)).unwrap();
        assert!(stale(), "a file with symbols and no graph rows");
        store
            .replace_file_graph("r", "main", "b.py", &[file_sym("b.py")], &[])
            .unwrap();
        assert!(!stale());
        // Also a parseable file with no symbol at all: this binary writes
        // its file symbol anyway, the older one does not.
        store.save_file_state(&state("empty.py", 0)).unwrap();
        assert!(
            stale(),
            "a parseable file with no symbols and no graph rows"
        );
        store
            .replace_file_graph("r", "main", "empty.py", &[file_sym("empty.py")], &[])
            .unwrap();
        assert!(!stale());

        // Deleted by the older binary: graph rows, no state.
        store.delete_file_state("/repo", "main", "a.py").unwrap();
        assert!(stale(), "graph rows of a file that is gone");
        // Another repo's rows in the same database are not this one's.
        store.delete_file_graph("r", "main", "a.py").unwrap();
        store
            .replace_file_graph("other", "main", "c.py", &[file_sym("c.py")], &[])
            .unwrap();
        assert!(!stale());

        // Modified by the older binary: new state, the old graph rows.
        store
            .save_file_state(&FileState {
                content_hash: "h2".into(),
                ..state("b.py", 1)
            })
            .unwrap();
        assert!(
            stale(),
            "graph rows parsed from other bytes than the file's"
        );
    }

    fn sealed_branch(store: &Store, branch: &str, file: &str, in_step: bool) {
        use crate::symbols::StoredSymbol;
        store
            .set_index_meta("/repo", branch, EXTRACTOR_META_KEY, "v2-x")
            .unwrap();
        store
            .set_index_meta("/repo", branch, EMBED_FP_META_KEY, "fp")
            .unwrap();
        store
            .save_file_state(&FileState {
                repo_path: "/repo".into(),
                branch: branch.into(),
                file_path: file.into(),
                content_hash: "h".into(),
                language: "python".into(),
                symbol_count: 1,
                chunk_count: 1,
            })
            .unwrap();
        let row = StoredSymbol {
            id: devctx_core::symbol_id::file_symbol_id("r", file),
            file: file.into(),
            kind: "file".into(),
            name: file.into(),
            qualified: file.into(),
            content_hash: Some(if in_step { "h" } else { "old" }.into()),
            ..Default::default()
        };
        store
            .replace_file_graph("r", branch, file, &[row], &[])
            .unwrap();
    }

    /// The seal of a branch says "current" also after an older binary's
    /// incremental runs: a branch whose graph is out of step with its files
    /// must not be the source of a copy, or its old rows spread to the
    /// branch being indexed under a fresh seal.
    #[test]
    fn a_branch_out_of_step_is_not_a_copy_source() {
        let store = Store::open_in_memory(3).unwrap();
        let setup = CopySetup {
            repo: "r",
            extractor: "v2-x",
            embed_fp: "fp",
            model_name: "m",
            dimension: 3,
        };
        let source = |store: &Store| {
            store
                .branch_with_same_content("/repo", "a.py", "h", "feature", &setup)
                .unwrap()
        };
        sealed_branch(&store, "dev", "a.py", false);
        assert_eq!(source(&store), None, "dev's graph is older than its files");
        sealed_branch(&store, "main", "a.py", true);
        assert_eq!(source(&store).as_deref(), Some("main"));
    }

    /// The graph check is remembered until the branch changes: a new
    /// `indexed_at`, or a write of this process to the branch.
    #[test]
    fn the_graph_check_is_cached_until_the_branch_changes() {
        let store = Store::open_in_memory(3).unwrap();
        sealed_branch(&store, "main", "a.py", true);
        let record = |at: &str| IndexRecord {
            repo_path: "/repo".into(),
            branch: "main".into(),
            last_commit: "c".into(),
            model_name: "m".into(),
            model_dimension: 3,
            file_count: 1,
            symbol_count: 1,
            chunk_count: 1,
            indexed_at: at.into(),
        };
        store.save_index_record(&record("1")).unwrap();
        let cached = || {
            store
                .graph_out_of_step_cached("r", "/repo", "main")
                .unwrap()
        };
        assert!(!cached());
        // Behind the store's back: the answer is the cached one… The write
        // goes through the raw connection because no other *process* can
        // make it while this one holds the file: DuckDB opens a database
        // read-write in one process only (an exclusive lock), so another
        // binary's writes land between two opens, and a fresh open starts
        // with an empty cache. What the cache must survive is the case below
        // with this process's own connections.
        store
            .conn
            .execute_batch("UPDATE file_state SET content_hash = 'h2'")
            .unwrap();
        assert!(store.graph_out_of_step("r", "/repo", "main").unwrap());
        assert!(!cached(), "cached under the same indexed_at");
        // …until that run stamps the branch.
        store
            .conn
            .execute_batch("UPDATE index_state SET indexed_at = '2'")
            .unwrap();
        assert!(cached());
        // This process's own writes drop it at once.
        sealed_branch(&store, "main", "a.py", true);
        assert!(!cached());
        store
            .save_file_state(&FileState {
                repo_path: "/repo".into(),
                branch: "main".into(),
                file_path: "b.py".into(),
                content_hash: "h".into(),
                language: "python".into(),
                symbol_count: 1,
                chunk_count: 1,
            })
            .unwrap();
        assert!(cached(), "b.py has no graph rows");
    }

    /// A request handler on a cloned connection asks while the indexer is
    /// inside a file's transaction: it reads the rows from before the
    /// transaction, and must not keep that answer past the commit — the
    /// writes forgot the cache *before* they ran, and `indexed_at` only
    /// changes at the end of the run.
    #[test]
    fn an_answer_read_before_a_commit_is_dropped_by_it() {
        let store = Store::open_in_memory(3).unwrap();
        let handler = store.try_clone().unwrap();
        sealed_branch(&store, "main", "a.py", true);
        let cached = |s: &Store| s.graph_out_of_step_cached("r", "/repo", "main").unwrap();
        assert!(!cached(&handler));
        store
            .in_transaction(|| {
                // The file changes; its graph rows are not rewritten (the
                // older-binary case), so after the commit it is out of step.
                store.save_file_state(&FileState {
                    repo_path: "/repo".into(),
                    branch: "main".into(),
                    file_path: "a.py".into(),
                    content_hash: "h2".into(),
                    language: "python".into(),
                    symbol_count: 1,
                    chunk_count: 1,
                })?;
                // The handler still sees the committed rows, and caches
                // that answer.
                assert!(!cached(&handler), "the handler reads the old snapshot");
                Ok(())
            })
            .unwrap();
        assert!(
            store.graph_out_of_step("r", "/repo", "main").unwrap(),
            "committed: out of step"
        );
        assert!(
            cached(&handler),
            "the pre-commit answer outlived the commit"
        );
        assert!(cached(&store));
    }

    /// The same, rolled back: the answer from inside the transaction is the
    /// one that must not survive, and the old one is right again.
    #[test]
    fn an_answer_read_inside_a_rolled_back_transaction_is_dropped() {
        let store = Store::open_in_memory(3).unwrap();
        let handler = store.try_clone().unwrap();
        sealed_branch(&store, "main", "a.py", true);
        let cached = |s: &Store| s.graph_out_of_step_cached("r", "/repo", "main").unwrap();
        assert!(!cached(&store));
        let out: Result<()> = store.in_transaction(|| {
            store.delete_file_state("/repo", "main", "a.py")?;
            // The writer sees its own rows, past the cache, and keeps them
            // from the others.
            assert!(cached(&store), "inside: the file symbol has no state");
            assert!(!cached(&handler), "the writer's answer leaked");
            assert!(cached(&store), "the handler's answer leaked");
            Err(crate::StoreError::Decode("cut".into()))
        });
        assert!(out.is_err());
        assert!(!cached(&store), "rolled back: in step again");
        assert!(!cached(&handler));
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
