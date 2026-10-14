//! Compaction: rewrite a database into a new file, verify it, and put it in
//! place with an atomic `rename` (PLAN-009 TASK-018).
//!
//! A full reindex that reuses vectors deletes and rewrites every row of
//! `vectors`, and DuckDB hands neither those blocks back (`VACUUM` does not;
//! a checkpoint only truncates free blocks at the very end of the file). A
//! 0.9.0 index upgraded in place ends up at ~2x. Copying row by row into a
//! fresh file brings it back to within 3-10 % of where it started.
//!
//! The protocol and its five invariants are those of the task's spec; in
//! short:
//!
//! - **I1** the compaction lock ([`crate::oslock`]) is held exclusively for
//!   the whole run; no [`Store::open`] succeeds meanwhile.
//! - **I2** one in-memory connection, `H`, adjoins the source read-only before
//!   reading anything and keeps it adjoined until after the `rename`: its
//!   DuckDB lock is what stops a binary that does not know the compaction
//!   lock. The process opens and closes **no other descriptor** of the
//!   source: an `fcntl` lock goes the moment any descriptor of the file is
//!   closed. Everything about the source is read through `H`, or `stat`ed by
//!   path.
//! - **I3** with the source adjoined, its WAL is absent or empty — right after
//!   the `ATTACH` and right before the `rename` — together with its size,
//!   mtime and inode unchanged (M-B.2). DuckDB applies a WAL it finds next to
//!   a file without checking it belongs there.
//! - **I4** nothing writes to the source between taking the lock and the
//!   `rename` (the declared exception is the *preparation* open that folds a
//!   WAL, before the `ATTACH`, and only when asked for).
//! - **I5** a temporary is never deleted while the database is missing, and
//!   the replacement is a `rename` over the database or nothing.
//!
//! "Answers the same" means the same **data** (row counts, a checksum of
//! every column of every row, and `EXCEPT ALL` both ways for what cannot be
//! regenerated) and the same **exact** top-k. Not the same approximate top-k
//! of the rebuilt HNSW index, whose graph differs from build to build.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use duckdb::Connection;

use crate::oslock::{CompactPaths, FileLock, LockMode};
use crate::store::{normalize_metric, Store};

/// `index_meta` key, for the whole database (`repo_path = branch = ''`), that
/// says whether a compaction is owed or when one was done.
pub const COMPACTION_META_KEY: &str = "compaction";
/// The note an upgrade from a pre-graph index leaves.
pub const COMPACTION_OWED_UPGRADE: &str = "owed:upgrade";
/// The note a compaction whose copy failed verification leaves; only
/// `devctx compact` retries it.
pub const COMPACTION_OWED_FAILED_VERIFY: &str = "owed:failed-verify";
/// `devctx-index`'s note of an HNSW index owed by an interrupted run.
pub const PENDING_HNSW_KEY: &str = "pending_hnsw";
/// `devctx-index`'s note of a BM25 index owed by an interrupted run.
pub const PENDING_FTS_KEY: &str = "pending_fts";

/// The tables nothing can regenerate: compared with `EXCEPT ALL` both ways,
/// and saved to the sidecar before the `rename`.
pub const IRREPLACEABLE: &[&str] = &["memories", "memory_symbol_references", "sessions"];

/// Indexes [`init_schema`](crate::init_schema) drops on every open: a source
/// may still carry them, and the copy is right not to.
const DROPPED_BY_OPEN: &[&str] = &[
    "idx_projects_path",
    "idx_projects_active",
    "idx_memories_topic",
    "idx_memories_hash",
    "idx_memories_type",
    "idx_memories_project",
];

/// The schema the FTS extension keeps its index in; rebuilt, never copied.
const FTS_SCHEMA: &str = "fts_main_vectors";

/// Blocks free at or above which a database counts as worth compacting: the
/// warning at the end of a full run and the `status` hint use it (not the
/// trigger after an upgrade, which goes by the note).
pub const WORTH_FREE_FRACTION: f64 = 0.25;
/// And at least this many free blocks (16 MiB of 256 KiB blocks).
pub const WORTH_FREE_BLOCKS: u64 = 64;

/// Used and free blocks of a database.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct BlockUsage {
    /// Bytes per block.
    pub block_size: u64,
    /// Blocks holding data.
    pub used: u64,
    /// Blocks the file has but nothing uses.
    pub free: u64,
}

impl BlockUsage {
    /// Whether this much free space is worth a compaction (see
    /// [`WORTH_FREE_FRACTION`]).
    pub fn worth_compacting(&self) -> bool {
        let total = self.used + self.free;
        total > 0
            && self.free >= WORTH_FREE_BLOCKS
            && self.free as f64 >= WORTH_FREE_FRACTION * total as f64
    }
}

impl Store {
    /// Used and free blocks of this database, `None` in memory or when
    /// DuckDB will not say.
    pub fn block_usage(&self) -> Option<BlockUsage> {
        self.conn
            .query_row(
                "SELECT block_size, used_blocks, free_blocks FROM pragma_database_size() \
                 WHERE database_name = current_database()",
                [],
                |r| {
                    Ok(BlockUsage {
                        block_size: r.get::<_, i64>(0)?.max(0) as u64,
                        used: r.get::<_, i64>(1)?.max(0) as u64,
                        free: r.get::<_, i64>(2)?.max(0) as u64,
                    })
                },
            )
            .ok()
            .filter(|u| u.used > 0)
    }
}

/// Where a compaction is. The hooks of the tests stop, break or cancel it at
/// each of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // variants are matched by the tests' hooks
pub(crate) enum Phase {
    /// The source is adjoined to `H` (I3 asserted once).
    Attached,
    /// The copy exists with this binary's schema and the inventory passed.
    Inventoried,
    /// That many tables copied (1-based), `dst` still adjoined.
    CopiedTable(usize),
    /// Every table copied and the note written; `dst` still adjoined.
    Copied,
    /// HNSW and FTS rebuilt on the copy, through another connection.
    IndexesBuilt,
    /// The copy reopened cold and its indexes answered.
    ColdVerified,
    /// Compared with the source. Every auxiliary connection is closed: `H`
    /// with the source adjoined is all that is open.
    Verified,
    /// The memories sidecar is in place.
    SidecarWritten,
    /// Right before the `rename` (after the last I3 assertion).
    BeforeRename,
    /// Right after the `rename`, before the lock is released.
    Renamed,
}

/// A hook of the tests: may stop the run (`Err`), or do anything to it.
pub(crate) type Hook<'a> = &'a dyn Fn(Phase, &Connection) -> std::result::Result<(), String>;

/// What the machine offers, injectable so each preflight refusal is testable.
#[derive(Clone, Copy)]
pub struct SystemProbe {
    /// Bytes available to this user on the filesystem of a path.
    pub available_disk: fn(&Path) -> Option<u64>,
    /// Bytes of memory available to start new work without swapping.
    pub available_memory: fn() -> Option<u64>,
    /// The effective user, where there is one.
    pub effective_uid: fn() -> Option<u32>,
    /// Whether a directory can be written to.
    pub dir_writable: fn(&Path) -> bool,
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self {
            available_disk: sys::available_disk,
            available_memory: sys::available_memory,
            effective_uid: sys::effective_uid,
            dir_writable: sys::dir_writable,
        }
    }
}

