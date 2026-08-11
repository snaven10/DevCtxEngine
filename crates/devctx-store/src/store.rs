//! The DuckDB-backed vector store (F1: brute-force cosine search).

use std::fmt::Write as _;
use std::path::Path;

use devctx_core::types::{SearchFilter, SearchResult, VectorMetadata, VectorPoint};
use duckdb::types::Value;
use duckdb::{params_from_iter, Connection};

use crate::error::{Result, StoreError};
use crate::schema;

/// How many chunk ids `delete_memory_vectors` sweeps per memory (`id_c1..id_cN`).
/// Comfortably above the memory chunker's default cap so lowering it still cleans up.
const DELETE_CHUNK_SWEEP: usize = 256;

/// The 19 `vectors` columns, in the canonical order used by every query.
const COLS: &str = r#"id, text, vector, repo, branch, "commit", file, symbol,
    symbol_type, language, start_line, end_line, chunk_level, content_hash,
    is_deletion, memory_type, memory_scope, memory_tags, indexed_at"#;

/// Rows per `INSERT`/`DELETE` statement in [`Store::upsert`]. Bounds SQL size
/// and bound-parameter count while collapsing many single-row writes into few.
const UPSERT_BATCH: usize = 256;

/// A DuckDB-backed store. Holds one connection and the fixed vector dimension.
pub struct Store {
    pub(crate) conn: Connection,
    dim: usize,
}

