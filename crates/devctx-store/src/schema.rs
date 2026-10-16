//! DuckDB schema (DDL). Parameterized by embedding dimension for the `vectors`
//! table. See `docs/architecture-spec.md` §4.
//!
//! Note: `commit` is a DuckDB keyword, so it is always double-quoted.

use duckdb::Connection;

use crate::error::Result;

/// Create every table/index if absent. `dim` fixes the `FLOAT[dim]` vector
/// column width; it must equal the active embedding model's dimension.
pub fn init_schema(conn: &Connection, dim: usize) -> Result<()> {
    conn.execute_batch(&vectors_ddl(dim))?;
    drop_legacy_memory_ref_pk(conn);
    // Tables a release added to an existing database: `index_meta` (0.8.3),
    // `symbols` and `edges` (PLAN-009).
    let had_all = ["index_meta", "symbols", "edges"]
        .iter()
        .all(|t| table_exists(conn, t));
    conn.execute_batch(RELATIONAL_DDL)?;
    // `symbols.content_hash` and `edges.hint` came after the tables, before
    // any release: a database written by a development build of PLAN-009
    // lacks them.
    let had_columns =
        column_exists(conn, "symbols", "content_hash") && column_exists(conn, "edges", "hint");
    if !had_columns {
        conn.execute_batch(
            "ALTER TABLE symbols ADD COLUMN IF NOT EXISTS content_hash VARCHAR;
             ALTER TABLE edges ADD COLUMN IF NOT EXISTS hint VARCHAR;",
        )?;
    }
    // The edges every reader means (PLAN-009 TASK-005): without the calls the
    // link pass discarded, which stay as rows only to be reopened by name.
    // A view, so a reader cannot forget the filter.
    let had_view = view_exists(conn, "live_edges");
    if !had_view {
        conn.execute_batch(
            "CREATE VIEW IF NOT EXISTS live_edges AS
             SELECT * FROM edges WHERE coalesce(resolution, '') <> 'discarded';",
        )?;
    }
    if !(had_all && had_columns && had_view) && checkpoint_is_safe(conn) {
        // A table created on an existing database is DDL in the WAL; a process
        // dying before the next checkpoint leaves a log whose replay breaks
        // the ART indexes (see `Store::checkpoint`). Best-effort, as there.
        let _ = conn.execute_batch("CHECKPOINT;");
    }
    drop_broken_project_indexes(conn);
    Ok(())
}

