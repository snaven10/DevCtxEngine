//! Project registry (`projects`) operations: the central store's index of every
//! repository DevCtxEngine tracks.
//!
//! Rows live only in the central database (see `devctx-central`). They are what
//! lets an agent working in one repo discover the others — name, where they are,
//! which embedding model they use, and how fresh their index is — without having
//! to open, or even visit, any of them.

use duckdb::params;

use crate::error::Result;
use crate::store::Store;

/// One `projects` row: a registered repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectRecord {
    /// Unique project key (the name an agent refers to it by).
    pub name: String,
    /// Absolute repository root.
    pub path: String,
    /// Absolute path of the project's `.devctx/config.yaml`.
    pub config_path: String,
    /// Absolute path of the project's DuckDB file.
    pub db_path: String,
    /// Embedding provider (`local`/`openai`/…).
    pub embed_provider: String,
    /// Embedding model key.
    pub embed_model: String,
    /// Embedding dimension — compared before any cross-project vector work.
    pub embed_dim: i64,
    /// Free-text description, for choosing a project without opening it.
    pub description: String,
    /// Comma-separated tags.
    pub tags: String,
    /// Commit at the last recorded index.
    pub last_commit: String,
    /// Branch at the last recorded index.
    pub last_branch: String,
    /// ISO/epoch timestamp of the last recorded index (empty = never).
    pub last_indexed_at: String,
    /// Files in the index at that point.
    pub file_count: i64,
    /// Symbols in the index at that point.
    pub symbol_count: i64,
    /// Chunks in the index at that point.
    pub chunk_count: i64,
    /// ISO/epoch registration timestamp.
    pub registered_at: String,
    /// ISO/epoch last-update timestamp.
    pub updated_at: String,
    /// Whether the project is live. Deactivating keeps the row (and its history)
    /// while hiding it from the default listing.
    pub active: bool,
}

const PROJ_COLS: &str = "name, path, config_path, db_path, embed_provider, embed_model, \
    embed_dim, description, tags, last_commit, last_branch, last_indexed_at, \
    file_count, symbol_count, chunk_count, registered_at, updated_at, active";

impl Store {
    /// Insert or replace a project (by name).
    pub fn upsert_project(&self, p: &ProjectRecord) -> Result<()> {
        self.conn
            .execute("DELETE FROM projects WHERE name = ?", params![p.name])?;
        self.conn.execute(
            &format!(
                "INSERT INTO projects ({PROJ_COLS}) VALUES \
                 (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            ),
            params![
                p.name,
                p.path,
                p.config_path,
                p.db_path,
                p.embed_provider,
                p.embed_model,
                p.embed_dim as i32,
                p.description,
                p.tags,
                p.last_commit,
                p.last_branch,
                p.last_indexed_at,
                p.file_count as i32,
                p.symbol_count as i32,
                p.chunk_count as i32,
                p.registered_at,
                p.updated_at,
                p.active,
            ],
        )?;
        Ok(())
    }

    /// Fetch a project by name (active or not).
    pub fn get_project(&self, name: &str) -> Result<Option<ProjectRecord>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {PROJ_COLS} FROM projects WHERE name = ?"))?;
        opt_row(stmt.query_row(params![name], row_to_project))
    }