impl Store {
    /// Open (creating if needed) a store at `path` with vector dimension `dim`.
    pub fn open(path: &Path, dim: usize) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        schema::init_schema(&conn, dim)?;
        let store = Self { conn, dim };
        store.load_extensions(); // best-effort, so existing HNSW/FTS indexes are usable
        Ok(store)
    }

    /// Open an in-memory store (for tests).
    pub fn open_in_memory(dim: usize) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::init_schema(&conn, dim)?;
        let store = Self { conn, dim };
        store.load_extensions();
        Ok(store)
    }

    /// The store's fixed vector dimension.
    pub fn dimension(&self) -> usize {
        self.dim
    }

    /// The vector width actually recorded in the `vectors` table on disk, or
    /// `None` when the table is absent or its type cannot be parsed.
    ///
    /// The schema is created with `IF NOT EXISTS`, so opening an existing
    /// database with a different [`dimension`](Self::dimension) silently leaves
    /// the old column in place and every write fails later, far from the cause.
    /// Callers that open a store whose model may have changed should compare the
    /// two up front and refuse.
    pub fn stored_dimension(&self) -> Result<Option<usize>> {
        let mut stmt = self.conn.prepare(
            "SELECT data_type FROM information_schema.columns
             WHERE table_name = 'vectors' AND column_name = 'vector'",
        )?;
        let ty: std::result::Result<String, _> = stmt.query_row([], |r| r.get(0));
        let ty = match ty {
            Ok(t) => t,
            Err(duckdb::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        // e.g. `FLOAT[384]`
        let Some(open) = ty.find('[') else {
            return Ok(None);
        };
        let Some(close) = ty[open..].find(']') else {
            return Ok(None);
        };
        Ok(ty[open + 1..open + close].trim().parse().ok())
    }

    /// Open another connection to the *same* in-process database. Unlike
    /// [`open`](Self::open), this shares the already-open database instance, so
    /// it does not take a second file lock — the way to hand concurrent
    /// connections to server request handlers while one owner keeps the file
    /// open. Extensions are loaded per connection.
    pub fn try_clone(&self) -> Result<Self> {
        let conn = self.conn.try_clone()?;
        let store = Self {
            conn,
            dim: self.dim,
        };
        store.load_extensions();
        Ok(store)
    }

    /// Best-effort load of the optional DuckDB extensions (VSS for HNSW, FTS for
    /// keyword search), so pre-built indexes are usable.
    fn load_extensions(&self) {
        self.load_vss();
        self.load_fts();
    }

    /// Best-effort load of the DuckDB VSS extension (for HNSW). Returns whether
    /// it loaded; silently no-ops when the extension is unavailable (e.g. offline).
    fn load_vss(&self) -> bool {
        self.conn
            .execute_batch(
                "INSTALL vss; LOAD vss; SET hnsw_enable_experimental_persistence = true;",
            )
            .is_ok()
    }

    /// Best-effort load of the DuckDB FTS extension (for BM25 keyword search).
    fn load_fts(&self) -> bool {
        self.conn.execute_batch("INSTALL fts; LOAD fts;").is_ok()
    }

    /// Create the HNSW (cosine) index on the `vectors` table for approximate
    /// nearest-neighbor search. Requires the VSS extension; returns `Ok(false)`
    /// (leaving brute-force search intact) when it is unavailable.
    pub fn enable_hnsw(&self) -> Result<bool> {
        if !self.load_vss() {
            return Ok(false);
        }
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_vectors_hnsw \
             ON vectors USING HNSW (vector) WITH (metric = 'cosine');",
        )?;
        Ok(true)
    }

    /// Drop the HNSW index if present. DuckDB maintains an HNSW index on every
    /// insert, which is expensive during a bulk (re)index; dropping it before a
    /// full load and rebuilding with [`enable_hnsw`](Self::enable_hnsw) after is
    /// far faster than loading into an indexed table. Best-effort no-op when the
    /// VSS extension or index is absent.
    pub fn drop_hnsw(&self) -> Result<()> {
        let _ = self
            .conn
            .execute_batch("DROP INDEX IF EXISTS idx_vectors_hnsw;");
        Ok(())
    }

    /// (Re)build the full-text (BM25) index over `vectors.text`. Rebuild-on-demand:
    /// call after indexing. Requires the FTS extension; returns `Ok(false)` when
    /// it is unavailable.
    pub fn rebuild_fts(&self) -> Result<bool> {
        if !self.load_fts() {
            return Ok(false);
        }
        self.conn
            .execute_batch("PRAGMA create_fts_index('vectors', 'id', 'text', overwrite = 1);")?;
        Ok(true)
    }

    /// BM25 keyword search over chunk text, with the same equality filters as
    /// [`search`](Self::search). Requires a prior [`rebuild_fts`](Self::rebuild_fts).
    /// Scores are BM25 relevance (higher is better), not cosine similarity.
    pub fn keyword_search(
        &self,
        query: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let (where_clause, fparams) = build_where(filter);
        let cond = if where_clause.is_empty() {
            "WHERE score IS NOT NULL".to_string()
        } else {
            format!("{where_clause} AND score IS NOT NULL")
        };
        let sql = format!(
            "SELECT {COLS}, score FROM (
                 SELECT *, fts_main_vectors.match_bm25(id, ?) AS score FROM vectors
             ) {cond}
             ORDER BY score DESC
             LIMIT {limit}",
        );
        let mut params: Vec<String> = vec![query.to_string()];
        params.extend(fparams);

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |row| {
            let point = row_to_point(row)?;
            let score: f64 = row.get(19)?;
            Ok((point, score as f32))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (point, score) = r?;
            out.push(SearchResult { point, score });
        }
        Ok(out)
    }

    /// Insert or replace points (delete-by-id then insert), atomically.
    pub fn upsert(&self, points: &[VectorPoint]) -> Result<()> {
        for p in points {
            if p.vector.len() != self.dim {
                return Err(StoreError::DimensionMismatch {
                    expected: self.dim,
                    got: p.vector.len(),
                    id: p.id.clone(),
                });
            }
        }

        self.conn.execute_batch("BEGIN TRANSACTION")?;
        let result = self.upsert_inner(points);
        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn upsert_inner(&self, points: &[VectorPoint]) -> Result<()> {
        // Batch the writes: DuckDB is a columnar/OLAP engine where single-row
        // `INSERT`s are the slowest path (fixed per-statement overhead). We
        // collapse each batch into one multi-row `INSERT` and one `DELETE`.
        //
        // The vector is inlined as a `FLOAT[dim]` literal rather than bound as a
        // parameter because duckdb-rs (as of 1.x) supports neither binding an
        // array as a `?` parameter ("binding List parameters is not yet
        // supported") nor appending array columns via the `Appender`. The
        // Arrow `append_record_batch` path could avoid literals entirely, but it
        // pulls in the heavy `arrow` stack for a per-file workload of tens to a
        // few hundred rows, where multi-row `INSERT` is already ample.
        for chunk in points.chunks(UPSERT_BATCH) {
            // 1) Delete any existing rows for these ids in a single statement.
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let del_ids: Vec<Value> = chunk.iter().map(|p| Value::Text(p.id.clone())).collect();
            self.conn.execute(
                &format!("DELETE FROM vectors WHERE id IN ({placeholders})"),
                params_from_iter(del_ids),
            )?;

            // 2) One multi-row INSERT: vectors as literals, scalars as params.
            let mut tuples = Vec::with_capacity(chunk.len());
            let mut params: Vec<Value> = Vec::with_capacity(chunk.len() * 18);
            for p in chunk {
                tuples.push(format!(
                    "(?, ?, {}, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    self.vec_literal(&p.vector)
                ));
                params.extend(row_params(&p.id, &p.text, &p.metadata));
            }
            self.conn.execute(
                &format!("INSERT INTO vectors ({COLS}) VALUES {}", tuples.join(", ")),
                params_from_iter(params),
            )?;
        }
        Ok(())
    }

    /// Cosine search with optional equality filters. Uses the HNSW index when
    /// present (VSS loaded, no filters); otherwise a brute-force scan. The
    /// `ORDER BY array_cosine_distance(...) LIMIT` shape is what the VSS optimizer
    /// matches, so no query change is needed to benefit from the index.
    pub fn search(
        &self,
        query: &[f32],
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let (where_clause, params) = build_where(filter);
        let dist_expr = format!(
            "array_cosine_distance(vector, {qv}::FLOAT[{dim}])",
            qv = self.vec_literal(query),
            dim = self.dim,
        );
        let sql = format!(
            "SELECT {COLS}, {dist_expr} AS dist
             FROM vectors {where_clause}
             ORDER BY {dist_expr}
             LIMIT {limit}",
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |row| {
            let point = row_to_point(row)?;
            let dist: f32 = row.get(19)?;
            Ok((point, dist))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (point, dist) = r?;
            out.push(SearchResult {
                point,
                score: 1.0 - dist,
            });
        }
        Ok(out)
    }

    /// Delete every vector for a given file.
    pub fn delete_by_file(&self, repo: &str, branch: &str, file: &str) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM vectors WHERE repo = ? AND branch = ? AND file = ?",
            [repo, branch, file],
        )?;
        Ok(n)
    }

    /// Delete a memory's intro vector plus its swept body-chunk vectors.
    pub fn delete_memory_vectors(&self, id: &str) -> Result<usize> {
        let mut ids: Vec<String> = Vec::with_capacity(DELETE_CHUNK_SWEEP + 1);
        ids.push(id.to_string());
        for n in 1..=DELETE_CHUNK_SWEEP {
            ids.push(format!("{id}_c{n}"));
        }
        let placeholders = vec!["?"; ids.len()].join(", ");
        let sql = format!("DELETE FROM vectors WHERE id IN ({placeholders})");
        let n = self.conn.execute(&sql, params_from_iter(ids.iter()))?;
        Ok(n)
    }

    /// Update the `file` column for every row of a renamed file.
    pub fn rename_file(&self, repo: &str, branch: &str, old: &str, new: &str) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE vectors SET file = ? WHERE repo = ? AND branch = ? AND file = ?",
            [new, repo, branch, old],
        )?;
        Ok(n)
    }

    /// Count rows, optionally filtered.
    pub fn count(&self, filter: &SearchFilter) -> Result<u64> {
        let (where_clause, params) = build_where(filter);
        let sql = format!("SELECT count(*) FROM vectors {where_clause}");
        let mut stmt = self.conn.prepare(&sql)?;
        let n: i64 = stmt.query_row(params_from_iter(params), |row| row.get(0))?;
        Ok(n as u64)
    }

    /// Return every point for a (repo, branch), unordered.
    pub fn scroll_all(&self, repo: &str, branch: &str) -> Result<Vec<VectorPoint>> {
        let sql = format!("SELECT {COLS} FROM vectors WHERE repo = ? AND branch = ?");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([repo, branch], row_to_point)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Render a slice of floats as a DuckDB fixed-size array literal.
    fn vec_literal(&self, v: &[f32]) -> String {
        let mut s = String::with_capacity(v.len() * 8 + 16);
        s.push('[');
        for (i, x) in v.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            // Debug formatting yields the shortest round-trippable form.
            let _ = write!(s, "{x:?}");
        }
        let _ = write!(s, "]::FLOAT[{}]", self.dim);
        s
    }
}