/// How to compact.
pub struct CompactOptions<'a> {
    /// The binary's version, for the note.
    pub version: String,
    /// Why: `upgrade` (after the upgrade of a pre-graph index) or `manual`.
    pub reason: String,
    /// Fold a WAL left next to the source before starting (`devctx compact`),
    /// with a normal open. Without it, a WAL aborts the run.
    pub fold_wal: bool,
    /// Leave a hard link of the original at `<db>.pre-compact`.
    pub keep_original: bool,
    /// Width to open the source with when folding its WAL; any value opens an
    /// existing database.
    pub dim_hint: usize,
    /// The HNSW index's size as the server reports it, for the memory check.
    pub hnsw_bytes: Option<u64>,
    /// Set from outside (Ctrl-C) to stop at the next phase, cleanly.
    pub cancel: Option<&'a AtomicBool>,
    /// The machine.
    pub probe: SystemProbe,
    pub(crate) hook: Option<Hook<'a>>,
    pub(crate) lock_probe: Option<fn(&Path) -> bool>,
    pub(crate) dir_sync: Option<fn(&Path) -> std::io::Result<()>>,
}

impl CompactOptions<'_> {
    /// Options for `reason`, with this crate's version.
    pub fn new(reason: &str) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            reason: reason.to_string(),
            fold_wal: false,
            keep_original: false,
            dim_hint: 384,
            hnsw_bytes: None,
            cancel: None,
            probe: SystemProbe::default(),
            hook: None,
            lock_probe: None,
            dir_sync: None,
        }
    }
}

/// Why a compaction did not happen. In every case the database is the one
/// that was there (I4), the temporaries are gone (unless the database itself
/// went missing: I5) and the lock is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    /// A preflight check refused (disk, memory, owner, read-only, extension).
    Preflight,
    /// The lock is held: a compaction runs, or the database is open.
    Busy,
    /// Another process holds the database (DuckDB's own lock).
    Locked,
    /// A WAL with content next to the source.
    WalPresent,
    /// The filesystem does not honour locks (M-B.1).
    LockUnsupported,
    /// The source changed under the run (M-B.2).
    SourceChanged,
    /// The source has an object this binary does not know.
    Unknown,
    /// A `devctx_repair_*` table with rows: maybe the only copy of a table.
    RepairLeftover,
    /// The copy does not hold what the source holds.
    Verify,
    /// Stopped on request.
    Cancelled,
    /// The `rename` itself failed.
    Rename,
    /// DuckDB or the filesystem failed.
    Engine,
    /// A finished copy next to the database holds what the database lacks
    /// (a reverted `rename`): nothing is touched (review D1).
    Unreconciled,
    /// The copy is in place but its directory entry could not be made
    /// durable; the lock is kept until the process ends (review D1).
    Durability,
}

/// A compaction that did not happen, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactError {
    /// What kind of refusal or failure.
    pub kind: FailKind,
    /// For a person.
    pub message: String,
}

impl std::fmt::Display for CompactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CompactError {}

impl CompactError {
    fn new(kind: FailKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The note to leave in the database after this failure, if it changes
    /// it: only a failed verification does (`owed:failed-verify`, which only
    /// `devctx compact` retries). Written by the caller, after the run: the
    /// compaction itself never writes to the source.
    pub fn note(&self) -> Option<&'static str> {
        (self.kind == FailKind::Verify).then_some(COMPACTION_OWED_FAILED_VERIFY)
    }
}

fn engine(e: impl std::fmt::Display) -> CompactError {
    let text = e.to_string();
    if text.contains("Could not set lock") || text.contains("Conflicting lock") {
        CompactError::new(
            FailKind::Locked,
            format!("another process holds the index ({text}); nothing was changed"),
        )
    } else {
        CompactError::new(FailKind::Engine, text)
    }
}

type CResult<T> = std::result::Result<T, CompactError>;

/// What a compaction did.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct CompactReport {
    /// The database, canonical.
    pub db: PathBuf,
    /// Blocks before (the source).
    pub before: BlockUsage,
    /// Blocks after (the copy, now in place).
    pub after: BlockUsage,
    /// File bytes before.
    pub bytes_before: u64,
    /// File bytes after.
    pub bytes_after: u64,
    /// Seconds per phase, in order.
    pub phases: Vec<(String, f64)>,
    /// Rows per table copied.
    pub rows: Vec<(String, u64)>,
    /// Metric of the HNSW index rebuilt, if any.
    pub hnsw: Option<String>,
    /// Whether the BM25 index was rebuilt.
    pub fts: bool,
    /// Columns the copy has and the source lacked, left NULL (`table.column`).
    pub null_filled: Vec<String>,
    /// Empty `devctx_repair_*` tables left out.
    pub ignored: Vec<String>,
    /// The memories sidecar.
    pub sidecar: PathBuf,
    /// An older sidecar kept under another name, because it held memories the
    /// source no longer has.
    pub kept_sidecar: Option<PathBuf>,
    /// The hard link of the original (`--keep-original`).
    pub original: Option<PathBuf>,
}

/// What the preflight found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preflight {
    /// The database, canonical.
    pub db: PathBuf,
    /// Its size in bytes.
    pub db_bytes: u64,
    /// Bytes available to this user next to it, when known.
    pub disk_available: Option<u64>,
    /// Memory available, when known.
    pub memory_available: Option<u64>,
    /// What the run is expected to need (DuckDB's limit plus the HNSW index).
    pub memory_needed: u64,
}

/// The checks that must pass before anything is stopped or written
/// (protocol, point 1). `hnsw_bytes`: the server's estimate of the index.
pub fn preflight(db: &Path, hnsw_bytes: Option<u64>, probe: &SystemProbe) -> CResult<Preflight> {
    let pre = |m: String| CompactError::new(FailKind::Preflight, m);
    let paths = CompactPaths::for_db(db);
    let meta = std::fs::metadata(&paths.db)
        .map_err(|_| pre(format!("there is no index at {}", paths.db.display())))?;
    let dir = paths.db.parent().unwrap_or(Path::new("."));
    if !(probe.dir_writable)(dir) {
        return Err(pre(format!(
            "{} cannot be written to (a read-only filesystem?); the copy is made next to the index",
            dir.display()
        )));
    }
    let db_bytes = meta.len();
    let disk_available = (probe.available_disk)(dir);
    if let Some(avail) = disk_available {
        if avail < db_bytes {
            return Err(pre(format!(
                "not enough disk next to the index: the copy needs up to {} MiB and {} MiB are \
                 available to you",
                mib(db_bytes),
                mib(avail)
            )));
        }
    }
    if let (Some(me), Some(owner)) = ((probe.effective_uid)(), sys::owner(&meta)) {
        if me != owner {
            return Err(pre(format!(
                "{} belongs to another user (uid {owner}); the copy would change its owner. Run \
                 the compaction as that user",
                paths.db.display()
            )));
        }
    }
    let memory_needed = memory_limit_bytes().saturating_add(hnsw_bytes.unwrap_or(0));
    let memory_available = (probe.available_memory)();
    if let Some(avail) = memory_available {
        if memory_needed as f64 > 0.8 * avail as f64 {
            return Err(pre(format!(
                "not enough memory: the copy and its HNSW index need about {} MiB, more than 80 % \
                 of the {} MiB available",
                mib(memory_needed),
                mib(avail)
            )));
        }
    }
    Ok(Preflight {
        db: paths.db,
        db_bytes,
        disk_available,
        memory_available,
        memory_needed,
    })
}

/// Whether the extensions a database with these indexes needs can be loaded
/// here; checked before stopping anything.
pub fn check_extensions(hnsw: bool, fts: bool) -> CResult<()> {
    let conn = Connection::open_in_memory().map_err(engine)?;
    let (vss, fts_ok) = load_extensions(&conn);
    if hnsw && !vss {
        return Err(CompactError::new(
            FailKind::Preflight,
            "the index has an HNSW index and the DuckDB VSS extension cannot be loaded here \
             (offline?); without it the copy cannot rebuild it",
        ));
    }
    if fts && !fts_ok {
        return Err(CompactError::new(
            FailKind::Preflight,
            "the index has a BM25 index and the DuckDB FTS extension cannot be loaded here",
        ));
    }
    Ok(())
}