    /// Fetch a project by repository path — the lookup that keeps re-registering
    /// the same repo from creating a second row under a different name.
    pub fn find_project_by_path(&self, path: &str) -> Result<Option<ProjectRecord>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {PROJ_COLS} FROM projects WHERE path = ?"))?;
        opt_row(stmt.query_row(params![path], row_to_project))
    }

    /// List projects by name. Inactive ones are hidden unless asked for.
    pub fn list_projects(&self, include_inactive: bool) -> Result<Vec<ProjectRecord>> {
        let where_clause = if include_inactive {
            ""
        } else {
            // `active = true` — not `WHERE active` — stopped matching rows once
            // the database had been written to heavily: every row still reads
            // back `true`, and `WHERE active`, `IS TRUE` and `CAST(active AS
            // INTEGER) = 1` all find them, but the equality is pruned away and
            // the registry looks empty. Test the column directly.
            "WHERE active"
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROJ_COLS} FROM projects {where_clause} ORDER BY name"
        ))?;
        let rows = stmt.query_map([], row_to_project)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Activate or deactivate a project. Returns whether a row was affected.
    pub fn set_project_active(&self, name: &str, active: bool, now: &str) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE projects SET active = ?, updated_at = ? WHERE name = ?",
            params![active, now, name],
        )?;
        Ok(n > 0)
    }

    /// Permanently remove a project row. Returns whether a row was deleted.
    pub fn delete_project(&self, name: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM projects WHERE name = ?", params![name])?;
        Ok(n > 0)
    }

    /// Record the outcome of an indexing run against a project.
    pub fn update_project_index_stats(
        &self,
        name: &str,
        stats: &ProjectIndexStats,
        now: &str,
    ) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE projects SET last_commit = ?, last_branch = ?, last_indexed_at = ?,
                    file_count = ?, symbol_count = ?, chunk_count = ?, updated_at = ?
             WHERE name = ?",
            params![
                stats.commit,
                stats.branch,
                now,
                stats.files as i32,
                stats.symbols as i32,
                stats.chunks as i32,
                now,
                name
            ],
        )?;
        Ok(n > 0)
    }
}

/// What an indexing run produced, as recorded against a registry row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectIndexStats {
    /// Commit that was indexed.
    pub commit: String,
    /// Branch that was indexed.
    pub branch: String,
    /// Files in the index.
    pub files: i64,
    /// Symbols in the index.
    pub symbols: i64,
    /// Chunks in the index.
    pub chunks: i64,
}

