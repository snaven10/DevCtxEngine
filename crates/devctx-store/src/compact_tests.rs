//! Tests of the compaction (PLAN-009 TASK-018, "Tests"). The numbers in the
//! doc comments are the spec's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use devctx_core::types::{SearchFilter, VectorMetadata, VectorPoint};
use duckdb::Connection;

use super::*;
use crate::oslock::{try_lock_exclusive, CompactPaths};
use crate::{Memory, Store};

const DIM: usize = 8;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("devctx_compact_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A deterministic unit vector.
fn unit(seed: usize) -> Vec<f32> {
    let mut x = (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let v: Vec<f32> = (0..DIM)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % 1000) as f32 / 1000.0 + 0.001
        })
        .collect();
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.into_iter().map(|x| x / n).collect()
}

fn code_point(i: usize) -> VectorPoint {
    VectorPoint {
        id: format!("code_{i:05}"),
        vector: unit(i),
        text: format!(
            "fn handler_{i}() connects the postgres database pool number {i} {}",
            "and validates the request payload ".repeat(20)
        ),
        metadata: VectorMetadata {
            repo: "demo".into(),
            branch: "main".into(),
            file: format!("src/f{}.rs", i % 40),
            symbol: format!("handler_{i}"),
            symbol_type: "function".into(),
            language: "rust".into(),
            start_line: 1,
            end_line: 9,
            chunk_level: "function".into(),
            content_hash: format!("h{i}"),
            indexed_at: "2026-10-13T00:00:00Z".into(),
            ..Default::default()
        },
    }
}

fn memory(id: &str, content: &str) -> Memory {
    Memory {
        id: id.into(),
        title: format!("title of {id}"),
        content: content.into(),
        memory_type: "decision".into(),
        scope: "local".into(),
        project: "demo".into(),
        topic_key: String::new(),
        tags: "a,b".into(),
        author: "t".into(),
        repo: "demo".into(),
        branch: "main".into(),
        files: "src/f1.rs".into(),
        revision_count: 1,
        duplicate_count: 0,
        normalized_hash: format!("nh_{id}"),
        vector_id: id.into(),
        session_id: "s1".into(),
        created_at: "1".into(),
        updated_at: "2".into(),
        deleted_at: None,
    }
}

fn memory_point(id: &str, seed: usize) -> VectorPoint {
    VectorPoint {
        id: id.into(),
        vector: unit(10_000 + seed),
        text: format!("memory {id} about the token interceptor"),
        metadata: VectorMetadata {
            repo: "demo".into(),
            branch: "main".into(),
            symbol: format!("title of {id}"),
            symbol_type: "decision".into(),
            language: "memory".into(),
            chunk_level: "memory".into(),
            memory_type: "decision".into(),
            memory_scope: "local".into(),
            memory_tags: "a,b".into(),
            ..Default::default()
        },
    }
}

#[derive(Clone, Copy)]
struct Fx {
    code: usize,
    hnsw: Option<&'static str>,
    fts: bool,
    bloat: bool,
}

impl Default for Fx {
    fn default() -> Self {
        Self {
            code: 300,
            hnsw: Some("cosine"),
            fts: true,
            bloat: false,
        }
    }
}

/// A database shaped like an upgraded index: code and memory vectors, the
/// graph tables, memories with their references (two identical rows: the
/// table has no key), a session, index notes.
fn fixture(db: &Path, fx: Fx) {
    let store = Store::open(db, DIM).unwrap();
    let points: Vec<VectorPoint> = (0..fx.code).map(code_point).collect();
    store.upsert(&points).unwrap();
    if fx.bloat {
        // What a reusing `--full` does: every row deleted and written again,
        // with other tables growing after it, so the freed blocks sit in the
        // middle of the file where a checkpoint cannot truncate them.
        for round in 0..3 {
            store.checkpoint();
            store
                .conn
                .execute_batch(&format!(
                    "INSERT INTO graph_edges (source, target, kind, source_file, line, repo, branch)
                     SELECT 'filler_{round}_' || i, 't', 'calls', 'f', 1, 'demo', 'main'
                     FROM range(20000) r(i);"
                ))
                .unwrap();
            store.checkpoint();
            store.upsert(&points).unwrap();
            store.checkpoint();
        }
    }
    store
        .upsert(&[
            memory_point("mem_a", 1),
            memory_point("mem_a_c1", 2),
            memory_point("mem_b", 3),
        ])
        .unwrap();
    store
        .upsert_memory(&memory("mem_a", "the token interceptor retries once"))
        .unwrap();
    store
        .upsert_memory(&memory("mem_b", "routes are registered at boot"))
        .unwrap();
    store
        .conn
        .execute_batch(
            "INSERT INTO memory_symbol_references VALUES
               ('mem_a', 'Interceptor.retry', 'src/f1.rs', 3, 'demo', 'main', 'files-field'),
               ('mem_a', 'Interceptor.retry', 'src/f1.rs', 3, 'demo', 'main', 'files-field'),
               ('mem_b', 'boot', NULL, NULL, 'demo', 'main', 'content-mention');
             INSERT INTO sessions VALUES ('s1', 'demo', '/repo', '1', '2', 'summary');
             INSERT INTO symbols (repo, branch, id, file, kind, name, start_line) VALUES
               ('demo', 'main', 1, 'src/f1.rs', 'function', 'handler_1', 1),
               ('demo', 'main', 1, 'src/f1.rs', 'function', 'handler_1', 1);
             INSERT INTO edges (repo, branch, kind, src_id, dst_name, file, line) VALUES
               ('demo', 'main', 'calls', 1, 'retry', 'src/f1.rs', 4),
               ('demo', 'main', 'calls', 1, 'retry', 'src/f1.rs', 4);
             INSERT INTO graph_edges (source, target, kind, source_file, line, repo, branch)
               VALUES ('handler_1', 'retry', 'calls', 'src/f1.rs', 4, 'demo', 'main');
             INSERT INTO routes VALUES ('axum', 'GET', '/x', 'H', 'get', 'H.get', 'src/f1.rs',
               2, 'demo', 'main', '1');
             INSERT INTO index_state VALUES ('/repo', 'main', 'abc', 'm', 8, 40, 300, 303, '1');
             INSERT INTO file_state VALUES ('/repo', 'main', 'src/f1.rs', 'h', 'rust', 1, 1);",
        )
        .unwrap();
    store
        .set_index_meta("/repo", "main", "extractor", "v21-0000")
        .unwrap();
    store
        .set_index_meta("/repo", "main", "embedding_fingerprint", "fp")
        .unwrap();
    store
        .set_index_meta("/repo", "main", "excludes", "ex")
        .unwrap();
    store
        .set_index_meta("", "", COMPACTION_META_KEY, COMPACTION_OWED_UPGRADE)
        .unwrap();
    if let Some(m) = fx.hnsw {
        assert!(
            store.enable_hnsw(m).unwrap(),
            "VSS is needed by these tests"
        );
    }
    if fx.fts {
        assert!(store.rebuild_fts().unwrap(), "FTS is needed by these tests");
    }
    store.checkpoint();
}