fn mib(b: u64) -> u64 {
    b.div_ceil(1 << 20)
}

/// DuckDB's `memory_limit` as this process sets it, in bytes.
fn memory_limit_bytes() -> u64 {
    let v = crate::store::memory_limit_setting();
    parse_size(&v).unwrap_or(2_000_000_000)
}

/// A DuckDB size literal (`2GB`, `512MiB`, `1.5GiB`) in bytes.
fn parse_size(v: &str) -> Option<u64> {
    let v = v.trim();
    let split = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(v.len());
    let n: f64 = v[..split].parse().ok()?;
    let unit = v[split..].trim().to_ascii_lowercase();
    let mult: f64 = match unit.as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mult) as u64)
}

fn load_extensions(conn: &Connection) -> (bool, bool) {
    let vss = conn
        .execute_batch("INSTALL vss; LOAD vss; SET hnsw_enable_experimental_persistence = true;")
        .is_ok();
    let fts = conn.execute_batch("INSTALL fts; LOAD fts;").is_ok();
    (vss, fts)
}

/// A path as a SQL string literal.
fn lit(p: &Path) -> CResult<String> {
    let s = p.to_str().ok_or_else(|| {
        CompactError::new(
            FailKind::Preflight,
            format!("{} is not valid UTF-8", p.display()),
        )
    })?;
    Ok(format!("'{}'", s.replace('\'', "''")))
}

/// An identifier, quoted.
fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Size, mtime and inode of a file, by path (never by descriptor: see I2).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    ino: Option<u64>,
}

impl Stamp {
    fn of(p: &Path) -> CResult<Self> {
        let m = std::fs::metadata(p).map_err(|e| {
            CompactError::new(
                FailKind::SourceChanged,
                format!("{} cannot be read any more: {e}", p.display()),
            )
        })?;
        Ok(Self {
            len: m.len(),
            modified: m.modified().ok(),
            ino: sys::inode(&m),
        })
    }
}

/// I3: the WAL next to `db` is absent or empty.
fn assert_no_wal(paths: &CompactPaths) -> CResult<()> {
    match std::fs::metadata(&paths.wal) {
        Ok(m) if m.len() > 0 => Err(CompactError::new(
            FailKind::WalPresent,
            format!(
                "{} has writes not folded into the index ({} bytes); it was left untouched. \
                 Run `devctx compact`, which folds it first",
                paths.wal.display(),
                m.len()
            ),
        )),
        _ => Ok(()),
    }
}

fn query_strings(conn: &Connection, sql: &str) -> CResult<Vec<String>> {
    let mut st = conn.prepare(sql).map_err(engine)?;
    let rows = st
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(engine)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(engine)
}

fn query_pairs(conn: &Connection, sql: &str) -> CResult<Vec<(String, String)>> {
    let mut st = conn.prepare(sql).map_err(engine)?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(engine)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(engine)
}

fn query_u64(conn: &Connection, sql: &str) -> CResult<u64> {
    conn.query_row(sql, [], |r| r.get::<_, i64>(0))
        .map(|v| v.max(0) as u64)
        .map_err(engine)
}

fn usage(conn: &Connection, alias: &str) -> CResult<BlockUsage> {
    conn.query_row(
        &format!(
            "SELECT block_size, used_blocks, free_blocks FROM pragma_database_size() \
             WHERE database_name = '{alias}'"
        ),
        [],
        |r| {
            Ok(BlockUsage {
                block_size: r.get::<_, i64>(0)?.max(0) as u64,
                used: r.get::<_, i64>(1)?.max(0) as u64,
                free: r.get::<_, i64>(2)?.max(0) as u64,
            })
        },
    )
    .map_err(engine)
}

/// Columns of `alias.main.table`, name → type, sorted by name.
fn columns(conn: &Connection, alias: &str, table: &str) -> CResult<Vec<(String, String)>> {
    query_pairs(
        conn,
        &format!(
            "SELECT column_name, data_type FROM duckdb_columns() \
             WHERE database_name = '{alias}' AND schema_name = 'main' AND table_name = '{}' \
             ORDER BY column_name",
            table.replace('\'', "''")
        ),
    )
}

/// What the inventory decided.
struct Inventory {
    /// Tables to copy, sorted, each with its columns (the source's, sorted).
    tables: Vec<(String, Vec<String>)>,
    null_filled: Vec<String>,
    ignored: Vec<String>,
}

/// Protocol point 4: everything the source holds must be something the copy
/// (made by this binary's `init_schema`) knows, by name — compared with both
/// adjoined, every catalog query filtered by database (M-A).
fn inventory(h: &Connection) -> CResult<Inventory> {
    let unknown = |what: String| {
        CompactError::new(
            FailKind::Unknown,
            format!(
                "the index holds {what}, which this version of devctx does not know; nothing was \
                 changed (a newer version wrote it, or it was added by hand)"
            ),
        )
    };
    let schemas = query_strings(
        h,
        "SELECT schema_name FROM duckdb_schemas() WHERE database_name = 'src' AND NOT internal \
         ORDER BY 1",
    )?;
    for s in &schemas {
        if s != "main" && s != FTS_SCHEMA {
            return Err(unknown(format!("the schema `{s}`")));
        }
    }
    let dst_tables = query_strings(
        h,
        "SELECT table_name FROM duckdb_tables() WHERE database_name = 'dst' AND schema_name = 'main'",
    )?;
    let src_tables = query_pairs(
        h,
        "SELECT schema_name, table_name FROM duckdb_tables() WHERE database_name = 'src' \
         ORDER BY schema_name, table_name",
    )?;
    let mut tables = Vec::new();
    let mut null_filled = Vec::new();
    let mut ignored = Vec::new();
    for (schema, table) in &src_tables {
        if schema == FTS_SCHEMA {
            continue;
        }
        if schema != "main" {
            return Err(unknown(format!("the table `{schema}.{table}`")));
        }
        if table.starts_with("devctx_repair_") {
            let rows = query_u64(h, &format!("SELECT count(*) FROM src.main.{}", q(table)))?;
            if rows > 0 {
                return Err(CompactError::new(
                    FailKind::RepairLeftover,
                    format!(
                        "the index holds `{table}` with {rows} rows, left by a `devctx repair` that \
                         was cut short: it may be the only copy of `{}`. Do NOT run `devctx repair` \
                         again (it starts by dropping that table). Copy its rows back by hand \
                         (`INSERT INTO {} SELECT * FROM {table}`) with the DuckDB CLI, then drop it",
                        table.trim_start_matches("devctx_repair_"),
                        table.trim_start_matches("devctx_repair_"),
                    ),
                ));
            }
            ignored.push(table.clone());
            continue;
        }
        if !dst_tables.contains(table) {
            return Err(unknown(format!("the table `{table}`")));
        }
        let src_cols = columns(h, "src", table)?;
        let dst_cols = columns(h, "dst", table)?;
        for (c, ty) in &src_cols {
            match dst_cols.iter().find(|(d, _)| d == c) {
                None => return Err(unknown(format!("the column `{table}.{c}`"))),
                Some((_, dty)) if dty != ty => {
                    return Err(unknown(format!(
                        "the column `{table}.{c}` as {ty} (this version makes it {dty})"
                    )))
                }
                Some(_) => {}
            }
        }
        for (c, _) in &dst_cols {
            if !src_cols.iter().any(|(s, _)| s == c) {
                null_filled.push(format!("{table}.{c}"));
            }
        }
        tables.push((
            table.clone(),
            src_cols.into_iter().map(|(c, _)| c).collect(),
        ));
    }
    let known = |kind: &str, sql_src: &str, sql_dst: &str| -> CResult<()> {
        let have = query_strings(h, sql_dst)?;
        for name in query_strings(h, sql_src)? {
            if !have.contains(&name) {
                return Err(unknown(format!("the {kind} `{name}`")));
            }
        }
        Ok(())
    };
    known(
        "view",
        "SELECT view_name FROM duckdb_views() WHERE database_name = 'src' AND NOT internal \
         AND schema_name <> 'fts_main_vectors'",
        "SELECT view_name FROM duckdb_views() WHERE database_name = 'dst' AND NOT internal",
    )?;
    known(
        "sequence",
        "SELECT sequence_name FROM duckdb_sequences() WHERE database_name = 'src' \
         AND schema_name <> 'fts_main_vectors'",
        "SELECT sequence_name FROM duckdb_sequences() WHERE database_name = 'dst'",
    )?;
    known(
        "macro",
        "SELECT function_name FROM duckdb_functions() WHERE database_name = 'src' AND NOT internal \
         AND function_type IN ('macro', 'table_macro') AND schema_name <> 'fts_main_vectors'",
        "SELECT function_name FROM duckdb_functions() WHERE database_name = 'dst' AND NOT internal \
         AND function_type IN ('macro', 'table_macro')",
    )?;
    let dst_idx = query_strings(
        h,
        "SELECT index_name FROM duckdb_indexes() WHERE database_name = 'dst'",
    )?;
    for name in query_strings(
        h,
        "SELECT index_name FROM duckdb_indexes() WHERE database_name = 'src' \
         AND schema_name <> 'fts_main_vectors'",
    )? {
        if !dst_idx.contains(&name)
            && !name.starts_with("idx_vectors_hnsw")
            && !DROPPED_BY_OPEN.contains(&name.as_str())
        {
            return Err(unknown(format!("the index `{name}`")));
        }
    }
    tables.sort();
    Ok(Inventory {
        tables,
        null_filled,
        ignored,
    })
}

