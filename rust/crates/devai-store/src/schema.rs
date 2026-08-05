//! DuckDB schema (DDL). Parameterized by embedding dimension for the `vectors`
//! table. Mirrors the legacy LanceDB/SQLite schema (rewrite plan §4).
//!
//! Note: `commit` is a DuckDB keyword, so it is always double-quoted.

use duckdb::Connection;

use crate::error::Result;

/// Create every table/index if absent. `dim` fixes the `FLOAT[dim]` vector
/// column width; it must equal the active embedding model's dimension.
pub fn init_schema(conn: &Connection, dim: usize) -> Result<()> {
    conn.execute_batch(&vectors_ddl(dim))?;
    conn.execute_batch(RELATIONAL_DDL)?;
    Ok(())
}

fn vectors_ddl(dim: usize) -> String {
    format!(
        r#"
CREATE TABLE IF NOT EXISTS vectors (
    id           VARCHAR PRIMARY KEY,
    text         VARCHAR,
    vector       FLOAT[{dim}],
    repo         VARCHAR,
    branch       VARCHAR,
    "commit"     VARCHAR,
    file         VARCHAR,
    symbol       VARCHAR,
    symbol_type  VARCHAR,
    language     VARCHAR,
    start_line   INTEGER,
    end_line     INTEGER,
    chunk_level  VARCHAR,
    content_hash VARCHAR,
    is_deletion  BOOLEAN,
    memory_type  VARCHAR,
    memory_scope VARCHAR,
    memory_tags  VARCHAR,
    indexed_at   VARCHAR
);
CREATE INDEX IF NOT EXISTS idx_vectors_repo_branch ON vectors (repo, branch);
CREATE INDEX IF NOT EXISTS idx_vectors_file ON vectors (repo, branch, file);
CREATE INDEX IF NOT EXISTS idx_vectors_chunk_level ON vectors (chunk_level);
"#
    )
}

/// Relational tables (graph, routes, memories, sessions, index state). These
/// carry no vector column, so they are dimension-independent.
const RELATIONAL_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS graph_edges (
    source      VARCHAR,
    target      VARCHAR,
    kind        VARCHAR,
    source_file VARCHAR,
    target_file VARCHAR,
    line        INTEGER,
    repo        VARCHAR,
    branch      VARCHAR,
    metadata    VARCHAR,
    UNIQUE (source, target, kind, repo, branch, source_file)
);
CREATE INDEX IF NOT EXISTS idx_edges_source ON graph_edges (repo, branch, source);
CREATE INDEX IF NOT EXISTS idx_edges_target ON graph_edges (repo, branch, target);
CREATE INDEX IF NOT EXISTS idx_edges_source_file ON graph_edges (repo, branch, source_file);

CREATE TABLE IF NOT EXISTS routes (
    framework      VARCHAR,
    http_method    VARCHAR,
    path           VARCHAR,
    handler_class  VARCHAR,
    handler_method VARCHAR,
    handler_symbol VARCHAR,
    file           VARCHAR,
    line           INTEGER,
    repo           VARCHAR,
    branch         VARCHAR,
    indexed_at     VARCHAR,
    UNIQUE (framework, http_method, path, repo, branch)
);

CREATE TABLE IF NOT EXISTS memories (
    id              VARCHAR PRIMARY KEY,
    title           VARCHAR,
    content         VARCHAR,
    memory_type     VARCHAR,
    scope           VARCHAR,
    project         VARCHAR,
    topic_key       VARCHAR,
    tags            VARCHAR,
    author          VARCHAR,
    repo            VARCHAR,
    branch          VARCHAR,
    files           VARCHAR,
    revision_count  INTEGER,
    duplicate_count INTEGER,
    normalized_hash VARCHAR,
    vector_id       VARCHAR,
    session_id      VARCHAR,
    created_at      VARCHAR,
    updated_at      VARCHAR,
    deleted_at      VARCHAR
);
CREATE INDEX IF NOT EXISTS idx_memories_topic ON memories (topic_key);
CREATE INDEX IF NOT EXISTS idx_memories_hash ON memories (normalized_hash);
CREATE INDEX IF NOT EXISTS idx_memories_type ON memories (memory_type);
CREATE INDEX IF NOT EXISTS idx_memories_project ON memories (project);

CREATE TABLE IF NOT EXISTS memory_symbol_references (
    memory_id VARCHAR,
    symbol    VARCHAR,
    file      VARCHAR,
    line      INTEGER,
    repo      VARCHAR,
    branch    VARCHAR,
    source    VARCHAR,
    PRIMARY KEY (memory_id, symbol, file, branch)
);

CREATE TABLE IF NOT EXISTS sessions (
    id         VARCHAR PRIMARY KEY,
    project    VARCHAR,
    directory  VARCHAR,
    started_at VARCHAR,
    ended_at   VARCHAR,
    summary    VARCHAR
);

CREATE TABLE IF NOT EXISTS index_state (
    repo_path       VARCHAR,
    branch          VARCHAR,
    last_commit     VARCHAR,
    model_name      VARCHAR,
    model_dimension INTEGER,
    file_count      INTEGER,
    symbol_count    INTEGER,
    chunk_count     INTEGER,
    indexed_at      VARCHAR,
    PRIMARY KEY (repo_path, branch)
);

CREATE TABLE IF NOT EXISTS file_state (
    repo_path    VARCHAR,
    branch       VARCHAR,
    file_path    VARCHAR,
    content_hash VARCHAR,
    language     VARCHAR,
    symbol_count INTEGER,
    chunk_count  INTEGER,
    PRIMARY KEY (repo_path, branch, file_path)
);

CREATE TABLE IF NOT EXISTS branch_lineage (
    repo_path         VARCHAR,
    branch            VARCHAR,
    base_branch       VARCHAR,
    merge_base_commit VARCHAR,
    PRIMARY KEY (repo_path, branch)
);
"#;
