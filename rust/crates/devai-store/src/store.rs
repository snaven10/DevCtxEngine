//! The DuckDB-backed vector store (F1: brute-force cosine search).

use std::fmt::Write as _;
use std::path::Path;

use devai_core::types::{SearchFilter, SearchResult, VectorMetadata, VectorPoint};
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

/// A DuckDB-backed store. Holds one connection and the fixed vector dimension.
pub struct Store {
    conn: Connection,
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
        Ok(Self { conn, dim })
    }

    /// Open an in-memory store (for tests).
    pub fn open_in_memory(dim: usize) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::init_schema(&conn, dim)?;
        Ok(Self { conn, dim })
    }

    /// The store's fixed vector dimension.
    pub fn dimension(&self) -> usize {
        self.dim
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
        let insert_sql = format!(
            "INSERT INTO vectors ({COLS}) VALUES \
             (?, ?, {{VEC}}, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        );
        for p in points {
            self.conn
                .execute("DELETE FROM vectors WHERE id = ?", [&p.id])?;
            let sql = insert_sql.replace("{VEC}", &self.vec_literal(&p.vector));
            let m = &p.metadata;
            self.conn
                .execute(&sql, params_from_iter(row_params(&p.id, &p.text, m)))?;
        }
        Ok(())
    }

    /// Brute-force cosine search with optional equality filters.
    pub fn search(
        &self,
        query: &[f32],
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let (where_clause, params) = build_where(filter);
        let sql = format!(
            "SELECT {COLS}, array_cosine_distance(vector, {qv}::FLOAT[{dim}])::DOUBLE AS dist
             FROM vectors {where_clause}
             ORDER BY dist ASC
             LIMIT {limit}",
            qv = self.vec_literal(query),
            dim = self.dim,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |row| {
            let point = row_to_point(row)?;
            let dist: f64 = row.get(19)?;
            Ok((point, dist as f32))
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