/// The `WHERE` that leaves out of a comparison the one row a compaction
/// changes on purpose, and the `PENDING_*` notes it settles.
fn meta_filter(table: &str, cleared: &[&str]) -> String {
    if table != "index_meta" {
        return String::new();
    }
    let mut keys = vec![format!("'{COMPACTION_META_KEY}'")];
    keys.extend(cleared.iter().map(|k| format!("'{k}'")));
    format!(
        " WHERE NOT (repo_path = '' AND branch = '' AND key IN ({}))",
        keys.join(", ")
    )
}

/// `count(*)` and `sum(hash(<every column, by name>)::HUGEINT)` of a table:
/// a sum and not `bit_xor`, so two identical rows of a table without a key
/// do not cancel out. As text, since a HUGEINT does not fit an `i64`.
pub(crate) fn checksum(
    conn: &Connection,
    alias: &str,
    table: &str,
    cols: &[String],
    filter: &str,
) -> CResult<(u64, String)> {
    let list = cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
    conn.query_row(
        &format!(
            "SELECT count(*), coalesce(sum(hash({list})::HUGEINT), 0)::VARCHAR \
             FROM {alias}.main.{}{filter}",
            q(table)
        ),
        [],
        |r| Ok((r.get::<_, i64>(0)?.max(0) as u64, r.get::<_, String>(1)?)),
    )
    .map_err(engine)
}

/// Rows of `a.t` that `b.t` lacks (with multiplicity), over `cols`.
pub(crate) fn except_all(
    conn: &Connection,
    a: &str,
    b: &str,
    table: &str,
    cols: &[String],
) -> CResult<u64> {
    let list = cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
    query_u64(
        conn,
        &format!(
            "SELECT count(*) FROM (SELECT {list} FROM {a}.main.{t} EXCEPT ALL \
             SELECT {list} FROM {b}.main.{t})",
            t = q(table)
        ),
    )
}

fn fsync_file(p: &Path) -> CResult<()> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(p)
        .and_then(|f| f.sync_all())
        .map_err(|e| CompactError::new(FailKind::Engine, format!("fsync {}: {e}", p.display())))
}