/// Whether a `CHECKPOINT` can bind every index of the database.
///
/// Not when it holds an HNSW index and the VSS extension is not loaded (an
/// offline machine that never installed it): DuckDB then fails the checkpoint
/// *fatally* and invalidates the database for the rest of the process. The
/// DDL stays in the WAL instead — no worse than before the table existed.
/// [`Store::checkpoint`](crate::Store::checkpoint) and its siblings ask the
/// same question before every checkpoint.
pub(crate) fn checkpoint_is_safe(conn: &Connection) -> bool {
    let hnsw = conn
        .query_row(
            "SELECT count(*) > 0 FROM duckdb_indexes() WHERE upper(sql) LIKE '%USING HNSW%'",
            [],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or(true);
    if !hnsw {
        return true;
    }
    conn.query_row(
        "SELECT count(*) > 0 FROM duckdb_extensions() WHERE extension_name = 'vss' AND loaded",
        [],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    conn.query_row(
        "SELECT count(*) > 0 FROM duckdb_columns() WHERE table_name = ? AND column_name = ?",
        [table, column],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

fn view_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT count(*) > 0 FROM duckdb_views() WHERE view_name = ?",
        [name],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT count(*) > 0 FROM duckdb_tables() WHERE table_name = ?",
        [name],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

/// Drop `memory_symbol_references` if it still carries the PRIMARY KEY an
/// earlier schema gave it, so the DDL below recreates it without one.
///
/// Dropping rather than altering because the table holds only derived data:
/// every row is recoverable by re-linking the memories that produced it, and a
/// column-preserving rewrite would cost more than the rebuild it protects.
/// Runs before the DDL so the recreate lands in the same open.
fn drop_legacy_memory_ref_pk(conn: &Connection) {
    let has_pk = conn
        .query_row(
            "SELECT count(*) > 0 FROM duckdb_constraints()
             WHERE table_name = 'memory_symbol_references'
               AND constraint_type = 'PRIMARY KEY'",
            [],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or(false);
    if has_pk {
        let _ = conn.execute_batch("DROP TABLE IF EXISTS memory_symbol_references;");
    }
}

/// Remove the `projects` and `memories` indexes an older schema created.
///
/// They are not merely useless on a table this small — they silently break it:
/// once present, equality lookups stop matching rows that are demonstrably
/// there. A database created before this change keeps them until something
/// drops them, and every open is the natural place. Best-effort: a store that
/// cannot drop them is no worse off than before.
fn drop_broken_project_indexes(conn: &Connection) {
    let _ = conn.execute_batch(
        "DROP INDEX IF EXISTS idx_projects_path;
         DROP INDEX IF EXISTS idx_projects_active;
         DROP INDEX IF EXISTS idx_memories_topic;
         DROP INDEX IF EXISTS idx_memories_hash;
         DROP INDEX IF EXISTS idx_memories_type;
         DROP INDEX IF EXISTS idx_memories_project;",
    );
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
-- No indexes on `memories` either, for the reason given above `projects`, and
-- observed there too: `WHERE project = '@global'` returned 0 against three rows
-- that `WHERE trim(project) = '@global'` found, while the same query for another
-- key on the same column answered correctly. An index that is right for most
-- values and silently wrong for some is worse than no index at all, and a scan
-- over a few thousand rows costs nothing in a columnar engine.

-- Which memories talk about which symbols. Derived at remember() time, never
-- declared; see `memory_refs.rs` for how the links are found.
--
-- No PRIMARY KEY, for the same reason `memories` above has no index: the ART
-- index a PK creates is exactly the one that answered `WHERE project = ?` with
-- zero rows over three matching ones. Uniqueness is enforced in Rust instead —
-- the writer rebuilds a memory's rows wholesale and de-duplicates before
-- inserting, so there is nothing for a constraint to catch.
CREATE TABLE IF NOT EXISTS memory_symbol_references (
    memory_id VARCHAR,
    symbol    VARCHAR,
    file      VARCHAR,
    line      INTEGER,
    repo      VARCHAR,
    branch    VARCHAR,
    source    VARCHAR
);

-- The project registry. Only ever populated in the *central* store, which is
-- the one place that knows about every repository DevCtxEngine tracks; it stays
-- empty in per-project databases.
CREATE TABLE IF NOT EXISTS projects (
    name            VARCHAR PRIMARY KEY,
    path            VARCHAR,
    config_path     VARCHAR,
    db_path         VARCHAR,
    embed_provider  VARCHAR,
    embed_model     VARCHAR,
    embed_dim       INTEGER,
    description     VARCHAR,
    tags            VARCHAR,
    last_commit     VARCHAR,
    last_branch     VARCHAR,
    last_indexed_at VARCHAR,
    file_count      INTEGER,
    symbol_count    INTEGER,
    chunk_count     INTEGER,
    registered_at   VARCHAR,
    updated_at      VARCHAR,
    active          BOOLEAN
);
-- No indexes on `projects` — deliberately. The table holds one row per tracked
-- repository (tens, at the very most), where a scan is already free, and the
-- indexes actively broke it: after enough writes, `WHERE path = ?` and
-- `WHERE active = true` matched nothing at all, while `LIKE`, `trim(path) = ?`
-- and `length(path) = ?` — the forms that cannot use an index — still found
-- every row. The registry looked empty (`projects list` printed nothing) and
-- every `record_index` silently reported "project not found", so a fully
-- indexed repository was listed as never indexed. Dropping both indexes
-- restores all of it; see the migration in `init_schema`.

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

-- Facts about how a branch's index was produced (today: `extractor`). A table
-- of its own so that no existing table changes shape.
CREATE TABLE IF NOT EXISTS index_meta (
    repo_path VARCHAR,
    branch    VARCHAR,
    key       VARCHAR,
    value     VARCHAR,
    PRIMARY KEY (repo_path, branch, key)
);

-- The symbol graph (PLAN-009 DD-2). One row per symbol defined in a file of a
-- branch, and one per *occurrence* of a relation — not per pair, so the second
-- call from one method to the same target is a row of its own. `graph_edges`
-- above is still written, from the same facts, for one release: a downgraded
-- binary and a branch not yet reindexed both still have a graph to read.
--
-- No PRIMARY KEY, UNIQUE or index, for the reason given above `projects`:
-- uniqueness is the writer's job (it deletes a file's rows and appends them
-- again), and an index is added only once a lookup is measured to need it,
-- with a test pinning its equality lookups.
--
-- `id` (DD-3) carries no branch: the same symbol has the same id on every
-- branch. Exposed in JSON as 16 hex digits.
CREATE TABLE IF NOT EXISTS symbols (
    repo        VARCHAR,
    branch      VARCHAR,
    id          UBIGINT,
    parent_id   UBIGINT,
    file        VARCHAR,
    kind        VARCHAR,
    name        VARCHAR,
    qualified   VARCHAR,
    container   VARCHAR,
    package     VARCHAR,
    signature   VARCHAR,
    start_line  INTEGER,
    end_line    INTEGER,
    start_byte  INTEGER,
    end_byte    INTEGER,
    exported    BOOLEAN,
    rank        DOUBLE,
    in_degree   INTEGER,
    is_test     BOOLEAN,
    -- The file symbol's only: the file's `content_hash`, as in `file_state`.
    content_hash VARCHAR
);

-- `src_id` is always a symbol of `file` (the file symbol for a module-level
-- call); `dst_id` stays NULL until the link pass resolves the destination
-- (PLAN-009 DD-6), and so do `confidence`, `resolution` and `external`.
-- `hint` is what the file said about a call's receiver (`typed field Foo`,
-- `name Office`, `chain find name Office /1`…): the link pass re-resolves
-- the row from it and `dst_name`, which it never rewrites, without the source.
CREATE TABLE IF NOT EXISTS edges (
    repo        VARCHAR,
    branch      VARCHAR,
    kind        VARCHAR,
    src_id      UBIGINT,
    dst_id      UBIGINT,
    dst_name    VARCHAR,
    file        VARCHAR,
    line        INTEGER,
    confidence  VARCHAR,
    resolution  VARCHAR,
    external    BOOLEAN,
    from_test   BOOLEAN,
    edge_source VARCHAR,
    hint        VARCHAR
);

CREATE TABLE IF NOT EXISTS branch_lineage (
    repo_path         VARCHAR,
    branch            VARCHAR,
    base_branch       VARCHAR,
    merge_base_commit VARCHAR,
    PRIMARY KEY (repo_path, branch)
);
"#;