/// Ordered SQL parameters for an INSERT row (vector is inlined separately).
fn row_params<'a>(id: &'a str, text: &'a str, m: &'a VectorMetadata) -> Vec<Value> {
    vec![
        Value::Text(id.to_string()),
        Value::Text(text.to_string()),
        Value::Text(m.repo.clone()),
        Value::Text(m.branch.clone()),
        Value::Text(m.commit.clone()),
        Value::Text(m.file.clone()),
        Value::Text(m.symbol.clone()),
        Value::Text(m.symbol_type.clone()),
        Value::Text(m.language.clone()),
        Value::Int(m.start_line),
        Value::Int(m.end_line),
        Value::Text(m.chunk_level.clone()),
        Value::Text(m.content_hash.clone()),
        Value::Boolean(m.is_deletion),
        Value::Text(m.memory_type.clone()),
        Value::Text(m.memory_scope.clone()),
        Value::Text(m.memory_tags.clone()),
        Value::Text(m.indexed_at.clone()),
    ]
}

/// Build a `WHERE` clause + string params from a filter.
fn build_where(f: &SearchFilter) -> (String, Vec<String>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<String> = Vec::new();

    if let Some(repo) = &f.repo {
        clauses.push("repo = ?".to_string());
        params.push(repo.clone());
    }
    if let Some(branch) = &f.branch {
        clauses.push("branch = ?".to_string());
        params.push(branch.clone());
    }
    if !f.languages.is_empty() {
        let ph = vec!["?"; f.languages.len()].join(", ");
        clauses.push(format!("language IN ({ph})"));
        params.extend(f.languages.iter().cloned());
    }
    if let Some(cl) = &f.chunk_level {
        clauses.push("chunk_level = ?".to_string());
        params.push(cl.clone());
    }
    if !f.chunk_levels.is_empty() {
        let ph = vec!["?"; f.chunk_levels.len()].join(", ");
        clauses.push(format!("chunk_level IN ({ph})"));
        params.extend(f.chunk_levels.iter().cloned());
    }
    if let Some(mt) = &f.memory_type {
        clauses.push("memory_type = ?".to_string());
        params.push(mt.clone());
    }
    if f.exclude_deletions {
        clauses.push("is_deletion = false".to_string());
    }

    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    (where_clause, params)
}