/// fsync of a directory, so a `rename` in it survives a power cut. Unix only:
/// Windows cannot open a directory this way, and there the replacement is
/// made with `MOVEFILE_WRITE_THROUGH` instead (see [`replace_file`]).
fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Replace `to` with `from` atomically.
#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// Windows: `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING |
/// MOVEFILE_WRITE_THROUGH`, which returns only once the move is on disk —
/// there is no directory to fsync. Not verified on a Windows machine
/// (PLAN-009 TASK-019).
#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
    let (a, b) = (wide(from), wide(to));
    // SAFETY: two NUL-terminated wide strings that outlive the call.
    let ok = unsafe {
        MoveFileExW(
            a.as_ptr(),
            b.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// What to do with a `<tmp>` found next to the database before a run (or an
/// open) starts (review D1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TmpVerdict {
    /// It holds nothing the database lacks: a run that died before its
    /// `rename`, or a reverted `rename` nothing wrote to. It may go.
    Disposable,
    /// A finished copy holding memories, references or sessions the database
    /// lacks: a reverted `rename` something wrote to. Touch neither.
    Keep(String),
    /// It could not be compared. It is not deleted.
    Unsure(String),
}

/// Compare `<tmp>` with `<db>` on what no reindex can rebuild. Reads both
/// through read-only `ATTACH`es of an in-memory connection.
pub(crate) fn tmp_verdict(paths: &CompactPaths) -> TmpVerdict {
    if !paths.tmp.exists() {
        return TmpVerdict::Disposable;
    }
    let Ok(c) = Connection::open_in_memory() else {
        return TmpVerdict::Unsure("no in-memory database".into());
    };
    let _ = load_extensions(&c);
    let attach = |path: &Path, alias: &str| -> std::result::Result<(), String> {
        let l = lit(path).map_err(|e| e.message)?;
        c.execute_batch(&format!("ATTACH {l} AS {alias} (READ_ONLY);"))
            .map_err(|e| e.to_string())
    };
    if let Err(e) = attach(&paths.tmp, "t") {
        return TmpVerdict::Unsure(format!("the copy cannot be read: {e}"));
    }
    let note = query_strings(
        &c,
        &format!(
            "SELECT value FROM t.main.index_meta WHERE repo_path = '' AND branch = '' \
             AND key = '{COMPACTION_META_KEY}'"
        ),
    )
    .unwrap_or_default();
    if !note.iter().any(|n| n.starts_with("done:")) {
        // Cut before its note was written: a partial copy, nothing more.
        return TmpVerdict::Disposable;
    }
    if let Err(e) = attach(&paths.db, "d") {
        return TmpVerdict::Unsure(format!("the index cannot be read to compare: {e}"));
    }
    let tables = |alias: &str| {
        query_strings(
            &c,
            &format!(
                "SELECT table_name FROM duckdb_tables() WHERE database_name = '{alias}' \
                 AND schema_name = 'main'"
            ),
        )
        .unwrap_or_default()
    };
    let (in_t, in_d) = (tables("t"), tables("d"));
    for table in IRREPLACEABLE {
        if !in_t.iter().any(|t| t == table) {
            continue;
        }
        let extra = if in_d.iter().any(|t| t == table) {
            let d_cols: Vec<String> = columns(&c, "d", table)
                .unwrap_or_default()
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            let cols: Vec<String> = columns(&c, "t", table)
                .unwrap_or_default()
                .into_iter()
                .map(|(n, _)| n)
                .filter(|n| d_cols.contains(n))
                .collect();
            except_all(&c, "t", "d", table, &cols)
        } else {
            query_u64(&c, &format!("SELECT count(*) FROM t.main.{}", q(table)))
        };
        match extra {
            Ok(0) => {}
            Ok(n) => return TmpVerdict::Keep(format!("{n} rows of `{table}` the index lacks")),
            Err(e) => return TmpVerdict::Unsure(format!("comparing `{table}`: {e}")),
        }
    }
    TmpVerdict::Disposable
}

/// Milliseconds since the epoch, for names that must not collide.
pub(crate) fn epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Compact the database at `db`: the protocol of PLAN-009 TASK-018. Nothing
/// may hold it (stop its server first); it fails with [`FailKind::Busy`]
/// otherwise. On any failure the database is the one that was there.
pub fn compact(db: &Path, opts: &CompactOptions<'_>) -> CResult<CompactReport> {
    let paths = CompactPaths::for_db(db);
    if !paths.db.exists() {
        return Err(CompactError::new(
            FailKind::Preflight,
            format!("there is no index at {}", paths.db.display()),
        ));
    }
    // I1: the lock, before touching anything.
    let lock = match FileLock::try_acquire(&paths.lock, LockMode::Exclusive) {
        Ok(Some(l)) => l,
        Ok(None) => {
            return Err(CompactError::new(
                FailKind::Busy,
                "the index is in use (a server, the TUI, another command) or already being \
                 compacted; stop what holds it (`devctx serve --stop`) and retry",
            ))
        }
        Err(e) => {
            return Err(CompactError::new(
                FailKind::Preflight,
                format!("cannot create {}: {e}", paths.lock.display()),
            ))
        }
    };
    // M-B.1: a second attempt on another descriptor must fail, or this
    // filesystem does not honour locks and nothing here would exclude anyone.
    let second_fails = match opts.lock_probe {
        Some(p) => p(&paths.lock),
        None => matches!(
            FileLock::try_acquire(&paths.lock, LockMode::Exclusive),
            Ok(None)
        ),
    };
    if !second_fails {
        return Err(CompactError::new(
            FailKind::LockUnsupported,
            format!(
                "the filesystem of {} does not support file locks (a second lock on {} was \
                 granted); compacting there could not keep other processes out, so it was not \
                 started",
                paths.db.display(),
                paths.lock.display()
            ),
        ));
    }
    // A `<tmp>` left from before is deleted only when it holds nothing the
    // database lacks (review D1): after a power cut it can be the file that
    // was really in use.
    match tmp_verdict(&paths) {
        TmpVerdict::Disposable => {}
        TmpVerdict::Keep(why) | TmpVerdict::Unsure(why) => {
            return Err(CompactError::new(
                FailKind::Unreconciled,
                crate::StoreError::UnreconciledCompaction {
                    db: paths.db.clone(),
                    tmp: paths.tmp.clone(),
                    reason: why,
                }
                .to_string(),
            ))
        }
    }
    remove_temporaries(&paths);
    let out = run(&paths, opts);
    match &out {
        // The copy is in place but its name may not be durable: no writer may
        // open it before the process ends, so the lock is not released.
        Err(e) if e.kind == FailKind::Durability => {
            std::mem::forget(lock);
            return out;
        }
        Err(_) => remove_temporaries(&paths),
        Ok(_) => {}
    }
    drop(lock);
    out
}

/// Remove what a run leaves, never while the database is missing (I5).
fn remove_temporaries(paths: &CompactPaths) {
    if !paths.db.exists() {
        return;
    }
    for f in [
        &paths.tmp,
        &paths.tmp_wal,
        &paths.side_tmp,
        &paths.side_tmp_wal,
    ] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all(&paths.tmp_dir);
}

struct Clock {
    last: Instant,
    phases: Vec<(String, f64)>,
}

impl Clock {
    fn lap(&mut self, name: &str) {
        let now = Instant::now();
        self.phases
            .push((name.to_string(), (now - self.last).as_secs_f64()));
        self.last = now;
    }
}

fn step(opts: &CompactOptions<'_>, phase: Phase, h: &Connection) -> CResult<()> {
    if opts.cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
        return Err(CompactError::new(
            FailKind::Cancelled,
            "the compaction was cancelled; the index is as it was",
        ));
    }
    if let Some(hook) = opts.hook {
        hook(phase, h).map_err(|m| CompactError::new(FailKind::Engine, m))?;
    }
    if opts.cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
        return Err(CompactError::new(
            FailKind::Cancelled,
            "the compaction was cancelled; the index is as it was",
        ));
    }
    Ok(())
}

