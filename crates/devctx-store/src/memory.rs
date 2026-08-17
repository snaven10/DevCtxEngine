//! Memory table (`memories`) operations: upsert, lookup, recency, stats.

use duckdb::params;

use crate::error::Result;
use crate::store::Store;

/// A stored memory row.
///
/// Serializable so it can be exported: the transfer format is one of these per
/// line, which is what lets memories reach a machine running a different build.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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

pub(crate) const MEM_COLS: &str =
    "id, title, content, memory_type, scope, project, topic_key, tags, \
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

    /// Every live memory stored under `project`, oldest first.
    ///
    /// Unlike [`recent_memories`](Self::recent_memories) this takes no limit: it
    /// backs export, where a cap would quietly hand someone a file missing the
    /// rows past it. Oldest first so replaying the file preserves the order the
    /// memories were written in.
    pub fn all_memories(&self, project: &str) -> Result<Vec<Memory>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MEM_COLS} FROM memories
             WHERE project = ? AND deleted_at IS NULL
             ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map(params![project], row_to_memory)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Live memories in every *shared* space: the global one and every group.
    ///
    /// The central store keys memories by a reserved project — `@global`, or
    /// `@group:<name>` — and a query for one of them silently excludes the
    /// others. That is right for recall, which is asked about a specific tier,
    /// and wrong for anything that means "everything shared": a machine whose
    /// memories all live in one group would report none at all.
    ///
    /// `limit` of zero means no limit, for sweeps that must not stop early.
    pub fn shared_memories(&self, limit: usize) -> Result<Vec<Memory>> {
        let cap = if limit == 0 {
            String::new()
        } else {
            format!(" LIMIT {limit}")
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MEM_COLS} FROM memories
             WHERE deleted_at IS NULL
               AND (project = '@global' OR starts_with(project, '@group:'))
             ORDER BY updated_at DESC{cap}"
        ))?;
        let rows = stmt.query_map([], row_to_memory)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Permanently remove one memory and the vectors that belong to it — its
    /// own row and its `<id>_cN` body chunks. Returns whether it existed.
    ///
    /// Distinct from [`delete_memory`](Self::delete_memory), which only writes a
    /// tombstone: a memory saved by mistake should leave nothing behind, and a
    /// tombstoned one keeps its vectors and goes on competing for the recall
    /// budget it was never meant to occupy.
    pub fn forget_memory(&self, id: &str) -> Result<bool> {
        let existed: i64 = self.conn.query_row(
            "SELECT count(*) FROM memories WHERE id = ?",
            params![id],
            |r| r.get(0),
        )?;
        if existed == 0 {
            return Ok(false);
        }
        self.conn.execute(
            "DELETE FROM vectors WHERE id = ? OR id LIKE ? || '_c%'",
            params![id, id],
        )?;
        self.conn
            .execute("DELETE FROM memories WHERE id = ?", params![id])?;
        Ok(true)
    }

    /// How many memories are stored under `project`, deleted ones included.
    pub fn count_memories_for_project(&self, project: &str) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM memories WHERE project = ?",
            params![project],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Permanently remove every memory stored under `project`, with the vectors
    /// that belong to them — the memory's own row and its `<id>_cN` chunks.
    ///
    /// Unlike [`Store::delete_memory`] this is not a soft delete: it exists for
    /// rows that should never have been written to this store at all, where a
    /// tombstone would leave the vectors behind and the space still spent.
    /// Returns how many memories were removed.
    pub fn purge_memories_for_project(&self, project: &str) -> Result<usize> {
        let n = self.count_memories_for_project(project)?;
        if n == 0 {
            return Ok(0);
        }
        self.conn.execute(
            "DELETE FROM vectors WHERE EXISTS (
                 SELECT 1 FROM memories m
                 WHERE m.project = ?
                   AND (vectors.id = m.id OR vectors.id LIKE m.id || '_c%')
             )",
            params![project],
        )?;
        self.conn
            .execute("DELETE FROM memories WHERE project = ?", params![project])?;
        Ok(n)
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

pub(crate) fn row_to_memory(r: &duckdb::Row<'_>) -> duckdb::Result<Memory> {
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

    /// Export needs the whole set, not a page of it: `recent_memories` caps at a
    /// limit, and a cap silently truncates the file someone is trusting to hold
    /// everything.
    #[test]
    fn all_memories_returns_every_live_row_for_a_project() {
        let store = Store::open_in_memory(3).unwrap();
        for i in 0..5 {
            let mut m = mem(&format!("mem_{i}"), "", "note");
            m.created_at = format!("{i}");
            store.upsert_memory(&m).unwrap();
        }
        let mut other = mem("mem_other", "", "note");
        other.project = "elsewhere".into();
        store.upsert_memory(&other).unwrap();

        let got = store.all_memories("proj").unwrap();
        assert_eq!(got.len(), 5, "every row, and only this project's");
        assert_eq!(
            got[0].id, "mem_0",
            "oldest first, so an import replays in order"
        );

        store.delete_memory("mem_2", "999").unwrap();
        assert_eq!(
            store.all_memories("proj").unwrap().len(),
            4,
            "tombstoned rows are not exported"
        );
    }

    /// Purging a key takes the memories *and* their vectors — the parent row and
    /// its body chunks — while leaving every other key untouched.
    #[test]
    fn purge_takes_memories_with_their_chunk_vectors() {
        let store = Store::open_in_memory(3).unwrap();
        let point = |id: &str| devctx_core::VectorPoint {
            id: id.into(),
            vector: vec![0.1, 0.2, 0.3],
            text: format!("text {id}"),
            metadata: Default::default(),
        };

        let mut stray = mem("mem_stray", "", "note");
        stray.project = "@global".into();
        let mut keep = mem("mem_keep", "", "note");
        keep.project = "proj".into();
        store.upsert_memory(&stray).unwrap();
        store.upsert_memory(&keep).unwrap();
        store
            .upsert(&[
                point("mem_stray"),
                point("mem_stray_c1"),
                point("mem_stray_c2"),
                point("mem_keep"),
                point("mem_keep_c1"),
            ])
            .unwrap();

        assert_eq!(store.count_memories_for_project("@global").unwrap(), 1);
        assert_eq!(store.purge_memories_for_project("@global").unwrap(), 1);

        assert!(store.get_memory("mem_stray").unwrap().is_none());
        assert!(store.get_memory("mem_keep").unwrap().is_some());
        let mut stmt = store
            .conn
            .prepare("SELECT id FROM vectors ORDER BY id")
            .unwrap();
        let left: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            left,
            vec!["mem_keep".to_string(), "mem_keep_c1".to_string()]
        );

        // Purging a key with nothing under it is a no-op, not an error.
        assert_eq!(store.purge_memories_for_project("@global").unwrap(), 0);
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
