//! The DuckDB-backed vector store (F1: brute-force cosine search).

use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use devctx_core::types::{SearchFilter, SearchResult, VectorMetadata, VectorPoint};
use duckdb::types::Value;
use duckdb::{params, params_from_iter, Connection};

use crate::error::{Result, StoreError};
use crate::schema;

/// How many chunk ids `delete_memory_vectors` sweeps per memory (`id_c1..id_cN`).
/// Comfortably above the memory chunker's default cap so lowering it still cleans up.
const DELETE_CHUNK_SWEEP: usize = 256;

/// The 19 `vectors` columns, in the canonical order used by every query.
pub(crate) const COLS: &str = r#"id, text, vector, repo, branch, "commit", file, symbol,
    symbol_type, language, start_line, end_line, chunk_level, content_hash,
    is_deletion, memory_type, memory_scope, memory_tags, indexed_at"#;

/// Rows per `INSERT`/`DELETE` statement in [`Store::upsert`]. Bounds SQL size
/// and bound-parameter count while collapsing many single-row writes into few.
const UPSERT_BATCH: usize = 256;

/// DuckDB's own defaults assume it owns the machine: `memory_limit` is 80% of
/// physical RAM and `threads` is one per core. DevCtxEngine does not own the
/// machine — it runs one server per project path (see
/// `devctx-cli::remote::auto_addr`), so several are alive at once, each
/// entitled to 80% of the same box. Three of them on a 16 GB laptop is 38 GB of
/// intent, and the kernel's OOM killer arrives long before DuckDB feels any
/// pressure. Worse, that killer does not pick the greedy process: it picks
/// whatever has the highest `oom_score`, which on a systemd session is the
/// user's own desktop services, so the visible symptom is a dead panel and
/// closed windows rather than a slow query.
///
/// A modest per-process budget costs a spill to disk on the largest queries and
/// buys a machine that stays usable. Override with `DEVCTX_DB_MEMORY_LIMIT`
/// (any DuckDB size literal, e.g. `8GB`) and `DEVCTX_DB_THREADS`.
const DEFAULT_MEMORY_LIMIT: &str = "2GB";
const DEFAULT_THREADS: usize = 4;

/// Whether `v` is a DuckDB size literal (`512MB`, `2GB`, `1.5GiB`) and nothing
/// else. The value reaches `SET` by interpolation, and DuckDB takes no bound
/// parameter there; a size literal cannot carry a quote, so refusing everything
/// that is not one keeps the statement safe by construction rather than by
/// escaping.
fn is_size_literal(v: &str) -> bool {
    !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
}

/// Read a `usize` from the environment, falling back to `default` when the
/// variable is absent or does not parse.
fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A DuckDB-backed store. Holds one connection and the fixed vector dimension.
pub struct Store {
    pub(crate) conn: Connection,
    dim: usize,
    /// Metric of the HNSW index, resolved once.
    ///
    /// Reading it from the catalog costs a `duckdb_indexes()` scan (~2 ms) —
    /// nothing next to indexing, but paid on *every* search it would be a
    /// larger slice than the vector scan it exists to speed up. Invalidated
    /// whenever the index is created or dropped.
    metric_cache: std::sync::Mutex<Option<Option<String>>>,
    /// Shared by every connection cloned from the same open (see
    /// [`instance_id`](Self::instance_id) and [`freeze`](Self::freeze)).
    shared: Arc<Shared>,
    /// Whether this connection is inside [`in_transaction`](Self::in_transaction).
    in_tx: AtomicBool,
}

/// State common to every connection of one open database.
#[derive(Default)]
struct Shared {
    /// Set by [`Store::freeze`]: every later write fails.
    frozen: AtomicBool,
    /// Writers hold the read side for one statement; a freeze takes the write
    /// side to wait out the statements already running.
    gate: RwLock<()>,
}

/// A connection borrowed for one write; see [`Store::w`].
pub(crate) struct WriteConn<'a> {
    _gate: RwLockReadGuard<'a, ()>,
    conn: &'a Connection,
}