fn run(paths: &CompactPaths, opts: &CompactOptions<'_>) -> CResult<CompactReport> {
    let mut clock = Clock {
        last: Instant::now(),
        phases: Vec::new(),
    };
    // Point 2, preparation: fold a WAL with a normal open, before the source
    // is adjoined — only when asked to (`devctx compact`).
    if opts.fold_wal && std::fs::metadata(&paths.wal).is_ok_and(|m| m.len() > 0) {
        let s = Store::open_unlocked(&paths.db, opts.dim_hint).map_err(engine)?;
        s.try_checkpoint().map_err(engine)?;
        drop(s);
        clock.lap("fold_wal");
    }
    assert_no_wal(paths)?;

    // Point 3: `H` holds the source. Limits and extensions first: adjoining a
    // database that has an HNSW index without VSS loaded is the fatal error
    // of PLAN-009 TASK-003.
    let h = Connection::open_in_memory().map_err(engine)?;
    let _ = h.execute_batch(&crate::store::resource_limits_sql());
    let _ = h.execute_batch(&format!("SET temp_directory = {};", lit(&paths.tmp_dir)?));
    let (vss, fts_ext) = load_extensions(&h);
    h.execute_batch(&format!("ATTACH {} AS src (READ_ONLY);", lit(&paths.db)?))
        .map_err(engine)?;
    assert_no_wal(paths)?;
    let stamp = Stamp::of(&paths.db)?;
    let before = usage(&h, "src")?;
    step(opts, Phase::Attached, &h)?;

    // What the source has, read through `H`.
    let dim = query_strings(
        &h,
        "SELECT data_type FROM duckdb_columns() WHERE database_name = 'src' \
         AND schema_name = 'main' AND table_name = 'vectors' AND column_name = 'vector'",
    )?
    .first()
    .and_then(|t| {
        let open = t.find('[')?;
        let close = t[open..].find(']')?;
        t[open + 1..open + close].trim().parse::<usize>().ok()
    })
    .ok_or_else(|| {
        CompactError::new(
            FailKind::Unknown,
            "the index has no `vectors.vector` column of a known width",
        )
    })?;
    let metric = query_strings(
        &h,
        "SELECT index_name FROM duckdb_indexes() WHERE database_name = 'src' \
         AND table_name = 'vectors' AND index_name LIKE 'idx_vectors_hnsw%'",
    )?
    .first()
    .map(|n| match n.rsplit_once("hnsw_") {
        Some((_, m)) => m.to_string(),
        None => "cosine".to_string(),
    });
    let has_fts = query_u64(
        &h,
        &format!(
            "SELECT count(*) FROM duckdb_schemas() WHERE database_name = 'src' \
             AND schema_name = '{FTS_SCHEMA}'"
        ),
    )? > 0;
    let pending = |key: &str| -> CResult<Option<String>> {
        Ok(query_strings(
            &h,
            &format!(
                "SELECT value FROM src.main.index_meta WHERE repo_path = '' AND branch = '' \
                 AND key = '{key}'"
            ),
        )
        .unwrap_or_default()
        .into_iter()
        .next())
    };
    let pending_hnsw = pending(PENDING_HNSW_KEY)?;
    let pending_fts = pending(PENDING_FTS_KEY)?.is_some();
    let hnsw = metric.clone().or(pending_hnsw.clone());
    let fts = has_fts || pending_fts;
    if hnsw.is_some() && !vss {
        return Err(CompactError::new(
            FailKind::Preflight,
            "the index has (or owes) an HNSW index and the DuckDB VSS extension cannot be loaded \
             here; nothing was changed",
        ));
    }
    if fts && !fts_ext {
        return Err(CompactError::new(
            FailKind::Preflight,
            "the index has (or owes) a BM25 index and the DuckDB FTS extension cannot be loaded \
             here; nothing was changed",
        ));
    }
    let mut cleared: Vec<&str> = Vec::new();
    if hnsw.is_some() && pending_hnsw.is_some() {
        cleared.push(PENDING_HNSW_KEY);
    }
    if fts && pending_fts {
        cleared.push(PENDING_FTS_KEY);
    }

    // Point 4 (M-A): the copy gets this binary's schema from a store of its
    // own, closed before it is adjoined — the schema helpers do not filter by
    // database, and with the source adjoined they would see its tables.
    drop(Store::open_unlocked(&paths.tmp, dim).map_err(engine)?);
    h.execute_batch(&format!("ATTACH {} AS dst;", lit(&paths.tmp)?))
        .map_err(engine)?;
    let inv = inventory(&h)?;
    clock.lap("inventory");
    step(opts, Phase::Inventoried, &h)?;

    // Point 5: copy, table by table, each in its own transaction.
    let mut rows = Vec::new();
    for (i, (table, cols)) in inv.tables.iter().enumerate() {
        let list = cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
        h.execute_batch(&format!(
            "BEGIN TRANSACTION;
             INSERT INTO dst.main.{t} BY NAME SELECT {list} FROM src.main.{t};
             COMMIT;",
            t = q(table)
        ))
        .map_err(|e| {
            let _ = h.execute_batch("ROLLBACK;");
            engine(e)
        })?;
        rows.push((
            table.clone(),
            query_u64(&h, &format!("SELECT count(*) FROM dst.main.{}", q(table)))?,
        ));
        step(opts, Phase::CopiedTable(i + 1), &h)?;
    }
    let mut keys = vec![format!("'{COMPACTION_META_KEY}'")];
    keys.extend(cleared.iter().map(|k| format!("'{k}'")));
    let note = format!(
        "done:{}:{}:{}",
        opts.version,
        epoch(),
        opts.reason.replace('\'', "")
    );
    h.execute_batch(&format!(
        "DELETE FROM dst.main.index_meta WHERE repo_path = '' AND branch = '' AND key IN ({});
         INSERT INTO dst.main.index_meta VALUES ('', '', '{COMPACTION_META_KEY}', '{note}');",
        keys.join(", ")
    ))
    .map_err(engine)?;
    step(opts, Phase::Copied, &h)?;
    h.execute_batch("CHECKPOINT dst; DETACH dst;")
        .map_err(engine)?;
    clock.lap("copy");

    // Point 6: the derived indexes, through another connection.
    {
        let b = Store::open_unlocked(&paths.tmp, dim).map_err(engine)?;
        if let Some(m) = &hnsw {
            if !b.enable_hnsw(m).map_err(engine)? {
                return Err(CompactError::new(
                    FailKind::Engine,
                    "the HNSW index could not be rebuilt on the copy",
                ));
            }
            clock.lap("hnsw");
        }
        if fts {
            if !b.rebuild_fts().map_err(engine)? {
                return Err(CompactError::new(
                    FailKind::Engine,
                    "the BM25 index could not be rebuilt on the copy",
                ));
            }
            clock.lap("fts");
        }
        b.try_checkpoint().map_err(engine)?;
    }
    step(opts, Phase::IndexesBuilt, &h)?;

    // Point 7: the copy, reopened cold, answers.
    cold_verify(&paths.tmp, dim, hnsw.as_deref(), fts)?;
    if paths.tmp_wal.exists() {
        return Err(CompactError::new(
            FailKind::Engine,
            "the copy kept a write-ahead log after it was closed",
        ));
    }
    clock.lap("cold_verify");
    step(opts, Phase::ColdVerified, &h)?;

    // Point 8: against the source, through `H`, which never let it go.
    h.execute_batch(&format!("ATTACH {} AS chk (READ_ONLY);", lit(&paths.tmp)?))
        .map_err(engine)?;
    let verified = verify(&h, &inv, &cleared);
    let after = usage(&h, "chk");
    h.execute_batch("DETACH chk;").map_err(engine)?;
    verified?;
    let after = after?;
    clock.lap("verify");
    step(opts, Phase::Verified, &h)?;

    // Point 9: the memories sidecar.
    let kept_sidecar = write_sidecar(&h, paths)?;
    clock.lap("sidecar");
    step(opts, Phase::SidecarWritten, &h)?;

    // Point 10: in place.
    let mode = std::fs::metadata(&paths.db).map_err(engine)?.permissions();
    std::fs::set_permissions(&paths.tmp, mode).map_err(engine)?;
    fsync_file(&paths.tmp)?;
    let bytes_after = std::fs::metadata(&paths.tmp).map_err(engine)?.len();
    step(opts, Phase::BeforeRename, &h)?;
    assert_no_wal(paths)?;
    if Stamp::of(&paths.db)? != stamp {
        return Err(CompactError::new(
            FailKind::SourceChanged,
            format!(
                "{} changed while it was being compacted (size, time or inode); the copy was \
                 dropped and the index left as it is",
                paths.db.display()
            ),
        ));
    }
    if paths.tmp_wal.exists() {
        return Err(CompactError::new(
            FailKind::Engine,
            "the copy has a write-ahead log next to it; not putting it in place",
        ));
    }
    let original = if opts.keep_original {
        if paths.original.exists() {
            return Err(CompactError::new(
                FailKind::Preflight,
                format!(
                    "{} already exists; move it away to keep another original",
                    paths.original.display()
                ),
            ));
        }
        std::fs::hard_link(&paths.db, &paths.original).map_err(|e| {
            CompactError::new(
                FailKind::Engine,
                format!(
                    "could not keep the original at {}: {e}",
                    paths.original.display()
                ),
            )
        })?;
        Some(paths.original.clone())
    } else {
        None
    };
    // Windows cannot rename over an open file: there the source is released
    // right before (not safe by construction; I1 covers every 0.10 binary).
    #[cfg(windows)]
    h.execute_batch("DETACH src;").map_err(engine)?;
    replace_file(&paths.tmp, &paths.db).map_err(|e| {
        CompactError::new(
            FailKind::Rename,
            format!("could not put the copy in place: {e}"),
        )
    })?;
    // From here on the copy is the database: nothing below deletes anything.
    let _ = step(opts, Phase::Renamed, &h);
    if let Some(dir) = paths.db.parent() {
        let sync = opts.dir_sync.unwrap_or(fsync_dir);
        let mut last = Ok(());
        for _ in 0..3 {
            last = sync(dir);
            if last.is_ok() {
                break;
            }
        }
        if let Err(e) = last {
            return Err(CompactError::new(
                FailKind::Durability,
                format!(
                    "the compacted index is in place at {}, but its directory could not be \
                     synced to disk ({e}): a power cut now could bring the old file back. \
                     This process keeps the index locked until it ends; run `sync` (or wait \
                     a few seconds) before using it",
                    paths.db.display()
                ),
            ));
        }
    }
    #[cfg(not(windows))]
    let _ = h.execute_batch("DETACH src;");
    drop(h);
    let _ = std::fs::remove_dir_all(&paths.tmp_dir);
    clock.lap("replace");
    Ok(CompactReport {
        db: paths.db.clone(),
        before,
        after,
        bytes_before: stamp.len,
        bytes_after,
        phases: clock.phases,
        rows,
        hnsw: hnsw.map(|m| normalize_metric(&m).to_string()),
        fts,
        null_filled: inv.null_filled,
        ignored: inv.ignored,
        sidecar: paths.side.clone(),
        kept_sidecar,
        original,
    })
}