/// Every table's row count and checksum, read from a fresh in-memory
/// connection (never from the process holding the database open).
fn snapshot(db: &Path) -> BTreeMap<String, (u64, String)> {
    let c = Connection::open_in_memory().unwrap();
    let _ = load_extensions(&c);
    c.execute_batch(&format!("ATTACH {} AS s (READ_ONLY);", lit(db).unwrap()))
        .unwrap();
    let tables = query_strings(
        &c,
        "SELECT table_name FROM duckdb_tables() WHERE database_name = 's' \
         AND schema_name = 'main' ORDER BY 1",
    )
    .unwrap();
    let mut out = BTreeMap::new();
    for t in tables {
        let cols: Vec<String> = columns(&c, "s", &t)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        out.insert(
            t.clone(),
            checksum(&c, "s", &t, &cols, &meta_filter(&t, &[])).unwrap(),
        );
    }
    out
}

/// sha256 of a file, computed by another process (M-E.2).
fn sha(p: &Path) -> String {
    let out = Command::new("sha256sum").arg(p).output().unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn usage_of(db: &Path) -> BlockUsage {
    let c = Connection::open_in_memory().unwrap();
    let _ = load_extensions(&c);
    c.execute_batch(&format!("ATTACH {} AS s (READ_ONLY);", lit(db).unwrap()))
        .unwrap();
    usage(&c, "s").unwrap()
}

fn opts<'a>() -> CompactOptions<'a> {
    let mut o = CompactOptions::new("manual");
    o.version = "0.10.0-test".into();
    o.dim_hint = DIM;
    o
}

/// The temporaries of a run are gone, and the lock is free.
fn assert_clean(db: &Path) {
    let p = CompactPaths::for_db(db);
    for f in [&p.tmp, &p.tmp_wal, &p.side_tmp, &p.side_tmp_wal, &p.tmp_dir] {
        assert!(!f.exists(), "{} was left behind", f.display());
    }
    assert!(p.lock.exists(), "the lock file is never deleted (M-C)");
    // A child forked by another test of this process holds a copy of a
    // just-closed descriptor until it execs: give the lock that moment.
    let t0 = Instant::now();
    while try_lock_exclusive(db).unwrap().is_none() {
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "the lock was not released"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn note_of(db: &Path) -> Option<String> {
    let s = Store::open(db, DIM).unwrap();
    s.get_index_meta("", "", COMPACTION_META_KEY).unwrap()
}

// --- the happy path ----------------------------------------------------------

/// 13, 14, 15, 17, 19 and 32 together: a compaction of a bloated index keeps
/// every row, rebuilds both indexes with the same metric, gives the space
/// back, writes the sidecar, leaves the note and keeps the lock file.
#[test]
fn a_compaction_keeps_every_row_and_gives_the_space_back() {
    let dir = scratch("happy");
    let db = dir.join("index.duckdb");
    fixture(
        &db,
        Fx {
            code: 1500,
            bloat: true,
            ..Fx::default()
        },
    );
    let before = snapshot(&db);
    let bloat_before = Store::open(&db, DIM).unwrap().bloat_ratio().unwrap();
    let kw_before = {
        let s = Store::open(&db, DIM).unwrap();
        let mut ids: Vec<String> = s
            .keyword_search("postgres", &SearchFilter::default(), 10)
            .unwrap()
            .into_iter()
            .map(|h| h.point.id)
            .collect();
        ids.sort();
        ids
    };

    let r = compact(&db, &opts()).expect("compacts");
    assert_eq!(r.hnsw.as_deref(), Some("cosine"));
    assert!(r.fts);
    assert!(
        r.after.used + r.after.free < r.before.used + r.before.free,
        "{r:?}"
    );
    assert!(r.bytes_after < r.bytes_before, "{r:?}");
    assert!(r.null_filled.is_empty(), "{r:?}");

    // 19: the data, and every note but the compaction one, identical.
    let after = snapshot(&db);
    assert_eq!(before, after);
    let note = note_of(&db).unwrap();
    assert!(note.starts_with("done:0.10.0-test:"), "{note}");
    assert!(note.ends_with(":manual"), "{note}");

    // 13, 14, 15.
    let s = Store::open(&db, DIM).unwrap();
    assert_eq!(s.hnsw_metric().as_deref(), Some("cosine"));
    let bloat_after = s.bloat_ratio().unwrap();
    assert!(
        bloat_after < bloat_before,
        "{bloat_after} >= {bloat_before}: {r:?}"
    );
    assert!(s.has_fts());
    let mut kw_after: Vec<String> = s
        .keyword_search("postgres", &SearchFilter::default(), 10)
        .unwrap()
        .into_iter()
        .map(|h| h.point.id)
        .collect();
    kw_after.sort();
    assert_eq!(kw_before, kw_after);
    // A semantic recall of a known memory finds it.
    let hit = s
        .search(&unit(10_001), &SearchFilter::default(), 1)
        .unwrap();
    assert_eq!(hit[0].point.id, "mem_a");
    assert_eq!(
        s.get_memory("mem_a").unwrap().unwrap().content,
        "the token interceptor retries once"
    );
    drop(s);

    // 17: the sidecar, with the three tables and no vectors.
    let side = snapshot(&r.sidecar);
    assert_eq!(
        side.keys().cloned().collect::<Vec<_>>(),
        vec!["memories", "memory_symbol_references", "sessions"]
    );
    assert_eq!(side["memories"], before["memories"]);
    assert_eq!(
        side["memory_symbol_references"],
        before["memory_symbol_references"]
    );
    assert_eq!(side["sessions"], before["sessions"]);
    assert_clean(&db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 4 (spike): the copy uses this binary's schema and lands within the
/// spike's 1.03-1.10x of a database written once (here: the same content
/// written once, without the rewrites).
#[test]
fn the_copy_is_about_the_size_of_the_same_content_written_once() {
    let dir = scratch("size");
    let once = dir.join("once.duckdb");
    fixture(
        &once,
        Fx {
            code: 1500,
            ..Fx::default()
        },
    );
    {
        // The rows the bloating rounds add, written once.
        let s = Store::open(&once, DIM).unwrap();
        for round in 0..3 {
            s.conn
                .execute_batch(&format!(
                    "INSERT INTO graph_edges (source, target, kind, source_file, line, repo, branch)
                     SELECT 'filler_{round}_' || i, 't', 'calls', 'f', 1, 'demo', 'main'
                     FROM range(20000) r(i);"
                ))
                .unwrap();
        }
        s.checkpoint();
    }
    let db = dir.join("index.duckdb");
    fixture(
        &db,
        Fx {
            code: 1500,
            bloat: true,
            ..Fx::default()
        },
    );
    let base = usage_of(&once);
    compact(&db, &opts()).unwrap();
    let got = usage_of(&db);
    assert!(
        (got.used as f64) <= 1.10 * base.used as f64 + 2.0,
        "copy {got:?} against written once {base:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 13 with `ip`, 35, and `--keep-original`: the metric survives, and the
/// exact top-k of the copy is the original's, query by query.
#[test]
fn ip_survives_and_the_exact_top_k_is_identical() {
    let dir = scratch("ip");
    let db = dir.join("index.duckdb");
    fixture(
        &db,
        Fx {
            hnsw: Some("ip"),
            ..Fx::default()
        },
    );
    let mut o = opts();
    o.keep_original = true;
    let r = compact(&db, &o).unwrap();
    assert_eq!(r.hnsw.as_deref(), Some("ip"));
    let original = r.original.clone().expect("kept");
    assert_eq!(original, CompactPaths::for_db(&db).original);
    assert_eq!(
        Store::open(&db, DIM).unwrap().hnsw_metric().as_deref(),
        Some("ip")
    );

    let c = Connection::open_in_memory().unwrap();
    let _ = load_extensions(&c);
    c.execute_batch(&format!(
        "ATTACH {} AS a (READ_ONLY); ATTACH {} AS b (READ_ONLY);",
        lit(&original).unwrap(),
        lit(&db).unwrap()
    ))
    .unwrap();
    for seed in [3usize, 77, 150, 10_002] {
        let v = unit(seed)
            .iter()
            .map(|x| format!("{x:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        // `+ 0.0` keeps the optimizer off the HNSW index: exact, by scan.
        let top = |alias: &str| {
            query_strings(
                &c,
                &format!(
                    "SELECT id FROM {alias}.main.vectors ORDER BY \
                     array_cosine_distance(vector, [{v}]::FLOAT[{DIM}]) + 0.0, id LIMIT 10"
                ),
            )
            .unwrap()
        };
        assert_eq!(top("a"), top("b"), "seed {seed}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 16: a database holding only memories compacts and keeps them.
#[test]
fn a_database_of_memories_only_compacts() {
    let dir = scratch("memonly");
    let db = dir.join("index.duckdb");
    fixture(
        &db,
        Fx {
            code: 0,
            hnsw: None,
            fts: false,
            bloat: false,
        },
    );
    let before = snapshot(&db);
    let r = compact(&db, &opts()).unwrap();
    assert_eq!(r.hnsw, None);
    assert!(!r.fts);
    assert_eq!(before, snapshot(&db));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 18: without a `compaction` note and with both indexes owed (an
/// interrupted run): it builds what is owed and the `PENDING_*` notes do not
/// reach the copy.
#[test]
fn owed_indexes_are_built_and_their_notes_cleared() {
    let dir = scratch("pending");
    let db = dir.join("index.duckdb");
    fixture(
        &db,
        Fx {
            hnsw: None,
            fts: false,
            ..Fx::default()
        },
    );
    {
        let s = Store::open(&db, DIM).unwrap();
        s.delete_index_meta("", "", COMPACTION_META_KEY).unwrap();
        s.set_index_meta("", "", PENDING_HNSW_KEY, "cosine")
            .unwrap();
        s.set_index_meta("", "", PENDING_FTS_KEY, "1").unwrap();
    }
    let r = compact(&db, &opts()).unwrap();
    assert_eq!(r.hnsw.as_deref(), Some("cosine"));
    assert!(r.fts);
    let s = Store::open(&db, DIM).unwrap();
    assert_eq!(s.hnsw_metric().as_deref(), Some("cosine"));
    assert!(s.has_fts());
    assert_eq!(s.get_index_meta("", "", PENDING_HNSW_KEY).unwrap(), None);
    assert_eq!(s.get_index_meta("", "", PENDING_FTS_KEY).unwrap(), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 10: through a symlink the replacement happens next to the real file and
/// the link still points there; the mode is kept.
#[cfg(unix)]
#[test]
fn a_symlinked_database_is_replaced_in_place_and_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("symlink");
    std::fs::create_dir_all(dir.join("real")).unwrap();
    let real = dir.join("real/index.duckdb");
    fixture(&real, Fx::default());
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
    let link = dir.join("index.duckdb");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let before = snapshot(&real);
    compact(&link, &opts()).unwrap();
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::canonicalize(&link).unwrap(),
        std::fs::canonicalize(&real).unwrap()
    );
    assert_eq!(before, snapshot(&real));
    assert_eq!(
        std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(!dir.join("index.duckdb.compact.tmp").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the sidecar (M-D) -------------------------------------------------------

/// 17: the next compaction replaces a sidecar whose memories are all still
/// in the index, keeps under another name one that holds a memory the index
/// no longer has, and the memories can be restored from it.
#[test]
fn the_sidecar_is_replaced_kept_or_restored_from() {
    let dir = scratch("sidecar");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let first = compact(&db, &opts()).unwrap();
    assert!(first.kept_sidecar.is_none());
    let side_before = snapshot(&first.sidecar);

    // Nothing lost since: replaced.
    let second = compact(&db, &opts()).unwrap();
    assert!(second.kept_sidecar.is_none());
    assert_eq!(side_before, snapshot(&second.sidecar));

    // A memory forgotten since: the old sidecar is the only place it is in.
    Store::open(&db, DIM)
        .unwrap()
        .forget_memory("mem_b")
        .unwrap();
    let third = compact(&db, &opts()).unwrap();
    let kept = third.kept_sidecar.clone().expect("kept under another name");
    assert!(kept.exists());
    assert!(kept
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("index.duckdb.pre-compact."));

    // Restoration: mem_b comes back, with its references, re-embedded.
    let s = Store::open(&db, DIM).unwrap();
    assert!(s.get_memory("mem_b").unwrap().is_none());
    let mut embedded = Vec::new();
    let r = restore_from_sidecar(&s, &kept, &mut |m| {
        embedded.push(m.id.clone());
        s.upsert(&[memory_point(&m.id, 3)])
    })
    .unwrap();
    assert_eq!(r.added, 1);
    assert_eq!(r.already, 1);
    assert_eq!(r.refs, 1);
    assert_eq!(embedded, vec!["mem_b"]);
    assert_eq!(
        s.get_memory("mem_b").unwrap().unwrap().content,
        "routes are registered at boot"
    );
    assert!(s.vector_by_id("mem_b").unwrap().is_some());
    // Only ever adds: a second restore adds nothing.
    let again = restore_from_sidecar(&s, &kept, &mut |_| panic!("nothing to embed")).unwrap();
    assert_eq!(again.added, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

// --- the copy is checked (6, 7, 11, 12, 30) -----------------------------------

fn failing_run(db: &Path, hook: Hook<'_>) -> CompactError {
    let mut o = opts();
    o.hook = Some(hook);
    compact(db, &o).expect_err("must fail")
}

/// 6 and 33: a row deleted, a duplicate added and a value changed in the copy
/// are each caught; the original is byte for byte what it was, and the note
/// to leave is `owed:failed-verify`.
#[test]
fn verification_catches_a_missing_a_duplicated_and_a_changed_row() {
    let dir = scratch("verify");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let sum = sha(&db);
    for (what, sql) in [
        (
            "vectors",
            "DELETE FROM dst.main.vectors WHERE id = 'code_00007'",
        ),
        (
            "edges",
            "INSERT INTO dst.main.edges SELECT * FROM dst.main.edges LIMIT 1",
        ),
        (
            "memories",
            "UPDATE dst.main.memories SET content = 'changed' WHERE id = 'mem_a'",
        ),
        (
            "memory_symbol_references",
            "DELETE FROM dst.main.memory_symbol_references WHERE memory_id = 'mem_b'",
        ),
    ] {
        let e = failing_run(&db, &|p, h| {
            if p == Phase::Copied {
                h.execute_batch(sql).map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        assert_eq!(e.kind, FailKind::Verify, "{what}: {e}");
        assert!(e.message.contains(what), "{what}: {e}");
        assert_eq!(e.note(), Some(COMPACTION_OWED_FAILED_VERIFY));
        assert_eq!(sha(&db), sum, "{what}: the original changed");
        assert_clean(&db);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 7: the checksum sees two identical rows of a keyless table (a `bit_xor`
/// would not), tells NULL from `''`, and does not depend on the physical
/// order of the columns.
#[test]
fn the_checksum_sees_duplicates_and_nulls_and_ignores_column_order() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "ATTACH ':memory:' AS a; ATTACH ':memory:' AS b;
         CREATE TABLE a.main.t (x VARCHAR, y INTEGER, v FLOAT[3]);
         CREATE TABLE b.main.t (v FLOAT[3], y INTEGER, x VARCHAR);
         INSERT INTO a.main.t VALUES ('p', 1, [1, 2, 3]), (NULL, 2, NULL);
         INSERT INTO b.main.t VALUES ([1, 2, 3], 1, 'p'), (NULL, 2, NULL);",
    )
    .unwrap();
    let cols = vec!["v".to_string(), "x".to_string(), "y".to_string()];
    assert_eq!(
        checksum(&c, "a", "t", &cols, "").unwrap(),
        checksum(&c, "b", "t", &cols, "").unwrap(),
        "same rows, columns in another physical order"
    );
    c.execute_batch("UPDATE b.main.t SET x = '' WHERE y = 2;")
        .unwrap();
    assert_ne!(
        checksum(&c, "a", "t", &cols, "").unwrap(),
        checksum(&c, "b", "t", &cols, "").unwrap(),
        "NULL and '' must differ"
    );
    c.execute_batch(
        "UPDATE b.main.t SET x = NULL WHERE y = 2;
         INSERT INTO a.main.t VALUES ('p', 1, [1, 2, 3]), ('p', 1, [1, 2, 3]);
         INSERT INTO b.main.t VALUES ([9, 9, 9], 9, 'q'), ([9, 9, 9], 9, 'q');",
    )
    .unwrap();
    // Two pairs of duplicates: xor cancels both and calls the tables equal.
    let xor = |alias: &str| {
        c.query_row(
            &format!("SELECT bit_xor(hash(v, x, y))::VARCHAR FROM {alias}.main.t"),
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
    };
    assert_eq!(xor("a"), xor("b"), "the trap: xor is blind to it");
    assert_ne!(
        checksum(&c, "a", "t", &cols, "").unwrap(),
        checksum(&c, "b", "t", &cols, "").unwrap()
    );
}

/// 11: a `devctx_repair_*` table with rows aborts, naming it and saying not
/// to run `repair` again; an empty one is left out.
#[test]
fn a_repair_leftover_with_rows_aborts_and_an_empty_one_is_ignored() {
    let dir = scratch("repair");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    {
        let s = Store::open(&db, DIM).unwrap();
        s.conn
            .execute_batch("CREATE TABLE devctx_repair_memories AS SELECT * FROM memories;")
            .unwrap();
        s.checkpoint();
    }
    let sum = sha(&db);
    let e = compact(&db, &opts()).unwrap_err();
    assert_eq!(e.kind, FailKind::RepairLeftover, "{e}");
    assert!(e.message.contains("devctx_repair_memories"), "{e}");
    assert!(e.message.contains("NOT run `devctx repair`"), "{e}");
    assert_eq!(sha(&db), sum);
    assert_clean(&db);
    {
        let s = Store::open(&db, DIM).unwrap();
        s.conn
            .execute_batch("DELETE FROM devctx_repair_memories;")
            .unwrap();
        s.checkpoint();
    }
    let r = compact(&db, &opts()).unwrap();
    assert_eq!(r.ignored, vec!["devctx_repair_memories"]);
    assert!(!snapshot(&db).contains_key("devctx_repair_memories"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 12 and 30: a table, a column, a view and a macro the binary does not know
/// each abort by name — the inventory runs with both databases adjoined, and
/// the copy still gets `live_edges` (M-A). A column the source lacks arrives
/// NULL and is declared.
#[test]
fn unknown_objects_abort_by_name_and_a_missing_column_is_null_filled() {
    for (what, sql) in [
        ("`extra_table`", "CREATE TABLE extra_table (a INTEGER);"),
        (
            "`symbols.extra`",
            "ALTER TABLE symbols ADD COLUMN extra VARCHAR;",
        ),
        ("`extra_view`", "CREATE VIEW extra_view AS SELECT 1 AS one;"),
        ("`extra_macro`", "CREATE MACRO extra_macro(a) AS a + 1;"),
        ("`extra_seq`", "CREATE SEQUENCE extra_seq;"),
        ("`extra_schema`", "CREATE SCHEMA extra_schema;"),
    ] {
        let dir = scratch("unknown");
        let db = dir.join("index.duckdb");
        fixture(&db, Fx::default());
        {
            let s = Store::open(&db, DIM).unwrap();
            s.conn.execute_batch(sql).unwrap();
            s.checkpoint();
        }
        let sum = sha(&db);
        let e = compact(&db, &opts()).unwrap_err();
        assert_eq!(e.kind, FailKind::Unknown, "{what}: {e}");
        assert!(e.message.contains(what), "{what}: {e}");
        assert_eq!(sha(&db), sum);
        assert_clean(&db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    let dir = scratch("nullfill");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    {
        let s = Store::open(&db, DIM).unwrap();
        s.conn
            .execute_batch(
                "UPDATE symbols SET content_hash = 'x';
                 ALTER TABLE symbols DROP COLUMN content_hash;",
            )
            .unwrap();
        s.checkpoint();
    }
    // `Store::open` above put the column back (`init_schema`): read the
    // source the way the compaction does, through its own catalog.
    let c = Connection::open_in_memory().unwrap();
    let _ = load_extensions(&c);
    c.execute_batch(&format!("ATTACH {} AS s (READ_ONLY);", lit(&db).unwrap()))
        .unwrap();
    let lacks = !columns(&c, "s", "symbols")
        .unwrap()
        .iter()
        .any(|(n, _)| n == "content_hash");
    drop(c);
    if lacks {
        let r = compact(&db, &opts()).unwrap();
        assert_eq!(r.null_filled, vec!["symbols.content_hash"]);
        let s = Store::open(&db, DIM).unwrap();
        let n: i64 = s
            .conn
            .query_row(
                "SELECT count(*) FROM symbols WHERE content_hash IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
    }
    let r = compact(&db, &opts()).unwrap();
    assert!(r.null_filled.is_empty());
    let s = Store::open(&db, DIM).unwrap();
    let views: i64 = s
        .conn
        .query_row(
            "SELECT count(*) FROM duckdb_views() WHERE view_name = 'live_edges'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(views, 1, "the copy has `live_edges`");
    let _ = std::fs::remove_dir_all(&dir);
}

// --- preflight (9, 34) --------------------------------------------------------

/// 9 and 34: not enough disk, a read-only directory, another owner and not
/// enough memory each refuse before anything is created.
#[test]
fn each_preflight_refusal_creates_nothing() {
    let dir = scratch("preflight");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let ok = SystemProbe::default();
    let p = preflight(&db, Some(1000), &ok).expect("this machine passes");
    assert!(p.db_bytes > 0);
    let cases: [(&str, SystemProbe); 4] = [
        (
            "not enough disk",
            SystemProbe {
                available_disk: |_| Some(10),
                ..ok
            },
        ),
        (
            "cannot be written",
            SystemProbe {
                dir_writable: |_| false,
                ..ok
            },
        ),
        (
            "another user",
            SystemProbe {
                effective_uid: || Some(u32::MAX - 7),
                ..ok
            },
        ),
        (
            "not enough memory",
            SystemProbe {
                available_memory: || Some(1 << 20),
                ..ok
            },
        ),
    ];
    for (what, probe) in cases {
        if what == "another user" && !cfg!(unix) {
            continue;
        }
        let e = preflight(&db, None, &probe).unwrap_err();
        assert_eq!(e.kind, FailKind::Preflight, "{what}");
        assert!(e.message.contains(what), "{what}: {e}");
    }
    assert!(!CompactPaths::for_db(&db).tmp.exists());
    assert_eq!(parse_size("2GB"), Some(2_000_000_000));
    assert_eq!(parse_size("1.5GiB"), Some(1_610_612_736));
    let _ = std::fs::remove_dir_all(&dir);
}

// --- WAL (3) -----------------------------------------------------------------

/// 3: a WAL with content before starting: the automatic run refuses and
/// leaves the database and its WAL byte for byte; `devctx compact` folds it
/// and compacts, the WAL's rows included.
#[test]
fn a_wal_before_starting_refuses_or_is_folded() {
    let dir = scratch("walbefore");
    let db = dir.join("index.duckdb");
    fixture(
        &db,
        Fx {
            hnsw: None,
            ..Fx::default()
        },
    );
    child(&db, "leave-wal");
    let wal = CompactPaths::for_db(&db).wal;
    assert!(
        std::fs::metadata(&wal).unwrap().len() > 0,
        "the child left a WAL"
    );
    let (sum, wal_sum) = (sha(&db), sha(&wal));
    let e = compact(&db, &opts()).unwrap_err();
    assert_eq!(e.kind, FailKind::WalPresent, "{e}");
    assert_eq!((sha(&db), sha(&wal)), (sum, wal_sum));
    assert_clean(&db);

    let mut o = opts();
    o.fold_wal = true;
    compact(&db, &o).expect("folds and compacts");
    assert!(std::fs::metadata(&wal)
        .map(|m| m.len() == 0)
        .unwrap_or(true));
    let s = Store::open(&db, DIM).unwrap();
    assert!(s.vector_by_id("from_the_wal").unwrap().is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 3 (I3) and 31 (M-B.2): a WAL that appears between the verification and the
/// `rename`, and a source whose mtime changes under the run, abort; the
/// original and its WAL are left byte for byte.
#[test]
fn a_wal_or_a_touch_before_the_rename_aborts() {
    let dir = scratch("walrename");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let wal = CompactPaths::for_db(&db).wal;
    let sum = sha(&db);
    let e = failing_run(&db, &|p, _| {
        if p == Phase::BeforeRename {
            std::fs::write(&wal, b"not ours").map_err(|e| e.to_string())?;
        }
        Ok(())
    });
    assert_eq!(e.kind, FailKind::WalPresent, "{e}");
    assert_eq!(sha(&db), sum);
    assert_eq!(std::fs::read(&wal).unwrap(), b"not ours");
    assert_clean(&db);
    std::fs::remove_file(&wal).unwrap();

    // `touch` in another process: this one must not open the file.
    let e = failing_run(&db, &|p, _| {
        if p == Phase::BeforeRename {
            std::thread::sleep(Duration::from_millis(20));
            let ok = Command::new("touch")
                .arg("-m")
                .arg(&db)
                .status()
                .map_err(|e| e.to_string())?;
            assert!(ok.success());
        }
        Ok(())
    });
    assert_eq!(e.kind, FailKind::SourceChanged, "{e}");
    assert_eq!(sha(&db), sum);
    assert_clean(&db);
    let _ = std::fs::remove_dir_all(&dir);
}

// --- locks (5, 31, 32) ---------------------------------------------------------

/// 5 and 32: with the lock held (another compaction, or an open store) a
/// compaction creates nothing; a filesystem that grants a second lock aborts
/// (M-B.1).
#[test]
fn a_held_lock_or_a_lockless_filesystem_stops_the_run() {
    let dir = scratch("busy");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let sum = sha(&db);
    let held = try_lock_exclusive(&db).unwrap().unwrap();
    let e = compact(&db, &opts()).unwrap_err();
    assert_eq!(e.kind, FailKind::Busy);
    drop(held);
    let open = Store::open(&db, DIM).unwrap();
    let e = compact(&db, &opts()).unwrap_err();
    assert_eq!(
        e.kind,
        FailKind::Busy,
        "an open store keeps the lock shared"
    );
    drop(open);
    let mut o = opts();
    o.lock_probe = Some(|_| false);
    let e = compact(&db, &o).unwrap_err();
    assert_eq!(e.kind, FailKind::LockUnsupported, "{e}");
    assert!(e.message.contains("does not support file locks"), "{e}");
    assert_eq!(sha(&db), sum);
    assert_clean(&db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 5: a process holding the database without knowing the compaction lock (a
/// 0.9.0 binary) makes the run fail on DuckDB's lock, with nothing created.
#[test]
fn a_binary_that_ignores_the_lock_file_still_stops_the_run() {
    let dir = scratch("oldbin");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let mut holder = spawn_child(&db, "hold-raw");
    wait_for(&db, "ready");
    let e = compact(&db, &opts()).unwrap_err();
    std::fs::write(signal(&db, "go"), b"").unwrap();
    let _ = holder.wait();
    assert_eq!(e.kind, FailKind::Locked, "{e}");
    assert_clean(&db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 29 and M-E.1 (I2, I4): paused after its auxiliary connections closed —
/// at `Verified` and again right before the `rename` — the compacting process
/// has exactly one descriptor on the source (`H`'s), and another process that
/// skips the lock file cannot open it for writing: DuckDB's lock stops it.
#[test]
fn only_h_holds_the_source() {
    let dir = scratch("pause");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let sum = sha(&db);
    let mut compactor = spawn_child(&db, "pause");
    for at in ["verified", "rename"] {
        wait_for(&db, &format!("ready-{at}"));
        match Connection::open(&db) {
            Err(e) => assert!(e.to_string().contains("lock"), "{at}: {e}"),
            Ok(_) => panic!("{at}: a writer opened the source during the compaction"),
        }
        std::fs::write(signal(&db, &format!("go-{at}")), b"").unwrap();
    }
    let status = compactor.wait().unwrap();
    assert!(
        status.success(),
        "the compaction (or its descriptor check) failed"
    );
    assert_ne!(sha(&db), sum, "the copy is in place");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 29, the reason for the rule: one open and close of the source in the
/// compacting process drops DuckDB's lock, a writer gets in and changes the
/// file — and the run, which must catch that, aborts without replacing it:
/// the writer's row is in the index afterwards.
#[test]
fn one_stray_descriptor_breaks_the_exclusion_and_the_run_catches_it() {
    let dir = scratch("pause_fdclose");
    let db = dir.join("index.duckdb");
    // No HNSW index: the writer below opens without VSS.
    fixture(
        &db,
        Fx {
            hnsw: None,
            ..Fx::default()
        },
    );
    let mut compactor = spawn_child(&db, "pause-fdclose");
    wait_for(&db, "ready-verified");
    {
        let c = Connection::open(&db).expect("the stray close let the writer in");
        c.execute_batch("INSERT INTO index_meta VALUES ('stray', 'writer', 'k', 'v'); CHECKPOINT;")
            .unwrap();
    }
    std::fs::write(signal(&db, "go-verified"), b"").unwrap();
    let _ = compactor.wait().unwrap();
    let outcome = std::fs::read_to_string(signal(&db, "outcome")).unwrap();
    assert!(
        outcome.contains("SourceChanged") || outcome.contains("WalPresent"),
        "{outcome}"
    );
    let s = Store::open(&db, DIM).unwrap();
    assert_eq!(
        s.get_index_meta("stray", "writer", "k").unwrap().as_deref(),
        Some("v"),
        "the writer's row was lost"
    );
    assert_eq!(
        s.get_index_meta("", "", COMPACTION_META_KEY)
            .unwrap()
            .as_deref(),
        Some(COMPACTION_OWED_UPGRADE)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Descriptors of this process on the file at `p` (Linux: `/proc/self/fd`).
#[cfg(target_os = "linux")]
fn descriptors_on(p: &Path) -> usize {
    use std::os::unix::fs::MetadataExt;
    let want = std::fs::metadata(p).unwrap();
    std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .flatten()
        .filter(|e| {
            std::fs::metadata(e.path())
                .is_ok_and(|m| m.ino() == want.ino() && m.dev() == want.dev())
        })
        .count()
}

// --- cuts and Ctrl-C (1, 2) ---------------------------------------------------

/// 2: cancelled at each phase, the run removes its temporaries, releases the
/// lock and leaves the original byte for byte, its note included.
#[test]
fn a_cancel_at_any_phase_leaves_the_original() {
    let dir = scratch("cancel");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let sum = sha(&db);
    for at in [
        Phase::Attached,
        Phase::Inventoried,
        Phase::CopiedTable(2),
        Phase::Copied,
        Phase::IndexesBuilt,
        Phase::ColdVerified,
        Phase::Verified,
        Phase::SidecarWritten,
        Phase::BeforeRename,
    ] {
        let cancel = AtomicBool::new(false);
        let mut o = opts();
        o.cancel = Some(&cancel);
        let hook = |p: Phase, _: &Connection| {
            if p == at {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        };
        o.hook = Some(&hook);
        let e = compact(&db, &o).unwrap_err();
        assert_eq!(e.kind, FailKind::Cancelled, "{at:?}: {e}");
        assert_eq!(sha(&db), sum, "{at:?}");
        assert_clean(&db);
    }
    assert_eq!(note_of(&db).as_deref(), Some(COMPACTION_OWED_UPGRADE));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 1: a process killed in the middle (after the 2nd table, after the HNSW
/// index, after the verification, between the `rename` and the release of
/// the lock) leaves a whole database — the original, or after the `rename`
/// the verified copy — and the next open cleans up.
#[test]
fn a_kill_at_any_phase_leaves_a_whole_database() {
    for (mode, replaced) in [
        ("abort-copied2", false),
        ("abort-indexes", false),
        ("abort-verified", false),
        ("abort-renamed", true),
    ] {
        let dir = scratch(mode);
        let db = dir.join("index.duckdb");
        fixture(&db, Fx::default());
        let before = snapshot(&db);
        let sum = sha(&db);
        let status = spawn_child(&db, mode).wait().unwrap();
        assert!(!status.success(), "{mode}: the child must die");
        if replaced {
            assert_ne!(sha(&db), sum, "{mode}");
        } else {
            assert_eq!(sha(&db), sum, "{mode}");
        }
        // The next open cleans up and the data is whole.
        let s = Store::open(&db, DIM).unwrap();
        drop(s);
        assert_eq!(before, snapshot(&db), "{mode}");
        assert_clean(&db);
        let note = note_of(&db).unwrap();
        assert_eq!(note.starts_with("done:"), replaced, "{mode}: {note}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// 8 (I5) on the error path: the database disappears during the run; the
/// run fails and does not delete its temporary.
#[test]
fn a_failed_run_does_not_delete_its_copy_when_the_database_is_gone() {
    let dir = scratch("gone");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let aside = dir.join("aside.duckdb");
    let e = failing_run(&db, &|p, _| {
        if p == Phase::BeforeRename {
            std::fs::rename(&db, &aside).map_err(|e| e.to_string())?;
        }
        Ok(())
    });
    assert_eq!(e.kind, FailKind::SourceChanged, "{e}");
    assert!(
        dir.join("index.duckdb.compact.tmp").exists(),
        "the copy was deleted while the database was missing (I5)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review D2: an embedder that fails half way leaves the restored rows in
/// place; running the restore again embeds every one still without a vector,
/// instead of skipping them as "already present".
#[test]
fn a_restore_cut_by_the_embedder_is_finished_by_the_next_one() {
    let dir = scratch("restore_retry");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let r = compact(&db, &opts()).unwrap();
    let s = Store::open(&db, DIM).unwrap();
    for id in ["mem_a", "mem_b"] {
        s.forget_memory(id).unwrap();
        s.delete_memory_vectors(id).unwrap();
    }
    let mut calls = 0;
    let first = restore_from_sidecar(&s, &r.sidecar, &mut |m| {
        calls += 1;
        if calls == 2 {
            return Err(crate::StoreError::Decode("the embedder went away".into()));
        }
        s.upsert(&[memory_point(&m.id, 1)])
    });
    assert!(first.is_err());
    let again =
        restore_from_sidecar(&s, &r.sidecar, &mut |m| s.upsert(&[memory_point(&m.id, 1)])).unwrap();
    assert_eq!(again.added, 0, "the rows were already restored");
    assert_eq!(again.embedded, 1, "the one left without a vector");
    for id in ["mem_a", "mem_b"] {
        assert!(s.get_memory(id).unwrap().is_some(), "{id}");
        assert!(s.vector_by_id(id).unwrap().is_some(), "{id} has no vector");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review T7: a failed verification is recorded even when nobody can open
/// the index right after it: the run leaves a marker, and the next open
/// turns it into the note.
#[test]
fn a_failed_verification_reaches_the_note_through_the_next_open() {
    let dir = scratch("failed_marker");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let e = failing_run(&db, &|p, h| {
        if p == Phase::Copied {
            h.execute_batch("DELETE FROM dst.main.vectors WHERE id = 'code_00001'")
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    });
    assert_eq!(e.kind, FailKind::Verify);
    let marker = CompactPaths::for_db(&db).failed_marker;
    assert!(marker.exists(), "no marker left");
    assert_eq!(note_of(&db).as_deref(), Some(COMPACTION_OWED_FAILED_VERIFY));
    assert!(!marker.exists(), "the open did not settle the marker");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review: a WAL that appears right after the `ATTACH` is caught before the
/// `rename`; the original and that WAL are left byte for byte.
#[test]
fn a_wal_right_after_the_attach_aborts() {
    let dir = scratch("walattach");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let wal = CompactPaths::for_db(&db).wal;
    let sum = sha(&db);
    let e = failing_run(&db, &|p, _| {
        if p == Phase::Attached {
            std::fs::write(&wal, b"appeared").map_err(|e| e.to_string())?;
        }
        Ok(())
    });
    assert_eq!(e.kind, FailKind::WalPresent, "{e}");
    assert_eq!(sha(&db), sum);
    assert_eq!(std::fs::read(&wal).unwrap(), b"appeared");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review (NIT): a read-only database (0400) compacts and stays 0400.
#[cfg(unix)]
#[test]
fn a_read_only_database_compacts_and_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("ro0400");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o400)).unwrap();
    compact(&db, &opts()).expect("compacts");
    assert_eq!(
        std::fs::metadata(&db).unwrap().permissions().mode() & 0o777,
        0o400
    );
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review: the old sidecar is kept when the index lost a session or a
/// symbol reference since, not only a memory.
#[test]
fn the_old_sidecar_is_kept_for_a_lost_session_or_reference() {
    for sql in [
        "DELETE FROM sessions",
        "DELETE FROM memory_symbol_references WHERE memory_id = 'mem_b'",
    ] {
        let dir = scratch("side_lost");
        let db = dir.join("index.duckdb");
        fixture(&db, Fx::default());
        compact(&db, &opts()).unwrap();
        {
            let s = Store::open(&db, DIM).unwrap();
            s.conn.execute_batch(sql).unwrap();
            s.checkpoint();
        }
        let r = compact(&db, &opts()).unwrap();
        assert!(
            r.kept_sidecar.is_some(),
            "{sql}: the old sidecar was replaced"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Review: `compact()` runs the preflight itself, whoever calls it, and with
/// `--keep-original` it asks for disk for two copies.
#[test]
fn compact_runs_its_own_preflight_and_keep_original_needs_twice_the_disk() {
    let dir = scratch("own_preflight");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let size = std::fs::metadata(&db).unwrap().len();
    let mut o = opts();
    o.probe = SystemProbe {
        available_disk: |_| Some(10),
        ..SystemProbe::default()
    };
    let e = compact(&db, &o).unwrap_err();
    assert_eq!(e.kind, FailKind::Preflight, "{e}");
    assert!(!CompactPaths::for_db(&db).tmp.exists());
    let probe = SystemProbe {
        available_disk: |p| {
            let db = p.join("index.duckdb");
            Some(std::fs::metadata(db).unwrap().len() * 3 / 2)
        },
        ..SystemProbe::default()
    };
    assert!(preflight_for(&db, None, &probe, false).is_ok());
    let e = preflight_for(&db, None, &probe, true).unwrap_err();
    assert!(e.message.contains("--keep-original"), "{e}");
    let _ = size;
    let _ = std::fs::remove_dir_all(&dir);
}

// --- a rename a power cut reverted (review D1) --------------------------------

/// Put the directory back as a power cut can leave it after a `rename` whose
/// directory entry never reached the disk: `<db>` is the original again and
/// the verified copy — which a server may have written to meanwhile — is
/// back under `<tmp>`. Returns the copy's path.
fn revert_rename(db: &Path) -> PathBuf {
    let p = CompactPaths::for_db(db);
    std::fs::rename(&p.db, &p.tmp).unwrap();
    std::fs::rename(&p.original, &p.db).unwrap();
    p.tmp
}

/// D1: a copy that holds memories the database lacks is never deleted, by an
/// open or by the next compaction: the open is refused, naming it.
#[test]
fn a_reverted_rename_with_new_memories_is_never_deleted() {
    let dir = scratch("reverted");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let mut o = opts();
    o.keep_original = true;
    compact(&db, &o).unwrap();
    let tmp = revert_rename(&db);
    {
        // What the server wrote after the rename, into the new inode.
        let s = Store::open_unlocked(&tmp, DIM).unwrap();
        s.upsert_memory(&memory("mem_after", "written after the rename"))
            .unwrap();
        s.checkpoint();
    }
    let sum = sha(&tmp);
    match Store::open(&db, DIM) {
        Err(crate::StoreError::UnreconciledCompaction { tmp: t, .. }) => assert_eq!(t, tmp),
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("opened over a copy that holds memories the index lacks"),
    }
    assert_eq!(sha(&tmp), sum, "the copy was touched");
    let e = compact(&db, &opts()).unwrap_err();
    assert_eq!(e.kind, FailKind::Unreconciled, "{e}");
    assert_eq!(sha(&tmp), sum, "the copy was touched");
    let _ = std::fs::remove_dir_all(&dir);
}

/// D1, the other side: a copy with nothing the database lacks (a run killed
/// after it was written, or a reverted rename nobody wrote to) still goes at
/// the next open, as before.
#[test]
fn a_finished_copy_with_nothing_new_is_still_cleaned_up() {
    let dir = scratch("reverted_clean");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let mut o = opts();
    o.keep_original = true;
    compact(&db, &o).unwrap();
    let tmp = revert_rename(&db);
    drop(Store::open(&db, DIM).expect("opens"));
    assert!(!tmp.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// D1 (b): if the directory cannot be synced after the `rename`, the run
/// reports it and keeps the lock: nothing may write to the new file until
/// its name is durable.
#[test]
fn a_failed_directory_sync_keeps_the_lock_and_says_so() {
    let dir = scratch("dirsync");
    let db = dir.join("index.duckdb");
    fixture(&db, Fx::default());
    let mut o = opts();
    o.dir_sync = Some(|_| Err(std::io::Error::other("injected")));
    let e = compact(&db, &o).unwrap_err();
    assert_eq!(e.kind, FailKind::Durability, "{e}");
    assert!(e.message.contains("injected"), "{e}");
    assert!(
        try_lock_exclusive(&db).unwrap().is_none(),
        "the lock was released"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- child processes ---------------------------------------------------------

fn signal(db: &Path, name: &str) -> PathBuf {
    db.with_extension(format!("duckdb.{name}"))
}

fn wait_for(db: &Path, name: &str) {
    let t0 = Instant::now();
    while !signal(db, name).exists() {
        assert!(t0.elapsed() < Duration::from_secs(120), "no {name}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn spawn_child(db: &Path, mode: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .arg("compact::tests::compact_child")
        .arg("--exact")
        .arg("--nocapture")
        .env("DEVCTX_COMPACT_CHILD", mode)
        .env("DEVCTX_COMPACT_DB", db)
        .spawn()
        .unwrap()
}

fn child(db: &Path, mode: &str) {
    let _ = spawn_child(db, mode).wait().unwrap();
}

/// The other process of the tests above. A no-op under a normal run.
#[test]
fn compact_child() {
    let (Ok(mode), Ok(db)) = (
        std::env::var("DEVCTX_COMPACT_CHILD"),
        std::env::var("DEVCTX_COMPACT_DB"),
    ) else {
        return;
    };
    let db = PathBuf::from(db);
    let abort_at = |want: Phase| {
        move |p: Phase, _: &Connection| {
            if p == want {
                std::process::abort();
            }
            Ok(())
        }
    };
    let pause = |stray_fd: bool| {
        let db = db.clone();
        move |p: Phase, _: &Connection| {
            let at = match p {
                Phase::Verified => "verified",
                Phase::BeforeRename if !stray_fd => "rename",
                _ => return Ok(()),
            };
            if stray_fd {
                drop(std::fs::File::open(&db).unwrap());
            } else {
                // I2: `H`'s descriptor, and no other.
                #[cfg(target_os = "linux")]
                assert_eq!(descriptors_on(&db), 1, "{at}: descriptors on the source");
            }
            std::fs::write(signal(&db, &format!("ready-{at}")), b"").unwrap();
            wait_for(&db, &format!("go-{at}"));
            Ok(())
        }
    };
    let run = |hook: Hook<'_>| {
        let mut o = opts();
        o.hook = Some(hook);
        compact(&db, &o)
    };
    match mode.as_str() {
        "leave-wal" => {
            let s = Store::open(&db, DIM).unwrap();
            s.upsert(&[VectorPoint {
                id: "from_the_wal".into(),
                ..code_point(1)
            }])
            .unwrap();
            std::process::abort();
        }
        "hold-raw" => {
            let c = Connection::open(&db).unwrap();
            std::fs::write(signal(&db, "ready"), b"").unwrap();
            wait_for(&db, "go");
            drop(c);
        }
        "pause" => {
            run(&pause(false)).unwrap();
        }
        "pause-fdclose" => {
            let out = run(&pause(true)).map(|_| ()).map_err(|e| e.kind);
            std::fs::write(signal(&db, "outcome"), format!("{out:?}")).unwrap();
        }
        "abort-copied2" => {
            let _ = run(&abort_at(Phase::CopiedTable(2)));
        }
        "abort-indexes" => {
            let _ = run(&abort_at(Phase::IndexesBuilt));
        }
        "abort-verified" => {
            let _ = run(&abort_at(Phase::Verified));
        }
        "abort-renamed" => {
            let _ = run(&abort_at(Phase::Renamed));
        }
        other => panic!("unknown child mode {other}"),
    }
}
