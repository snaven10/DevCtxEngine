//! Memory table (`memories`) operations: upsert, lookup, recency, stats.

use duckdb::params;

use crate::error::Result;
use crate::store::Store;

/// A stored memory row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    /// Deterministic id (`mem_<hash>`).
    pub id: String,
    /// Short title.
    pub title: String,
    /// Full content.
    pub content: String,
    /// Type (insight/decision/note/bug/architecture/pattern/discovery).
    pub memory_type: String,
    /// Scope (`shared`/`local`).
    pub scope: String,
    /// Project the memory belongs to.
    pub project: String,
    /// Topic key for upsert-by-topic (empty if none).
    pub topic_key: String,
    /// Comma-separated tags.
    pub tags: String,
    /// Author.
    pub author: String,
    /// Repo.
    pub repo: String,
    /// Branch.
    pub branch: String,
    /// Comma-separated related files.
    pub files: String,
    /// Number of revisions.
    pub revision_count: i64,
    /// Number of duplicate re-adds collapsed into this row.
    pub duplicate_count: i64,
    /// sha256 of normalized content.
    pub normalized_hash: String,
    /// Id of the intro vector (== `id`).
    pub vector_id: String,
    /// Session id.
    pub session_id: String,
    /// ISO/epoch creation timestamp.
    pub created_at: String,
    /// ISO/epoch update timestamp.
    pub updated_at: String,
    /// Soft-delete timestamp, or `None` if live.
    pub deleted_at: Option<String>,
}

/// Aggregate memory counts for a project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryStats {
    /// Total live memories.
    pub total: i64,
    /// Live counts per memory type, descending.
    pub by_type: Vec<(String, i64)>,
}

const MEM_COLS: &str = "id, title, content, memory_type, scope, project, topic_key, tags, \
    author, repo, branch, files, revision_count, duplicate_count, normalized_hash, \
    vector_id, session_id, created_at, updated_at, deleted_at";

impl Store {
    /// Insert or replace a memory (by id).
    pub fn upsert_memory(&self, m: &Memory) -> Result<()> {
        self.conn
            .execute("DELETE FROM memories WHERE id = ?", params![m.id])?;
        self.conn.execute(
            &format!(
                "INSERT INTO memories ({MEM_COLS}) VALUES \
                 (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            ),
            params![
                m.id,
                m.title,
                m.content,
                m.memory_type,
                m.scope,
                m.project,
                m.topic_key,
                m.tags,
                m.author,
                m.repo,
                m.branch,
                m.files,
                m.revision_count as i32,
                m.duplicate_count as i32,
                m.normalized_hash,
                m.vector_id,
                m.session_id,
                m.created_at,
                m.updated_at,
                m.deleted_at,
            ],
        )?;
        Ok(())
    }

    /// Fetch a memory by id (including soft-deleted ones).
    pub fn get_memory(&self, id: &str) -> Result<Option<Memory>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {MEM_COLS} FROM memories WHERE id = ?"))?;
        opt_row(stmt.query_row(params![id], row_to_memory))
    }

    /// Fetch a live memory by (project, topic_key).
    pub fn find_memory_by_topic(&self, project: &str, topic_key: &str) -> Result<Option<Memory>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MEM_COLS} FROM memories
             WHERE project = ? AND topic_key = ? AND deleted_at IS NULL
             ORDER BY updated_at DESC LIMIT 1"
        ))?;
        opt_row(stmt.query_row(params![project, topic_key], row_to_memory))
    }

    /// Most recently updated live memories for a project.
    pub fn recent_memories(&self, project: &str, limit: usize) -> Result<Vec<Memory>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MEM_COLS} FROM memories
             WHERE project = ? AND deleted_at IS NULL
             ORDER BY updated_at DESC LIMIT {limit}"
        ))?;
        let rows = stmt.query_map(params![project], row_to_memory)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Soft-delete a memory (records the timestamp).
    pub fn delete_memory(&self, id: &str, now: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE memories SET deleted_at = ? WHERE id = ?",
            params![now, id],
        )?;
        Ok(())
    }

    /// Live memory counts for a project (total + per type).
    pub fn memory_stats(&self, project: &str) -> Result<MemoryStats> {
        let mut stmt = self.conn.prepare(
            "SELECT memory_type, count(*) FROM memories
             WHERE project = ? AND deleted_at IS NULL
             GROUP BY memory_type ORDER BY count(*) DESC",
        )?;
        let rows = stmt.query_map(params![project], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        let by_type = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let total = by_type.iter().map(|(_, n)| n).sum();
        Ok(MemoryStats { total, by_type })
    }
}

fn opt_row(r: duckdb::Result<Memory>) -> Result<Option<Memory>> {
    match r {
        Ok(m) => Ok(Some(m)),
        Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn row_to_memory(r: &duckdb::Row<'_>) -> duckdb::Result<Memory> {
    Ok(Memory {
        id: r.get(0)?,
        title: r.get(1)?,
        content: r.get(2)?,
        memory_type: r.get(3)?,
        scope: r.get(4)?,
        project: r.get(5)?,
        topic_key: r.get(6)?,
        tags: r.get(7)?,
        author: r.get(8)?,
        repo: r.get(9)?,
        branch: r.get(10)?,
        files: r.get(11)?,
        revision_count: r.get::<_, i32>(12)? as i64,
        duplicate_count: r.get::<_, i32>(13)? as i64,
        normalized_hash: r.get(14)?,
        vector_id: r.get(15)?,
        session_id: r.get(16)?,
        created_at: r.get(17)?,
        updated_at: r.get(18)?,
        deleted_at: r.get(19)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(id: &str, topic: &str, ty: &str) -> Memory {
        Memory {
            id: id.into(),
            title: format!("title {id}"),
            content: format!("content {id}"),
            memory_type: ty.into(),
            project: "proj".into(),
            topic_key: topic.into(),
            normalized_hash: "hash".into(),
            vector_id: id.into(),
            created_at: "100".into(),
            updated_at: "100".into(),
            ..Default::default()
        }
    }

    #[test]
    fn upsert_get_and_topic_lookup() {
        let store = Store::open_in_memory(3).unwrap();
        assert!(store.get_memory("mem_a").unwrap().is_none());
        let m = mem("mem_a", "auth", "decision");
        store.upsert_memory(&m).unwrap();
        assert_eq!(store.get_memory("mem_a").unwrap(), Some(m.clone()));
        assert_eq!(store.find_memory_by_topic("proj", "auth").unwrap(), Some(m));
        assert!(store
            .find_memory_by_topic("proj", "nope")
            .unwrap()
            .is_none());
    }

    #[test]
    fn recent_excludes_deleted_and_orders() {
        let store = Store::open_in_memory(3).unwrap();
        let mut a = mem("mem_a", "", "note");
        a.updated_at = "100".into();
        let mut b = mem("mem_b", "", "note");
        b.updated_at = "200".into();
        store.upsert_memory(&a).unwrap();
        store.upsert_memory(&b).unwrap();
        let recent = store.recent_memories("proj", 10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, "mem_b"); // newer first

        store.delete_memory("mem_b", "300").unwrap();
        let recent = store.recent_memories("proj", 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, "mem_a");
    }

    #[test]
    fn stats_counts_by_type() {
        let store = Store::open_in_memory(3).unwrap();
        store.upsert_memory(&mem("m1", "", "decision")).unwrap();
        store.upsert_memory(&mem("m2", "", "decision")).unwrap();
        store.upsert_memory(&mem("m3", "", "note")).unwrap();
        let stats = store.memory_stats("proj").unwrap();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.by_type[0], ("decision".to_string(), 2));
    }
}