/// Point 7: reopen the copy and ask its indexes.
fn cold_verify(tmp: &Path, dim: usize, hnsw: Option<&str>, fts: bool) -> CResult<()> {
    let bad = |m: String| CompactError::new(FailKind::Verify, m);
    let v = Store::open_unlocked(tmp, dim).map_err(engine)?;
    let probe: Option<(String, String)> = v
        .conn
        .query_row(
            "SELECT id, coalesce(text, '') FROM vectors ORDER BY id LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();
    if let Some(m) = hnsw {
        let m = normalize_metric(m);
        if v.hnsw_metric().as_deref() != Some(m) {
            return Err(bad(format!(
                "the copy's HNSW index is {:?}, not {m}",
                v.hnsw_metric()
            )));
        }
        if let Some((id, _)) = &probe {
            let vector = v
                .vector_by_id(id)
                .map_err(engine)?
                .ok_or_else(|| bad("a vector of the copy cannot be read back".into()))?;
            let func = if m == "ip" {
                "array_negative_inner_product"
            } else {
                "array_cosine_distance"
            };
            let lit = vector
                .iter()
                .map(|x| format!("{x:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            let plan: Vec<String> = {
                let mut st = v
                    .conn
                    .prepare(&format!(
                        "EXPLAIN SELECT id FROM vectors ORDER BY {func}(vector, [{lit}]::FLOAT[{dim}]) \
                         LIMIT 10"
                    ))
                    .map_err(engine)?;
                let rows = st
                    .query_map([], |r| r.get::<_, String>(1))
                    .map_err(engine)?;
                rows.filter_map(std::result::Result::ok).collect()
            };
            if !plan.iter().any(|p| p.contains("HNSW_INDEX_SCAN")) {
                return Err(bad(
                    "a vector search on the copy does not use its HNSW index".into(),
                ));
            }
            let hits = v
                .search(&vector, &devctx_core::types::SearchFilter::default(), 5)
                .map_err(engine)?;
            if hits.is_empty() {
                return Err(bad("a vector search on the copy found nothing".into()));
            }
        }
    }
    if fts {
        if !v.has_fts() {
            return Err(bad("the copy's BM25 index cannot be queried".into()));
        }
        if let Some((_, text)) = &probe {
            let word = text
                .split(|c: char| !c.is_alphanumeric())
                .find(|w| w.chars().count() >= 3);
            if let Some(w) = word {
                let hits = v
                    .keyword_search(w, &devctx_core::types::SearchFilter::default(), 5)
                    .map_err(engine)?;
                if hits.is_empty() {
                    return Err(bad(format!(
                        "a keyword search for `{w}` on the copy found nothing"
                    )));
                }
            }
        }
    }
    v.try_checkpoint().map_err(engine)?;
    Ok(())
}

/// Point 8: every table, by count and checksum; the irreplaceable ones also
/// by `EXCEPT ALL` both ways.
fn verify(h: &Connection, inv: &Inventory, cleared: &[&str]) -> CResult<()> {
    for (table, cols) in &inv.tables {
        let filter = meta_filter(table, cleared);
        let a = checksum(h, "src", table, cols, &filter)?;
        let b = checksum(h, "chk", table, cols, &filter)?;
        if a.0 != b.0 {
            return Err(CompactError::new(
                FailKind::Verify,
                format!(
                    "the copy of `{table}` has {} rows and the index {}; the copy was dropped",
                    b.0, a.0
                ),
            ));
        }
        if a.1 != b.1 {
            return Err(CompactError::new(
                FailKind::Verify,
                format!("the copy of `{table}` differs from the index; the copy was dropped"),
            ));
        }
        if IRREPLACEABLE.contains(&table.as_str()) {
            let missing = except_all(h, "src", "chk", table, cols)?;
            let extra = except_all(h, "chk", "src", table, cols)?;
            if missing + extra > 0 {
                return Err(CompactError::new(
                    FailKind::Verify,
                    format!(
                        "the copy of `{table}` lacks {missing} rows of the index and has {extra} \
                         it does not; the copy was dropped"
                    ),
                ));
            }
        }
    }
    let note = query_u64(
        h,
        &format!(
            "SELECT count(*) FROM chk.main.index_meta WHERE repo_path = '' AND branch = '' \
             AND key = '{COMPACTION_META_KEY}' AND value LIKE 'done:%'"
        ),
    )?;
    if note != 1 {
        return Err(CompactError::new(
            FailKind::Verify,
            "the copy lacks its compaction note",
        ));
    }
    Ok(())
}

/// Point 9 (M-D): `memories`, `memory_symbol_references` and `sessions` from
/// the source into `<db>.pre-compact.duckdb`, without vectors. An older
/// sidecar holding a memory the source no longer has is kept under another
/// name rather than replaced.
fn write_sidecar(h: &Connection, paths: &CompactPaths) -> CResult<Option<PathBuf>> {
    let present = query_strings(
        h,
        "SELECT table_name FROM duckdb_tables() WHERE database_name = 'src' \
         AND schema_name = 'main'",
    )?;
    h.execute_batch(&format!("ATTACH {} AS side;", lit(&paths.side_tmp)?))
        .map_err(engine)?;
    for t in IRREPLACEABLE {
        if present.iter().any(|p| p == t) {
            h.execute_batch(&format!(
                "CREATE TABLE side.main.{t} AS SELECT * FROM src.main.{t};",
                t = q(t)
            ))
            .map_err(engine)?;
        }
    }
    h.execute_batch("CHECKPOINT side; DETACH side;")
        .map_err(engine)?;
    if paths.side_tmp_wal.exists() {
        return Err(CompactError::new(
            FailKind::Engine,
            "the memories sidecar kept a write-ahead log",
        ));
    }
    fsync_file(&paths.side_tmp)?;
    let mut kept = None;
    if paths.side.exists() {
        let lost = h
            .execute_batch(&format!(
                "ATTACH {} AS oldside (READ_ONLY);",
                lit(&paths.side)?
            ))
            .map_err(engine)
            .and_then(|()| {
                let n = if query_u64(
                    h,
                    "SELECT count(*) FROM duckdb_tables() WHERE database_name = 'oldside' \
                     AND table_name = 'memories'",
                )? > 0
                {
                    query_u64(
                        h,
                        "SELECT count(*) FROM oldside.main.memories WHERE id NOT IN \
                         (SELECT id FROM src.main.memories)",
                    )?
                } else {
                    0
                };
                let _ = h.execute_batch("DETACH oldside;");
                Ok(n)
            })
            // An old sidecar that cannot be read is kept, not replaced.
            .unwrap_or(1);
        if lost > 0 {
            let mut name = paths.db.clone().into_os_string();
            name.push(format!(".pre-compact.{}.duckdb", epoch()));
            let to = PathBuf::from(name);
            std::fs::rename(&paths.side, &to).map_err(engine)?;
            kept = Some(to);
        }
    }
    std::fs::rename(&paths.side_tmp, &paths.side).map_err(engine)?;
    if let Some(dir) = paths.side.parent() {
        fsync_dir(dir).map_err(engine)?;
    }
    Ok(kept)
}

/// What a database is, read without writing anything (`devctx compact
/// --dry-run`, and the numbers before a compaction when no server runs).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Inspection {
    /// Blocks.
    pub usage: BlockUsage,
    /// Bytes of the file.
    pub bytes: u64,
    /// Metric of its HNSW index, if any.
    pub hnsw: Option<String>,
    /// The HNSW index's estimated size (vectors x width x 4).
    pub hnsw_bytes: Option<u64>,
    /// Whether it has a BM25 index.
    pub fts: bool,
    /// Its compaction note.
    pub note: Option<String>,
}

/// Read `db` through a read-only `ATTACH` from an in-memory connection:
/// creates no file next to it and writes nothing. Fails while another
/// process holds it for writing (ask its server instead).
pub fn inspect(db: &Path) -> CResult<Inspection> {
    let paths = CompactPaths::for_db(db);
    let bytes = std::fs::metadata(&paths.db)
        .map_err(|_| {
            CompactError::new(
                FailKind::Preflight,
                format!("there is no index at {}", paths.db.display()),
            )
        })?
        .len();
    let c = Connection::open_in_memory().map_err(engine)?;
    let _ = load_extensions(&c);
    c.execute_batch(&format!("ATTACH {} AS src (READ_ONLY);", lit(&paths.db)?))
        .map_err(engine)?;
    let usage = usage(&c, "src")?;
    let hnsw = query_strings(
        &c,
        "SELECT index_name FROM duckdb_indexes() WHERE database_name = 'src' \
         AND table_name = 'vectors' AND index_name LIKE 'idx_vectors_hnsw%'",
    )?
    .first()
    .map(|n| match n.rsplit_once("hnsw_") {
        Some((_, m)) => m.to_string(),
        None => "cosine".to_string(),
    });
    let width = query_strings(
        &c,
        "SELECT data_type FROM duckdb_columns() WHERE database_name = 'src' \
         AND table_name = 'vectors' AND column_name = 'vector'",
    )?
    .first()
    .and_then(|t| {
        let open = t.find('[')?;
        let close = t[open..].find(']')?;
        t[open + 1..open + close].trim().parse::<u64>().ok()
    })
    .unwrap_or(0);
    let rows = query_u64(&c, "SELECT count(*) FROM src.main.vectors").unwrap_or(0);
    let fts = query_u64(
        &c,
        &format!(
            "SELECT count(*) FROM duckdb_schemas() WHERE database_name = 'src' \
             AND schema_name = '{FTS_SCHEMA}'"
        ),
    )? > 0;
    let note = query_strings(
        &c,
        &format!(
            "SELECT value FROM src.main.index_meta WHERE repo_path = '' AND branch = '' \
             AND key = '{COMPACTION_META_KEY}'"
        ),
    )
    .unwrap_or_default()
    .into_iter()
    .next();
    Ok(Inspection {
        usage,
        bytes,
        hnsw_bytes: hnsw.as_ref().map(|_| rows * width * 4),
        hnsw,
        fts,
        note,
    })
}

/// What [`restore_from_sidecar`] put back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Memories added (absent from the index).
    pub added: usize,
    /// Memories already there, left alone.
    pub already: usize,
    /// `memory_symbol_references` rows added, for the added memories.
    pub refs: usize,
    /// `sessions` rows added.
    pub sessions: usize,
}