impl std::ops::Deref for WriteConn<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn
    }
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
        let store = Self {
            conn,
            dim,
            metric_cache: std::sync::Mutex::new(None),
            shared: Arc::new(Shared::default()),
            in_tx: AtomicBool::new(false),
        };
        store.apply_resource_limits(); // before any query can allocate against the defaults
        schema::init_schema(&store.conn, dim)?;
        store.load_extensions(); // best-effort, so existing HNSW/FTS indexes are usable
        Ok(store)
    }

    /// Open and immediately release the database file at `path`, to learn
    /// whether another process holds it — without loading a model or touching
    /// the schema. A file that does not exist yet is not a conflict.
    pub fn check_unlocked(path: &Path) -> Result<()> {
        if path.exists() {
            drop(Connection::open(path)?);
        }
        Ok(())
    }

    /// Open an in-memory store (for tests).
    pub fn open_in_memory(dim: usize) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn,
            dim,
            metric_cache: std::sync::Mutex::new(None),
            shared: Arc::new(Shared::default()),
            in_tx: AtomicBool::new(false),
        };
        store.apply_resource_limits();
        schema::init_schema(&store.conn, dim)?;
        store.load_extensions();
        Ok(store)
    }

    /// Identifies the in-process database behind this connection: equal for a
    /// store and every [`try_clone`](Self::try_clone) of it, different for any
    /// other open (including another in-memory store). Stable while any of
    /// those connections lives.
    pub fn instance_id(&self) -> usize {
        Arc::as_ptr(&self.shared) as *const () as usize
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
            metric_cache: std::sync::Mutex::new(None),
            shared: self.shared.clone(),
            in_tx: AtomicBool::new(false),
        };
        store.load_extensions();
        Ok(store)
    }

    /// Cap what this database may take from the machine — see
    /// [`DEFAULT_MEMORY_LIMIT`].
    ///
    /// Best-effort: a rejected setting leaves DuckDB on its own defaults, which
    /// is no worse than never having asked. Called only where a database
    /// instance is created; [`try_clone`](Self::try_clone) shares that instance
    /// and inherits both settings, so it pays for no extra statements per
    /// request.
    fn apply_resource_limits(&self) {
        let limit = std::env::var("DEVCTX_DB_MEMORY_LIMIT")
            .ok()
            .filter(|v| is_size_literal(v))
            .unwrap_or_else(|| DEFAULT_MEMORY_LIMIT.to_string());
        let threads = env_usize("DEVCTX_DB_THREADS", DEFAULT_THREADS).max(1);
        let _ = self.conn.execute_batch(&format!(
            "SET memory_limit = '{limit}'; SET threads = {threads};"
        ));
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

    /// Create the HNSW index on the `vectors` table for approximate
    /// nearest-neighbor search. Requires the VSS extension; returns `Ok(false)`
    /// (leaving brute-force search intact) when it is unavailable.
    ///
    /// `metric` is `cosine` (always correct) or `ip` (inner product — cheaper,
    /// but only equivalent to cosine when the embeddings are unit-normalized).
    ///
    /// The metric is encoded in the index *name* because DuckDB's catalog does
    /// not report it: `duckdb_indexes()` shows the `CREATE INDEX` without its
    /// `WITH (metric = …)` clause. A search whose distance function disagrees
    /// with the index silently falls back to a full scan — or, worse, orders by
    /// the wrong measure — so [`hnsw_metric`](Self::hnsw_metric) reads it back
    /// from the name and the query is built to match.
    pub fn enable_hnsw(&self, metric: &str) -> Result<bool> {
        if !self.load_vss() {
            return Ok(false);
        }
        let metric = normalize_metric(metric);
        // Only one may exist: two indexes on the same column would both be
        // maintained on every insert, for no gain.
        self.drop_hnsw()?;
        self.w()?.execute_batch(&format!(
            "CREATE INDEX IF NOT EXISTS idx_vectors_hnsw_{metric} \
             ON vectors USING HNSW (vector) WITH (metric = '{metric}');",
        ))?;
        self.invalidate_metric_cache();
        // The index DDL above must not outlive this process uncheckpointed —
        // see `checkpoint`'s own doc comment. Best-effort, same as there.
        self.checkpoint();
        Ok(true)
    }

    /// The metric of the HNSW index on `vectors`, or `None` when there is none.
    /// Resolved once per connection; see `metric_cache`.
    pub fn hnsw_metric(&self) -> Option<String> {
        if let Ok(mut guard) = self.metric_cache.lock() {
            if let Some(cached) = guard.as_ref() {
                return cached.clone();
            }
            let fresh = self.read_hnsw_metric();
            *guard = Some(fresh.clone());
            return fresh;
        }
        self.read_hnsw_metric()
    }

    /// Uncached catalog read behind [`hnsw_metric`](Self::hnsw_metric).
    fn read_hnsw_metric(&self) -> Option<String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT index_name FROM duckdb_indexes() \
                 WHERE table_name = 'vectors' AND index_name LIKE 'idx_vectors_hnsw%'",
            )
            .ok()?;
        let mut rows = stmt.query([]).ok()?;
        let row = rows.next().ok()??;
        let name: String = row.get(0).ok()?;
        // Legacy indexes carry no suffix and were always cosine.
        Some(match name.rsplit_once("hnsw_") {
            Some((_, m)) => m.to_string(),
            None => "cosine".to_string(),
        })
    }

    /// Drop the HNSW index if present. DuckDB maintains an HNSW index on every
    /// insert, which is expensive during a bulk (re)index; dropping it before a
    /// full load and rebuilding with [`enable_hnsw`](Self::enable_hnsw) after is
    /// far faster than loading into an indexed table. Best-effort no-op when the
    /// VSS extension or index is absent.
    pub fn drop_hnsw(&self) -> Result<()> {
        let _ = self.w()?.execute_batch(
            "DROP INDEX IF EXISTS idx_vectors_hnsw; \
             DROP INDEX IF EXISTS idx_vectors_hnsw_cosine; \
             DROP INDEX IF EXISTS idx_vectors_hnsw_ip;",
        );
        self.invalidate_metric_cache();
        // Same shape as the FTS drop below: a DROP is DDL, and a crash before
        // the next checkpoint leaves it stranded in the WAL, unreplayable.
        self.checkpoint();
        Ok(())
    }

    /// (Re)build the full-text (BM25) index over `vectors.text`. Rebuild-on-demand:
    /// call after indexing. Requires the FTS extension; returns `Ok(false)` when
    /// it is unavailable.
    pub fn rebuild_fts(&self) -> Result<bool> {
        if !self.load_fts() {
            return Ok(false);
        }
        self.w()?
            .execute_batch("PRAGMA create_fts_index('vectors', 'id', 'text', overwrite = 1);")?;
        // This is DDL, and a process that dies before the next checkpoint
        // leaves it stranded in the WAL — DuckDB cannot replay it on the next
        // open, and the database is unopenable for good. See `checkpoint`'s
        // doc comment; best-effort there for the same reason it is here.
        self.checkpoint();
        Ok(true)
    }

    /// Whether the BM25 index exists and is usable.
    ///
    /// The FTS extension defines `match_bm25` only once an index has been built,
    /// so probing the function is what distinguishes "no index" from "no
    /// extension" — and lets a caller build one instead of surfacing a raw
    /// catalog error.
    pub fn has_fts(&self) -> bool {
        self.load_fts()
            && self
                .conn
                .prepare("SELECT fts_main_vectors.match_bm25(id, 'x') FROM vectors LIMIT 1")
                .is_ok()
    }

    /// Drop the BM25 index if present.
    ///
    /// DuckDB cannot maintain an FTS index across row deletions on the indexed
    /// table: a reindex that deletes rows aborts with "Failed to delete all rows
    /// from index". So it is dropped before any run that deletes and rebuilt
    /// after — the same shape as [`drop_hnsw`](Self::drop_hnsw), for the same
    /// reason. Best-effort: absent index or extension is a no-op.
    ///
    /// The drop is DDL too, and the exact failure that motivates the
    /// checkpoint below — a process dying between this drop and the next
    /// checkpoint — is what actually bricked this repo's own index: DuckDB
    /// replaying the WAL hit "Cannot drop entry \"fts_main_vectors\" because
    /// there are entries that depend on it" and refused to open ever again.
    pub fn drop_fts(&self) -> Result<()> {
        if self.load_fts() {
            let _ = self.w()?.execute_batch("PRAGMA drop_fts_index('vectors');");
            self.checkpoint();
        }
        Ok(())
    }

    /// Fold the write-ahead log into the database file.
    ///
    /// DuckDB replays the WAL when it opens a database, but a replayed append
    /// does not restore the entries of an ART index — the structure behind every
    /// `PRIMARY KEY` and `UNIQUE` in our schema. The table then holds rows the
    /// index has never heard of, and the next `DELETE` touching them aborts with
    /// *"Failed to delete all rows from index"* and takes the whole connection
    /// down with it, permanently: reindexing cannot fix it, because reindexing
    /// begins by deleting.
    ///
    /// So the WAL must not outlive the process that wrote it. Every path that
    /// ends the server checkpoints first, and so does the end of an indexing
    /// run, which is where nearly all of the WAL comes from.
    ///
    /// Best-effort: a checkpoint can legitimately fail while another connection
    /// has an open transaction, and that is not worth failing a run over.
    pub fn checkpoint(&self) {
        let _ = self.try_checkpoint();
    }

    /// [`checkpoint`](Self::checkpoint), reporting whether it happened.
    ///
    /// A plain `CHECKPOINT` refuses to run while another connection to the same
    /// database has a write transaction open — exactly the situation on the way
    /// out of a server whose indexing run is mid-file. Callers that are about
    /// to end the process need to know, so they can escalate to
    /// [`force_checkpoint`](Self::force_checkpoint).
    pub fn try_checkpoint(&self) -> Result<()> {
        self.conn.execute_batch("CHECKPOINT;")?;
        Ok(())
    }

    /// Fold the WAL even if other connections have transactions open, aborting
    /// them (DuckDB's `FORCE CHECKPOINT`).
    ///
    /// Only for the last moments of a process: whatever those transactions were
    /// doing is lost — which it would be anyway once the process exits — but the
    /// rows already committed are folded into the file instead of being left in
    /// a WAL whose replay breaks the ART indexes (see `checkpoint`).
    pub fn force_checkpoint(&self) -> Result<()> {
        self.conn.execute_batch("FORCE CHECKPOINT;")?;
        Ok(())
    }

    /// How many times bigger the database file is than the rows it holds, or
    /// `None` when it cannot be worked out (an in-memory store, or a query that
    /// fails — neither is worth failing an indexing run over).
    ///
    /// `PRAGMA database_size` reports the blocks in use and the blocks free;
    /// a full reindex deletes every row and writes them again, and the freed
    /// blocks stay in the file. This is the number that makes that visible.
    pub fn bloat_ratio(&self) -> Option<f64> {
        let (used, free): (i64, i64) = self
            .conn
            .query_row(
                "SELECT total_blocks - free_blocks, free_blocks FROM pragma_database_size()",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()?;
        if used <= 0 {
            return None;
        }
        Some((used + free) as f64 / used as f64)
    }

    /// Rebuild every table, restoring index structures from the rows themselves.
    ///
    /// The repair for the damage [`checkpoint`](Self::checkpoint) prevents: copy
    /// each table aside, drop it, recreate it from the schema, and write the rows
    /// back. Recreating the table builds its ART index from the data, so index
    /// and table agree again.
    ///
    /// Needs exclusive access — stop any server on this database first.
    pub fn rebuild_indexes(&self) -> Result<Vec<String>> {
        // Both derived indexes are dropped rather than carried across: `vectors`
        // is about to be recreated underneath them, and each is rebuilt on demand
        // by the next indexing run.
        self.drop_fts()?;
        self.drop_hnsw()?;
        // Read the width off the stored column, not from whatever this process
        // was configured with: a repair must not quietly change the schema.
        let dim = self.stored_dimension()?.unwrap_or(self.dim);
        let mut repaired = Vec::new();
        for table in self.table_names()? {
            let tmp = format!("devctx_repair_{table}");
            self.w()?
                .execute_batch(&format!("DROP TABLE IF EXISTS {tmp};"))?;
            self.w()?.execute_batch(&format!(
                "CREATE TABLE {tmp} AS SELECT * FROM {table};
                 DROP TABLE {table};"
            ))?;
            schema::init_schema(&self.conn, dim)?;
            self.w()?.execute_batch(&format!(
                "INSERT INTO {table} SELECT * FROM {tmp};
                 DROP TABLE {tmp};"
            ))?;
            repaired.push(table);
        }
        self.checkpoint();
        Ok(repaired)
    }

    /// The store's own tables, in catalog order.
    fn table_names(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = 'main' AND table_type = 'BASE TABLE'
               AND table_name NOT LIKE 'devctx_repair_%'
             ORDER BY table_name",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(std::result::Result::ok).collect())
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
            out.push(SearchResult::new(point, score));
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

        // Inside a caller's transaction (one file's writes, see
        // `in_transaction`) the rows simply join it; DuckDB has no nested
        // transactions to open here.
        if self.in_tx.load(Ordering::SeqCst) {
            return self.upsert_inner(points);
        }
        self.in_transaction(|| self.upsert_inner(points))
    }

    /// Run `f` as one transaction: every write it makes is committed together,
    /// or — on an error, or a process that dies part-way — none of them is.
    ///
    /// The unit an indexing run writes per file. Its five writes (the old
    /// vectors out, the new ones in, edges, routes, the file's content hash)
    /// used to be five autocommits, and a process ending between the first and
    /// the last left a file whose hash said "indexed, unchanged" over no
    /// vectors at all: a hole no incremental run would ever look at again.
    ///
    /// Re-entrant: a call made while this connection is already inside one
    /// runs `f` in the outer transaction.
    pub fn in_transaction<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        if self.in_tx.load(Ordering::SeqCst) {
            return f();
        }
        self.w()?.execute_batch("BEGIN TRANSACTION")?;
        self.in_tx.store(true, Ordering::SeqCst);
        let result = f();
        let result = match result {
            Ok(v) => match self.w().and_then(|c| Ok(c.execute_batch("COMMIT")?)) {
                Ok(()) => Ok(v),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        };
        if result.is_err() {
            // Never gated: a rollback writes nothing to the WAL, and a frozen
            // store must still be able to abandon what it started.
            let _ = self.conn.execute_batch("ROLLBACK");
        }
        self.in_tx.store(false, Ordering::SeqCst);
        result
    }

    /// [`in_transaction`](Self::in_transaction), rolled back — nothing kept —
    /// when `f` decides there was nothing to do (`Ok(None)`). Not for use
    /// inside another transaction, where there is nothing of its own to roll
    /// back.
    pub fn in_transaction_opt<T>(
        &self,
        f: impl FnOnce() -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        let mut nothing = false;
        let out = self.in_transaction(|| match f()? {
            Some(v) => Ok(Some(v)),
            None => {
                nothing = true;
                Err(StoreError::Decode("nothing to commit".into()))
            }
        });
        if nothing {
            return Ok(None);
        }
        out
    }

    /// The connection, for a statement that writes — or an error once the
    /// database has been [frozen](Self::freeze).
    ///
    /// The returned guard holds the freeze gate's shared side for as long as
    /// the statement runs (it lives until the end of the caller's expression),
    /// so a freeze cannot slip in between "not frozen yet" and the write.
    pub(crate) fn w(&self) -> Result<WriteConn<'_>> {
        let gate = self
            .shared
            .gate
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.shared.frozen.load(Ordering::SeqCst) {
            return Err(StoreError::Frozen);
        }
        Ok(WriteConn {
            _gate: gate,
            conn: &self.conn,
        })
    }

    /// Refuse every write from now on, on this connection and on every
    /// connection [cloned](Self::try_clone) from the same open. Returns once no
    /// write is in progress any more — or after `wait`, whichever comes first
    /// (`false` then: some statement was still running).
    ///
    /// For the last moments of a process: freeze, then checkpoint, then end.
    /// A write that landed between the exit checkpoint and `_exit` — an
    /// indexing thread's next autocommit — sat alone in a fresh WAL, and
    /// replaying an `INSERT` into a table with a `PRIMARY KEY` after a
    /// checkpoint is exactly what leaves the ART index without its entries
    /// (see [`checkpoint`](Self::checkpoint)). Frozen, that write fails instead.
    /// Checkpoints and rollbacks are not writes and stay allowed.
    pub fn freeze(&self, wait: Duration) -> bool {
        self.shared.frozen.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + wait;
        loop {
            match self.shared.gate.try_write() {
                Ok(_) | Err(std::sync::TryLockError::Poisoned(_)) => return true,
                Err(std::sync::TryLockError::WouldBlock) => {}
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Whether [`freeze`](Self::freeze) was called on this database.
    pub fn is_frozen(&self) -> bool {
        self.shared.frozen.load(Ordering::SeqCst)
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
            self.w()?.execute(
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
            self.w()?.execute(
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
        // The distance function must be the one the index was built with, or
        // the VSS optimizer will not match it.
        let ip = self.hnsw_metric().as_deref() == Some("ip");
        let func = if ip {
            "array_negative_inner_product"
        } else {
            "array_cosine_distance"
        };
        let dist_expr = format!(
            "{func}(vector, {qv}::FLOAT[{dim}])",
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
            // Scores stay comparable across metrics: cosine distance is
            // `1 - similarity`, negative inner product is `-similarity`, and on
            // unit-normalized vectors both similarities are the same number.
            out.push(SearchResult::new(
                point,
                if ip { -dist } else { 1.0 - dist },
            ));
        }
        Ok(out)
    }

    /// The stored embedding for `id`, or `None` when there is none.
    ///
    /// Vectors could be searched but never fetched, which is what export needs:
    /// carrying the embedding lets an import between two machines on the same
    /// model skip recomputing it — measured at 46 minutes for 2090 memories.
    pub fn vector_by_id(&self, id: &str) -> Result<Option<Vec<f32>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT vector FROM vectors WHERE id = ?")?;
        match stmt.query_row(params![id], |r| r.get::<_, Value>(0)) {
            Ok(v) => Ok(Some(value_to_f32_vec(v))),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete every vector for a given file.
    pub fn delete_by_file(&self, repo: &str, branch: &str, file: &str) -> Result<usize> {
        let n = self.w()?.execute(
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
        let n = self.w()?.execute(&sql, params_from_iter(ids.iter()))?;
        Ok(n)
    }

    /// Update the `file` column for every row of a renamed file.
    pub fn rename_file(&self, repo: &str, branch: &str, old: &str, new: &str) -> Result<usize> {
        let n = self.w()?.execute(
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

    /// Chunks that define a symbol, by name.
    ///
    /// Two passes, because a caller has a name and the index has ids. An exact
    /// hit on `symbol` is what a name typed from memory produces; the suffix
    /// pass catches the qualified spellings the parsers emit for methods
    /// (`Card.charge`, `path/pay.rs::charge`), which is what a caller
    /// copy-pasting from `impact` or `get_references` has in hand. Doing the
    /// exact pass first means the common case never pays for the scan.
    ///
    /// A third pass, only when both miss, finds a small function inside a
    /// `grouped` chunk (several small functions indexed together).
    ///
    /// Definitions only — chunks whose `chunk_level` is not a call site — so the
    /// answer is the code of the symbol rather than every place it appears.
    pub fn symbol_definitions(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
        limit: usize,
    ) -> Result<Vec<VectorPoint>> {
        let exact = format!(
            "SELECT {COLS} FROM vectors
             WHERE repo = ? AND branch = ? AND symbol = ? AND NOT is_deletion
             ORDER BY file, start_line LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&exact)?;
        let rows = stmt.query_map([repo, branch, name], row_to_point)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        if !out.is_empty() {
            return Ok(out);
        }

        // `Card.charge` and `pay.rs::charge` both end in `charge`; anchoring on
        // the separator keeps `recharge` out.
        // `ends_with` rather than `LIKE '%.name'`: `_` is a LIKE wildcard.
        let sql = format!(
            "SELECT {COLS} FROM vectors
             WHERE repo = ? AND branch = ? AND NOT is_deletion
               AND (ends_with(symbol, '.' || ?) OR ends_with(symbol, '::' || ?))
             ORDER BY file, start_line LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([repo, branch, name, name], row_to_point)?;
        for r in rows {
            out.push(r?);
        }
        if !out.is_empty() {
            return Ok(out);
        }

        // Small functions are chunked together under a symbol that lists them
        // (`a, b, c`, or `a, b, c, d +2` past four names). An exact element of
        // the list is a definition; so is a name past the cap, which the chunk
        // text still carries in its per-function header (`# file > name`, or
        // `# file > Parent > name`). Without this pass a small getter — which
        // calls nothing, so is never a graph source either — looked external.
        //
        // The header is matched as a whole line that starts with `# `, as
        // `context_header` in devctx-chunk writes it: a bare `' > name\n'`
        // also matched code in the group (`return a > limit\n`).
        let header = format!("(^|\n)# [^\n]* > {}(\n|$)", regex_escape(name));
        let sql = format!(
            "SELECT {COLS} FROM vectors
             WHERE repo = ? AND branch = ? AND NOT is_deletion
               AND symbol_type = 'grouped'
               AND (list_contains(string_split(regexp_replace(symbol, ' \\+[0-9]+$', ''), ', '), ?)
                    OR regexp_matches(text, ?))
             ORDER BY file, start_line LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([repo, branch, name, header.as_str()], row_to_point)?;
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Up to `n` symbol names of this repo/branch that `name` could be a slip for.
    ///
    /// Candidates come from SQL by cheap string tests (shared prefix, shared
    /// suffix, or containing the name, all case-insensitive), ordered there by
    /// DuckDB's `levenshtein` / `jaro_winkler_similarity` on the last path
    /// segment and capped, so the cap is deterministic and keeps the closest
    /// names. They are then classed in memory: same name ignoring case, then
    /// prefix/suffix/substring, then small edit distance.
    pub fn symbol_suggestions(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
        n: usize,
    ) -> Result<Vec<String>> {
        let lname = name.to_lowercase();
        if lname.chars().count() < 2 || n == 0 {
            return Ok(Vec::new());
        }
        let head: String = lname.chars().take(3).collect();
        let tail: String = {
            let c: Vec<char> = lname.chars().collect();
            c[c.len().saturating_sub(3)..].iter().collect()
        };
        // Ranked in SQL before the cap, so the cap keeps the closest names
        // rather than whichever the scan met first: edit distance on the last
        // path segment (`Card.charge` → `charge`), then Jaro-Winkler, then the
        // name itself as a deterministic tie-break. "Contained in the query"
        // only counts for names of three letters or more — `a` or `id` are
        // inside almost anything. Grouped chunks list several names in one
        // symbol (`a, b, c`) and are not a name to suggest.
        let mut stmt = self.conn.prepare(
            "SELECT symbol FROM (
               SELECT DISTINCT symbol, regexp_extract(lower(symbol), '[^.:]*$') AS last
                 FROM vectors
                WHERE repo = ? AND branch = ? AND NOT is_deletion
                  AND chunk_level NOT IN ('memory', 'memory_chunk')
                  AND symbol_type <> 'grouped'
                  AND symbol <> ''
                  AND (starts_with(lower(symbol), ?) OR ends_with(lower(symbol), ?)
                       OR contains(lower(symbol), ?)
                       OR (length(symbol) >= 3 AND contains(?, lower(symbol))))
             )
             ORDER BY levenshtein(last, ?), jaro_winkler_similarity(last, ?) DESC, symbol
             LIMIT 200",
        )?;
        let rows = stmt.query_map(
            params![repo, branch, head, tail, lname, lname, lname, lname],
            |r| r.get::<_, String>(0),
        )?;
        let mut cands = Vec::new();
        for r in rows {
            cands.push(r?);
        }
        Ok(rank_suggestions(name, cands, n))
    }

    /// Definitions of `name` among the rows `filter` admits — the exact symbol
    /// first, then `Class.name` / `mod::name` — without requiring a repo and
    /// branch the way [`symbol_definitions`](Self::symbol_definitions) does.
    ///
    /// Memory rows are never definitions (their `symbol` is a title). Used to
    /// anchor an identifier query on the code that defines it.
    pub fn symbol_matches(
        &self,
        filter: &SearchFilter,
        name: &str,
        limit: usize,
    ) -> Result<Vec<VectorPoint>> {
        let (where_clause, fparams) = build_where(filter);
        let base = if where_clause.is_empty() {
            "WHERE chunk_level NOT IN ('memory', 'memory_chunk')".to_string()
        } else {
            format!("{where_clause} AND chunk_level NOT IN ('memory', 'memory_chunk')")
        };
        let run = |cond: &str, extra: Vec<String>| -> Result<Vec<VectorPoint>> {
            let sql = format!(
                "SELECT {COLS} FROM vectors {base} AND ({cond})
                 ORDER BY file, start_line LIMIT {limit}"
            );
            let mut params = fparams.clone();
            params.extend(extra);
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), row_to_point)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        };
        let exact = run("symbol = ?", vec![name.to_string()])?;
        if !exact.is_empty() {
            return Ok(exact);
        }
        // `ends_with`, not `LIKE '%.name'`: `_` is a LIKE wildcard, so
        // `get_x` would match `Foo.getAx` (and a leading `%` scans anyway).
        run(
            "ends_with(symbol, '.' || ?) OR ends_with(symbol, '::' || ?)",
            vec![name.to_string(), name.to_string()],
        )
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

/// Escape `s` for a literal match inside an RE2 pattern.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
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
pub(crate) fn row_to_point(row: &duckdb::Row<'_>) -> duckdb::Result<VectorPoint> {
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

impl Store {
    /// Forget the resolved metric so the next search re-reads it.
    fn invalidate_metric_cache(&self) {
        if let Ok(mut g) = self.metric_cache.lock() {
            *g = None;
        }
    }
}

/// Accept only the metrics this store knows how to query, defaulting to the
/// always-correct one. The value reaches SQL by interpolation — both in the
/// index name and in `WITH (metric = …)` — so anything unrecognized is mapped
/// to `cosine` rather than passed through.
fn normalize_metric(metric: &str) -> &'static str {
    match metric.trim().to_ascii_lowercase().as_str() {
        "ip" | "inner_product" => "ip",
        _ => "cosine",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_metrics_reach_sql() {
        assert_eq!(normalize_metric("ip"), "ip");
        assert_eq!(normalize_metric("Inner_Product"), "ip");
        assert_eq!(normalize_metric("cosine"), "cosine");
        assert_eq!(normalize_metric(""), "cosine");
        assert_eq!(normalize_metric("l2sq"), "cosine");
        // Nothing quotable can be smuggled into the interpolated statement.
        assert_eq!(normalize_metric("ip'); DROP TABLE vectors; --"), "cosine");
    }

    #[test]
    fn size_literals_are_accepted_and_anything_quotable_is_not() {
        assert!(is_size_literal("2GB"));
        assert!(is_size_literal("512MB"));
        assert!(is_size_literal("1.5GiB"));
        assert!(!is_size_literal(""));
        // The value is interpolated into `SET`, where DuckDB takes no bound
        // parameter, so nothing that could close the quote may pass.
        assert!(!is_size_literal("2GB'; DROP TABLE vectors; --"));
        assert!(!is_size_literal("2 GB"));
    }

    /// PLAN-008 TASK-009 fixup D1: the exit path checkpoints from a different
    /// connection than the one an indexing run writes through. A plain
    /// `CHECKPOINT` is refused while that run has a write transaction open —
    /// and the process then exited with the WAL unfolded. The forced variant
    /// must fold it regardless.
    #[test]
    fn a_forced_checkpoint_folds_the_wal_past_an_open_write_transaction() {
        let dir = std::env::temp_dir().join(format!("devctx_force_ckpt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("index.duckdb");
        let wal = dir.join("index.duckdb.wal");
        let store = Store::open(&path, 4).unwrap();
        store
            .set_index_meta("/r", "main", "k", "committed")
            .unwrap();
        let other = store.try_clone().unwrap();
        other.conn.execute_batch("BEGIN TRANSACTION").unwrap();
        other
            .set_index_meta("/r", "main", "k2", "in flight")
            .unwrap();
        assert!(
            std::fs::metadata(&wal)
                .map(|m| m.len() > 0)
                .unwrap_or(false),
            "the committed write should sit in the WAL"
        );
        // Measured on the bundled DuckDB: a plain CHECKPOINT is *not* refused
        // by an open transaction whose writes are still transaction-local; it
        // folds what is committed. Whatever it does, it must not claim success
        // and leave the WAL behind.
        let plain = store.try_checkpoint();
        let wal_after_plain = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
        assert!(
            plain.is_err() || wal_after_plain == 0,
            "a CHECKPOINT that reports success must have folded the WAL ({wal_after_plain} bytes left)"
        );
        // Another committed write, with the transaction still open: the forced
        // variant the exit path escalates to must fold it too.
        store
            .set_index_meta("/r", "main", "k3", "committed later")
            .unwrap();
        store.force_checkpoint().expect("FORCE CHECKPOINT");
        assert!(
            std::fs::metadata(&wal)
                .map(|m| m.len() == 0)
                .unwrap_or(true),
            "the WAL must be folded after a forced checkpoint"
        );
        drop(other);
        drop(store);
        let reopened = Store::open(&path, 4).unwrap();
        assert_eq!(
            reopened
                .get_index_meta("/r", "main", "k")
                .unwrap()
                .as_deref(),
            Some("committed")
        );
        assert_eq!(
            reopened
                .get_index_meta("/r", "main", "k3")
                .unwrap()
                .as_deref(),
            Some("committed later")
        );
        assert_eq!(reopened.get_index_meta("/r", "main", "k2").unwrap(), None);
        drop(reopened);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("devctx_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wal_len(dir: &Path) -> u64 {
        std::fs::metadata(dir.join("index.duckdb.wal"))
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// The per-function header of a grouped chunk is a whole `# ` line; code
    /// in the group that merely reads `a > limit` is not a definition of
    /// `limit`.
    #[test]
    fn a_grouped_definition_matches_the_header_line_not_the_code() {
        let store = Store::open_in_memory(4).unwrap();
        let grouped = |id: &str, text: &str| {
            let mut p = point(id, &format!("{id}.js"));
            p.text = text.into();
            p.metadata.symbol = "a, b, c, d +2".into();
            p.metadata.symbol_type = "grouped".into();
            p.metadata.chunk_level = "function".into();
            p
        };
        store
            .upsert(&[
                grouped(
                    "code",
                    "# code.js > check\nfunction check(a) {\n  return a > limit\n}",
                ),
                grouped(
                    "def",
                    "# def.js > other\nfunction other() {}\n\n# def.js > Box > limit\nfunction limit() {}",
                ),
                grouped("tail", "# tail.js > a\nfunction a() {}\n\n# tail.js > last"),
            ])
            .unwrap();
        let ids = |name: &str| -> Vec<String> {
            store
                .symbol_definitions("demo", "main", name, 10)
                .unwrap()
                .into_iter()
                .map(|p| p.id)
                .collect()
        };
        assert_eq!(ids("limit"), ["def"]);
        assert_eq!(ids("last"), ["tail"], "a header on the last line");
        assert!(ids("lim").is_empty(), "the name is matched whole");
    }

    fn point(id: &str, file: &str) -> VectorPoint {
        VectorPoint {
            id: id.into(),
            vector: vec![0.5; 4],
            text: format!("text {id}"),
            metadata: VectorMetadata {
                repo: "demo".into(),
                branch: "main".into(),
                file: file.into(),
                ..Default::default()
            },
        }
    }

    /// PLAN-008 TASK-009 fixup D1b (item 2): the exit path checkpoints and then
    /// `_exit`s while an indexing thread is still alive. Once frozen, a write
    /// from any connection of the database — the indexing thread's next
    /// autocommit into a table with a PRIMARY KEY — must fail instead of
    /// landing in a fresh WAL that nothing will fold before the process ends.
    #[test]
    fn a_frozen_store_refuses_writes_and_leaves_the_wal_alone() {
        let dir = scratch("freeze");
        let path = dir.join("index.duckdb");
        let store = Store::open(&path, 4).unwrap();
        store.set_index_meta("/r", "main", "k", "before").unwrap();
        let indexer = store.try_clone().unwrap();
        assert!(store.freeze(Duration::from_secs(1)), "nothing was writing");
        store.try_checkpoint().expect("a checkpoint is not a write");
        assert_eq!(wal_len(&dir), 0, "the exit checkpoint folded the WAL");

        let refused = [
            indexer
                .save_file_state(&crate::FileState {
                    repo_path: "/r".into(),
                    branch: "main".into(),
                    file_path: "a.rs".into(),
                    content_hash: "h".into(),
                    language: "rust".into(),
                    symbol_count: 0,
                    chunk_count: 1,
                })
                .err(),
            indexer.set_index_meta("/r", "main", "k2", "after").err(),
            indexer.upsert(&[point("p", "a.rs")]).err(),
            indexer.delete_by_file("demo", "main", "a.rs").err(),
        ];
        for (i, e) in refused.into_iter().enumerate() {
            assert!(
                matches!(e, Some(StoreError::Frozen)),
                "write #{i} after the freeze was not refused: {e:?}"
            );
        }
        assert!(indexer.is_frozen(), "the freeze covers cloned connections");
        assert_eq!(
            wal_len(&dir),
            0,
            "no write reached the WAL after the freeze"
        );
        drop(indexer);
        drop(store);
        let reopened = Store::open(&path, 4).unwrap();
        assert_eq!(
            reopened
                .get_index_meta("/r", "main", "k")
                .unwrap()
                .as_deref(),
            Some("before")
        );
        assert_eq!(reopened.get_index_meta("/r", "main", "k2").unwrap(), None);
        assert!(
            !reopened.is_frozen(),
            "a freeze lasts one process, not the file"
        );
        drop(reopened);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A freeze waits for a statement already running (it may be the commit
    /// of a file's writes), but only as long as it is told to.
    #[test]
    fn a_freeze_waits_for_a_write_in_progress_up_to_its_budget() {
        let store = Store::open_in_memory(4).unwrap();
        let other = store.try_clone().unwrap();
        let running = other.w().unwrap();
        let t0 = Instant::now();
        assert!(
            !store.freeze(Duration::from_millis(200)),
            "a write is still running"
        );
        assert!(t0.elapsed() >= Duration::from_millis(200));
        drop(running);
        assert!(store.freeze(Duration::from_millis(200)));
        assert!(matches!(other.w().err(), Some(StoreError::Frozen)));
    }

    /// One transaction is all or nothing, and an upsert inside it joins it
    /// rather than committing on its own.
    #[test]
    fn a_transaction_commits_everything_or_nothing() {
        let store = Store::open_in_memory(4).unwrap();
        store.upsert(&[point("old", "a.rs")]).unwrap();
        let failed: Result<()> = store.in_transaction(|| {
            store.delete_by_file("demo", "main", "a.rs")?;
            store.upsert(&[point("new", "a.rs")])?;
            Err(StoreError::Decode("die half-way".into()))
        });
        assert!(failed.is_err());
        let ids: Vec<String> = store
            .scroll_all("demo", "main")
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, vec!["old".to_string()], "the delete was rolled back");

        store
            .in_transaction(|| {
                store.delete_by_file("demo", "main", "a.rs")?;
                store.upsert(&[point("new", "a.rs")])
            })
            .unwrap();
        let ids: Vec<String> = store
            .scroll_all("demo", "main")
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, vec!["new".to_string()]);
    }

    #[test]
    fn env_usize_falls_back_when_absent() {
        assert_eq!(env_usize("DEVCTX_TEST_DEFINITELY_UNSET", 4), 4);
    }

    /// The whole point of [`Store::apply_resource_limits`]: a fresh store must
    /// not inherit DuckDB's one-thread-per-core default, because several
    /// servers run at once and none of them owns the machine.
    #[test]
    fn a_new_store_caps_its_own_thread_count() {
        let store = Store::open_in_memory(8).expect("opening an in-memory store");
        let threads: i64 = store
            .conn
            .query_row(
                "SELECT value::BIGINT FROM duckdb_settings() WHERE name = 'threads'",
                [],
                |r| r.get(0),
            )
            .expect("reading the thread count back");
        // Derived the same way the setter derives it, so an operator running the
        // suite with `DEVCTX_DB_THREADS` set does not see a spurious failure.
        let expected = env_usize("DEVCTX_DB_THREADS", DEFAULT_THREADS).max(1) as i64;
        assert_eq!(threads, expected);
    }

    /// Regression for a WAL that a crash leaves unreplayable.
    ///
    /// A bare first-time `create_fts_index` replays fine even uncheckpointed —
    /// WAL replay of a plain `CREATE` is the ordinary case. The failure this
    /// repo actually hit ("Cannot drop entry \"fts_main_vectors\" because
    /// there are entries that depend on it... Use DROP...CASCADE") comes from
    /// the *drop* half: `drop_fts` issues `PRAGMA drop_fts_index`, DDL that
    /// tears down the `fts_main_vectors` schema and its dependent
    /// `terms`/`docs`/`dict` tables, and if the process dies before the next
    /// checkpoint folds that into the database file, DuckDB cannot replay the
    /// drop on the next open — the database is unopenable for good, forever
    /// (a reindex cannot fix it either, because reindexing itself begins by
    /// deleting).
    ///
    /// So the crash producer below builds the index, *checkpoints* (steady
    /// state — an index that has always been there), then drops it and exits
    /// via [`std::process::abort`] with no checkpoint after the drop. `abort`
    /// (unlike [`std::process::exit`], which still runs C `atexit`/static
    /// destructors — DuckDB is a C++ library and could be using one to flush
    /// on ordinary process exit) raises `SIGABRT` and runs nothing, the
    /// closest a same-process simulation gets to `kill -9`.
    #[test]
    fn fts_drop_survives_a_crash_before_the_next_checkpoint() {
        let path =
            std::env::temp_dir().join(format!("devctx_fts_crash_{}.duckdb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("duckdb.wal"));

        let exe = std::env::current_exe().expect("test binary path");
        let status = std::process::Command::new(exe)
            .arg("store::tests::crash_producer_drop_fts")
            .arg("--exact")
            .arg("--nocapture")
            .env("DEVCTX_CRASH_TEST_DB", &path)
            .status()
            .expect("spawning the crash-producer child");

        if !path.exists() {
            // The child bailed out early: the FTS extension is unavailable in
            // this environment (e.g. offline), and it exited normally on
            // purpose. Checked before the status, or that exit reads as a
            // failure. Nothing to verify.
            return;
        }
        assert!(
            !status.success(),
            "the crash producer is expected to abort, not return normally"
        );

        let store = Store::open(&path, 8).expect(
            "reopening a database crashed right after drop_fts must not fail to \
             replay its WAL",
        );
        // The point isn't whether FTS is still built (it was mid-drop) — it's
        // that the database opens at all instead of being bricked.
        let _ = store.has_fts();

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("duckdb.wal"));
    }

    /// Child entry point for
    /// [`fts_drop_survives_a_crash_before_the_next_checkpoint`]. A no-op
    /// under an ordinary `cargo test` run — the env var is only ever set by
    /// that test, which spawns this same binary with it set to the scratch
    /// database path.
    #[test]
    fn crash_producer_drop_fts() {
        let Ok(path) = std::env::var("DEVCTX_CRASH_TEST_DB") else {
            return;
        };
        let store = Store::open(Path::new(&path), 8).expect("opening the scratch store");
        store
            .upsert(&[VectorPoint {
                id: "a".into(),
                vector: vec![0.0; 8],
                text: "connect to the postgres database".into(),
                metadata: VectorMetadata {
                    repo: "demo".into(),
                    branch: "main".into(),
                    language: "rust".into(),
                    ..Default::default()
                },
            }])
            .expect("seeding a row");
        if !store.rebuild_fts().expect("rebuild_fts") {
            // FTS extension unavailable here either; leave no file behind so
            // the parent knows to skip its assertion, then exit *normally* —
            // no drop happened, so there is nothing to abort mid-way.
            let _ = std::fs::remove_file(&path);
            std::process::exit(0);
        }
        // Steady state: the index has been built and checkpointed, same as an
        // index that has existed since a previous run.
        store.checkpoint();
        // The risky part: drop it, then crash before the next checkpoint.
        store.drop_fts().expect("drop_fts");
        std::process::abort();
    }
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

/// Rank candidate symbols against a name that matched nothing. Pure.
fn rank_suggestions(name: &str, cands: Vec<String>, n: usize) -> Vec<String> {
    let lname = name.to_lowercase();
    let threshold = (lname.chars().count() / 3).max(2);
    let mut scored: Vec<(u8, usize, String)> = Vec::new();
    for c in cands {
        let last = c.rsplit(['.', ':']).next().unwrap_or(&c).to_lowercase();
        if last.is_empty() || c == name {
            continue;
        }
        let dist = edit_distance(&lname, &last);
        let class = if last == lname {
            0
        } else if last.starts_with(&lname) || last.ends_with(&lname) || last.contains(&lname) {
            1
        } else if dist <= threshold {
            2
        } else {
            continue;
        };
        scored.push((class, dist, c));
    }
    scored.sort();
    scored.dedup_by(|a, b| a.2 == b.2);
    scored.into_iter().take(n).map(|(_, _, c)| c).collect()
}

/// Levenshtein distance over chars.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod suggestion_tests {
    use super::*;

    #[test]
    fn suggestions_rank_exact_case_then_substring_then_edit_distance() {
        let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let r = rank_suggestions(
            "AuthServce",
            c(&["AuthService", "Unrelated", "Other.Zzzz"]),
            5,
        );
        assert_eq!(r[0], "AuthService");
        assert!(!r.contains(&"Unrelated".to_string()));
        let r = rank_suggestions("charge", c(&["Card.Charge", "Card.recharge", "zzz"]), 5);
        assert_eq!(r[0], "Card.Charge");
        assert_eq!(r[1], "Card.recharge");
        assert_eq!(rank_suggestions("x1", c(&["x1"]), 5), Vec::<String>::new());
    }

    fn sym(id: &str, symbol: &str) -> VectorPoint {
        VectorPoint {
            id: id.into(),
            vector: vec![0.0; 3],
            text: String::new(),
            metadata: VectorMetadata {
                repo: "r".into(),
                branch: "main".into(),
                file: "a.rs".into(),
                symbol: symbol.into(),
                symbol_type: "function".into(),
                chunk_level: "function".into(),
                ..Default::default()
            },
        }
    }

    /// The candidate cap used to come without an order: past it, which names
    /// survived was up to the scan, and one- or two-letter symbols (contained
    /// in almost any name) filled it. The closest name must always be offered.
    #[test]
    fn the_closest_symbol_survives_a_flood_of_candidates() {
        let store = Store::open_in_memory(3).unwrap();
        let mut pts: Vec<VectorPoint> = (0..20000)
            .map(|i| sym(&format!("n{i}"), &format!("authz_noise_{i:05}")))
            .collect();
        for (i, short) in ["a", "u", "t", "h", "se", "rv"].iter().enumerate() {
            pts.push(sym(&format!("s{i}"), short));
        }
        pts.push(sym("g", "AuthService, other"));
        pts.push(sym("want", "AuthService"));
        store.upsert(&pts).unwrap();
        for _ in 0..3 {
            let r = store
                .symbol_suggestions("r", "main", "AuthServce", 5)
                .unwrap();
            assert_eq!(r.first().map(String::as_str), Some("AuthService"), "{r:?}");
            assert!(
                !r.iter().any(|c| c.len() <= 2),
                "short noise offered: {r:?}"
            );
        }
    }

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "ab"), 2);
        assert_eq!(edit_distance("same", "same"), 0);
    }
}
