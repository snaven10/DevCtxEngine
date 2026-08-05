//! Incremental-indexing state: `index_state` and `file_state` operations.
//!
//! Keyed by `repo_path` (absolute) + `branch`, distinct from the short `repo`
//! name used on the `vectors` table.

use duckdb::params;

use crate::error::Result;
use crate::store::Store;

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
        self.conn.execute(
            "DELETE FROM index_state WHERE repo_path = ? AND branch = ?",
            params![rec.repo_path, rec.branch],
        )?;
        self.conn.execute(
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

    /// Insert or replace a file-state row.
    pub fn save_file_state(&self, fs: &FileState) -> Result<()> {
        self.conn.execute(
            "DELETE FROM file_state WHERE repo_path = ? AND branch = ? AND file_path = ?",
            params![fs.repo_path, fs.branch, fs.file_path],
        )?;
        self.conn.execute(
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
    pub fn delete_file_state(&self, repo_path: &str, branch: &str, file: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM file_state WHERE repo_path = ? AND branch = ? AND file_path = ?",
            params![repo_path, branch, file],
        )?;
        Ok(())
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