/// Put back, from a compaction's sidecar, the memories the index lacks — with
/// their symbol references, and the sessions it lacks. Only ever adds. The
/// sidecar has no vectors: `reindex` is called for every memory added, to
/// embed it again.
pub fn restore_from_sidecar(
    store: &Store,
    side: &Path,
    reindex: &mut dyn FnMut(&crate::Memory) -> crate::Result<()>,
) -> crate::Result<RestoreReport> {
    let side_lit = lit(side).map_err(|e| crate::StoreError::Decode(e.message))?;
    store
        .w()?
        .execute_batch(&format!("ATTACH {side_lit} AS devctx_restore (READ_ONLY);"))?;
    let result = (|| -> crate::Result<RestoreReport> {
        let has = |t: &str| -> crate::Result<bool> {
            Ok(store.conn.query_row(
                "SELECT count(*) > 0 FROM duckdb_tables() WHERE database_name = 'devctx_restore' \
                 AND table_name = ?",
                [t],
                |r| r.get::<_, bool>(0),
            )?)
        };
        let mut report = RestoreReport::default();
        if has("memories")? {
            let added: Vec<String> = {
                let mut st = store.conn.prepare(
                    "SELECT id FROM devctx_restore.main.memories WHERE id NOT IN \
                     (SELECT id FROM main.memories) ORDER BY id",
                )?;
                let rows = st.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            report.already = store.conn.query_row(
                "SELECT count(*) FROM devctx_restore.main.memories WHERE id IN \
                 (SELECT id FROM main.memories)",
                [],
                |r| r.get::<_, i64>(0),
            )? as usize;
            let refs = has("memory_symbol_references")?;
            store.in_transaction(|| {
                // The references first: "added" is read off what `memories`
                // lacks, which the next statement changes.
                if refs {
                    report.refs = store.w()?.execute(
                        "INSERT INTO main.memory_symbol_references BY NAME \
                         SELECT * FROM devctx_restore.main.memory_symbol_references \
                         WHERE memory_id IN (SELECT id FROM devctx_restore.main.memories \
                                             WHERE id NOT IN (SELECT id FROM main.memories))",
                        [],
                    )?;
                }
                store.w()?.execute_batch(
                    "INSERT INTO main.memories BY NAME SELECT * FROM devctx_restore.main.memories \
                     WHERE id NOT IN (SELECT id FROM main.memories);",
                )?;
                Ok(())
            })?;
            report.added = added.len();
            for id in &added {
                if let Some(m) = store.get_memory(id)? {
                    if m.deleted_at.is_none() {
                        reindex(&m)?;
                    }
                }
            }
        }
        if has("sessions")? {
            report.sessions = store.w()?.execute(
                "INSERT INTO main.sessions BY NAME SELECT * FROM devctx_restore.main.sessions \
                 WHERE id NOT IN (SELECT id FROM main.sessions)",
                [],
            )?;
        }
        Ok(report)
    })();
    let _ = store.w()?.execute_batch("DETACH devctx_restore;");
    store.checkpoint();
    result
}

#[cfg(test)]
#[path = "compact_tests.rs"]
mod tests;

/// Facts about the machine, behind [`SystemProbe`].
mod sys {
    use std::path::Path;

    #[cfg(unix)]
    pub fn available_disk(dir: &Path) -> Option<u64> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: `statvfs` fills the struct we hand it; the path is a valid
        // C string for the duration of the call.
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        // `f_bavail`: what an unprivileged user may use, not the free total.
        Some((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
    }

    #[cfg(not(unix))]
    pub fn available_disk(_dir: &Path) -> Option<u64> {
        None
    }

    #[cfg(target_os = "linux")]
    pub fn available_memory() -> Option<u64> {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = text.lines().find(|l| l.starts_with("MemAvailable:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn available_memory() -> Option<u64> {
        None
    }

    #[cfg(unix)]
    pub fn effective_uid() -> Option<u32> {
        // SAFETY: `geteuid` cannot fail.
        Some(unsafe { libc::geteuid() })
    }

    #[cfg(not(unix))]
    pub fn effective_uid() -> Option<u32> {
        None
    }

    #[cfg(unix)]
    pub fn owner(m: &std::fs::Metadata) -> Option<u32> {
        use std::os::unix::fs::MetadataExt;
        Some(m.uid())
    }

    #[cfg(not(unix))]
    pub fn owner(_m: &std::fs::Metadata) -> Option<u32> {
        None
    }

    #[cfg(unix)]
    pub fn inode(m: &std::fs::Metadata) -> Option<u64> {
        use std::os::unix::fs::MetadataExt;
        Some(m.ino())
    }

    #[cfg(not(unix))]
    pub fn inode(_m: &std::fs::Metadata) -> Option<u64> {
        None
    }

    #[cfg(unix)]
    pub fn dir_writable(dir: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `access` reads the path for the duration of the call.
        unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
    }

    #[cfg(not(unix))]
    pub fn dir_writable(dir: &Path) -> bool {
        std::fs::metadata(dir).is_ok_and(|m| !m.permissions().readonly())
    }
}