fn opt_row(r: duckdb::Result<ProjectRecord>) -> Result<Option<ProjectRecord>> {
    match r {
        Ok(p) => Ok(Some(p)),
        Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn row_to_project(r: &duckdb::Row<'_>) -> duckdb::Result<ProjectRecord> {
    Ok(ProjectRecord {
        name: r.get(0)?,
        path: r.get(1)?,
        config_path: r.get(2)?,
        db_path: r.get(3)?,
        embed_provider: r.get(4)?,
        embed_model: r.get(5)?,
        embed_dim: r.get::<_, i32>(6)? as i64,
        description: r.get(7)?,
        tags: r.get(8)?,
        last_commit: r.get(9)?,
        last_branch: r.get(10)?,
        last_indexed_at: r.get(11)?,
        file_count: r.get::<_, i32>(12)? as i64,
        symbol_count: r.get::<_, i32>(13)? as i64,
        chunk_count: r.get::<_, i32>(14)? as i64,
        registered_at: r.get(15)?,
        updated_at: r.get(16)?,
        active: r.get(17)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(name: &str, path: &str) -> ProjectRecord {
        ProjectRecord {
            name: name.into(),
            path: path.into(),
            config_path: format!("{path}/.devctx/config.yaml"),
            db_path: format!("{path}/.devctx/state/index.duckdb"),
            embed_provider: "local".into(),
            embed_model: "minilm-l6".into(),
            embed_dim: 384,
            registered_at: "100".into(),
            updated_at: "100".into(),
            active: true,
            ..Default::default()
        }
    }

    /// `projects` must carry no index. They are worthless on a table this size
    /// and were observed to break equality lookups outright — `WHERE path = ?`
    /// and `WHERE active = true` returning nothing while the rows sat there —
    /// which emptied `projects list` and made every `record_index` a silent
    /// no-op. Re-adding one would bring all of that back.
    #[test]
    fn the_projects_table_carries_no_index() {
        let store = Store::open_in_memory(4).unwrap();
        store
            .upsert_project(&proj("alpha", "/repos/alpha"))
            .unwrap();
        let mut stmt = store
            .conn
            .prepare("SELECT index_name FROM duckdb_indexes() WHERE table_name = 'projects'")
            .unwrap();
        let found: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(found.is_empty(), "projects must stay unindexed: {found:?}");

        // The lookups that the indexes used to break.
        assert!(store
            .find_project_by_path("/repos/alpha")
            .unwrap()
            .is_some());
        assert_eq!(store.list_projects(false).unwrap().len(), 1);
    }

    #[test]
    fn round_trip_and_lookup_by_name_and_path() {
        let store = Store::open_in_memory(4).unwrap();
        store
            .upsert_project(&proj("alpha", "/repos/alpha"))
            .unwrap();

        let got = store.get_project("alpha").unwrap().unwrap();
        assert_eq!(got.path, "/repos/alpha");
        assert_eq!(got.embed_dim, 384);
        assert!(got.active);
        assert_eq!(got.last_indexed_at, "", "never indexed yet");

        let by_path = store.find_project_by_path("/repos/alpha").unwrap().unwrap();
        assert_eq!(by_path.name, "alpha");

        assert!(store.get_project("nope").unwrap().is_none());
        assert!(store.find_project_by_path("/nope").unwrap().is_none());
    }

    #[test]
    fn upsert_replaces_rather_than_duplicating() {
        let store = Store::open_in_memory(4).unwrap();
        store
            .upsert_project(&proj("alpha", "/repos/alpha"))
            .unwrap();
        let mut updated = proj("alpha", "/repos/alpha-moved");
        updated.description = "moved".into();
        store.upsert_project(&updated).unwrap();

        assert_eq!(store.list_projects(true).unwrap().len(), 1);
        let got = store.get_project("alpha").unwrap().unwrap();
        assert_eq!(got.path, "/repos/alpha-moved");
        assert_eq!(got.description, "moved");
    }

    #[test]
    fn listing_hides_inactive_unless_asked() {
        let store = Store::open_in_memory(4).unwrap();
        store
            .upsert_project(&proj("alpha", "/repos/alpha"))
            .unwrap();
        store.upsert_project(&proj("beta", "/repos/beta")).unwrap();

        assert!(store.set_project_active("beta", false, "200").unwrap());
        let live: Vec<_> = store
            .list_projects(false)
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(live, vec!["alpha"]);
        assert_eq!(store.list_projects(true).unwrap().len(), 2);

        // Deactivating is reversible and keeps the row's history.
        assert!(store.set_project_active("beta", true, "300").unwrap());
        assert_eq!(store.list_projects(false).unwrap().len(), 2);
        assert!(!store.set_project_active("ghost", false, "300").unwrap());
    }

    #[test]
    fn index_stats_are_recorded() {
        let store = Store::open_in_memory(4).unwrap();
        store
            .upsert_project(&proj("alpha", "/repos/alpha"))
            .unwrap();
        let stats = ProjectIndexStats {
            commit: "abc123".into(),
            branch: "main".into(),
            files: 12,
            symbols: 40,
            chunks: 90,
        };
        assert!(store
            .update_project_index_stats("alpha", &stats, "500")
            .unwrap());

        let got = store.get_project("alpha").unwrap().unwrap();
        assert_eq!(got.last_commit, "abc123");
        assert_eq!(got.last_branch, "main");
        assert_eq!(got.file_count, 12);
        assert_eq!(got.chunk_count, 90);
        assert_eq!(got.last_indexed_at, "500");
        assert_eq!(got.updated_at, "500");

        assert!(!store
            .update_project_index_stats("ghost", &stats, "500")
            .unwrap());
    }

    #[test]
    fn delete_removes_the_row() {
        let store = Store::open_in_memory(4).unwrap();
        store
            .upsert_project(&proj("alpha", "/repos/alpha"))
            .unwrap();
        assert!(store.delete_project("alpha").unwrap());
        assert!(store.get_project("alpha").unwrap().is_none());
        assert!(!store.delete_project("alpha").unwrap());
    }
}