/// Decode the first 19 columns of a row into a `VectorPoint`.
fn row_to_point(row: &duckdb::Row<'_>) -> duckdb::Result<VectorPoint> {
    let id: String = row.get(0)?;
    let text: String = row.get(1)?;
    let vector = value_to_f32_vec(row.get(2)?);
    let metadata = VectorMetadata {
        repo: row.get(3)?,
        branch: row.get(4)?,
        commit: row.get(5)?,
        file: row.get(6)?,
        symbol: row.get(7)?,
        symbol_type: row.get(8)?,
        language: row.get(9)?,
        start_line: row.get(10)?,
        end_line: row.get(11)?,
        chunk_level: row.get(12)?,
        content_hash: row.get(13)?,
        is_deletion: row.get(14)?,
        memory_type: row.get(15)?,
        memory_scope: row.get(16)?,
        memory_tags: row.get(17)?,
        indexed_at: row.get(18)?,
    };
    Ok(VectorPoint {
        id,
        vector,
        text,
        metadata,
    })
}

/// Convert a DuckDB array/list value into `Vec<f32>` (best-effort; unknown
/// elements decode to 0.0).
fn value_to_f32_vec(v: Value) -> Vec<f32> {
    let items = match v {
        Value::Array(a) | Value::List(a) => a,
        _ => return Vec::new(),
    };
    items
        .into_iter()
        .map(|e| match e {
            Value::Float(f) => f,
            Value::Double(d) => d as f32,
            _ => 0.0,
        })
        .collect()
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    const DIM: usize = 384;

    fn points(n: usize) -> Vec<VectorPoint> {
        (0..n)
            .map(|i| VectorPoint {
                id: format!("id_{i}"),
                vector: (0..DIM)
                    .map(|j| ((i * 31 + j) % 100) as f32 / 100.0)
                    .collect(),
                text: format!("chunk text number {i}"),
                metadata: VectorMetadata {
                    repo: "demo".into(),
                    branch: "main".into(),
                    file: format!("src/f{}.rs", i % 50),
                    symbol: format!("sym_{i}"),
                    symbol_type: "function".into(),
                    language: "rust".into(),
                    start_line: 1,
                    end_line: 10,
                    chunk_level: "function".into(),
                    content_hash: "hash".into(),
                    indexed_at: "0".into(),
                    ..Default::default()
                },
            })
            .collect()
    }

    /// A/B: the old per-row DELETE+INSERT (one statement per row, one shared
    /// transaction) vs the batched multi-row `upsert`. Run with:
    /// `cargo test -p devctx-store --release -- --ignored --nocapture bench_upsert`
    #[test]
    #[ignore = "perf benchmark; run explicitly"]
    fn bench_upsert_rowwise_vs_batched() {
        let n = 8000;
        let pts = points(n);

        // Old path: replicate the pre-batch code exactly (per-row statements).
        let slow = Store::open_in_memory(DIM).unwrap();
        let insert_sql = format!(
            "INSERT INTO vectors ({COLS}) VALUES \
             (?, ?, {{VEC}}, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        let t0 = Instant::now();
        slow.conn.execute_batch("BEGIN TRANSACTION").unwrap();
        for p in &pts {
            slow.conn
                .execute("DELETE FROM vectors WHERE id = ?", [&p.id])
                .unwrap();
            let sql = insert_sql.replace("{VEC}", &slow.vec_literal(&p.vector));
            slow.conn
                .execute(
                    &sql,
                    params_from_iter(row_params(&p.id, &p.text, &p.metadata)),
                )
                .unwrap();
        }
        slow.conn.execute_batch("COMMIT").unwrap();
        let row_wise = t0.elapsed();

        // New path: batched multi-row upsert.
        let fast = Store::open_in_memory(DIM).unwrap();
        let t1 = Instant::now();
        fast.upsert(&pts).unwrap();
        let batched = t1.elapsed();

        assert_eq!(fast.count(&SearchFilter::default()).unwrap(), n as u64);
        eprintln!(
            "\nupsert {n} vectors (dim {DIM}):\n  row-wise: {row_wise:?}\n  batched : {batched:?}\n  speedup : {:.1}x\n",
            row_wise.as_secs_f64() / batched.as_secs_f64().max(1e-9)
        );
    }
}
