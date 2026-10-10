//! `repo_map` in the serve (PLAN-009 TASK-011, DD-12, DD-14): the branch's
//! adjacency ([`MapGraph`]) cached in [`AppState`] by `(repo, branch, index
//! generation)`, dropped when an `index` ends and when it goes unused as long
//! as a model would (`release_idle_models`); the map itself is
//! [`crate::repo_map::render`].
//!
//! There is no 0.9 path: a branch without a current symbol graph is an error
//! that asks for `devctx index --full`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use devctx_core::procmem;
use devctx_store::Store;

use super::lookup::{Legacy, Reader};
use super::{AppState, BranchChoice};
use crate::repo_map::{render, MapGraph, RepoMapQuery};

/// The error of a branch the current extractor has not indexed (DD-19).
pub const REPO_MAP_NEEDS_REINDEX: &str =
    "repo_map reads the symbol graph, and this branch's index predates it (an older \
     extractor, or no symbol rows): reindex with devctx index --full (vectors are reused)";

/// The error of a failed read of the symbol graph: not a reason to reindex.
const REPO_MAP_READ_ERROR: &str =
    "could not read the symbol graph of this branch (a read error, not a reason to \
     reindex); retry, and see serve.log if it persists";

/// The adjacency the serve holds for one branch.
pub(super) struct CachedMap {
    /// `(repo, branch, generation)` it was built for.
    key: (String, String, String),
    graph: Arc<MapGraph>,
    last_used: Instant,
    /// Wall-clock build time, seconds since the Unix epoch.
    loaded_at: u64,
    /// What building it took.
    took: Duration,
}

/// Refuses a branch without a current symbol graph, with the action.
pub(super) fn check(chosen: &BranchChoice, store: &Store) -> Result<(), String> {
    match Reader::of(chosen, store) {
        Reader::SymbolGraph => Ok(()),
        Reader::Legacy(Legacy::Error) => Err(REPO_MAP_READ_ERROR.into()),
        Reader::Legacy(_) => Err(REPO_MAP_NEEDS_REINDEX.into()),
    }
}

/// Reads a branch's graph: its symbols and its `live_edges`, aggregated.
pub(super) fn load(store: &Store, repo: &str, branch: &str) -> Result<MapGraph, String> {
    let symbols = store
        .map_symbols(repo, branch)
        .map_err(|_| REPO_MAP_READ_ERROR.to_string())?;
    let edges = store
        .branch_rank_edges(repo, branch)
        .map_err(|_| REPO_MAP_READ_ERROR.to_string())?;
    Ok(MapGraph::build(symbols, &edges))
}

/// What identifies an index of a branch: its record (commit, time, counts).
/// A run that ends rewrites it; the serve drops the cache anyway when its
/// own `index` ends.
fn generation(store: &Store, repo_path: &str, branch: &str) -> String {
    match store.get_index_record(repo_path, branch) {
        Ok(Some(r)) => format!(
            "{}|{}|{}|{}",
            r.last_commit, r.indexed_at, r.symbol_count, r.file_count
        ),
        _ => String::new(),
    }
}

/// The map `q` asks for over `graph`, the branch's notes at the end.
pub(super) fn map_on(graph: &MapGraph, chosen: &BranchChoice, q: &RepoMapQuery) -> String {
    let max = super::env_usize("DEVCTX_MAX_OUTPUT_TOKENS", super::DEFAULT_MAX_OUTPUT_TOKENS);
    let mut tail = Vec::new();
    if let Some(f) = &chosen.fallback {
        tail.push(format!(
            "[devctx] branch_fallback: {}",
            f.to_json()["why"]
                .as_str()
                .unwrap_or("answered from another branch")
        ));
    }
    render(graph, q, max, &tail)
}

impl AppState {
    /// The adjacency of `chosen`, from the cache when it was built for the
    /// same index generation, else read and kept.
    pub(super) fn repo_map_graph(
        &self,
        store: &Store,
        chosen: &BranchChoice,
    ) -> Result<Arc<MapGraph>, String> {
        let key = (
            chosen.repo.clone(),
            chosen.branch.clone(),
            generation(store, &self.repo_path(), &chosen.branch),
        );
        let mut slot = self.repo_map.lock().map_err(|e| e.to_string())?;
        if let Some(c) = slot.as_mut().filter(|c| c.key == key) {
            c.last_used = Instant::now();
            return Ok(c.graph.clone());
        }
        // Another branch, or another generation: the old one goes first.
        *slot = None;
        let (rss_before, started) = (procmem::rss_bytes(), Instant::now());
        let graph = Arc::new(load(store, &chosen.repo, &chosen.branch)?);
        let took = started.elapsed();
        self.repo_map_loads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        super::log_model_event(
            "loaded repo map graph",
            &format!(
                "{}@{} ({} symbols, {} edges, ~{} KiB)",
                chosen.repo,
                chosen.branch,
                graph.symbol_count(),
                graph.edge_count(),
                graph.heap_bytes() / 1024
            ),
            Some(took),
            rss_before,
        );
        *slot = Some(CachedMap {
            key,
            graph: graph.clone(),
            last_used: Instant::now(),
            loaded_at: procmem::unix_now(),
            took,
        });
        Ok(graph)
    }

    /// Drops the cached adjacency: an `index` ended, or it went unused.
    pub(super) fn drop_repo_map(&self) -> bool {
        self.repo_map
            .lock()
            .map(|mut s| s.take().is_some())
            .unwrap_or(false)
    }

    /// Drops the adjacency when unused for `max_idle`.
    pub(super) fn release_idle_repo_map(&self, max_idle: Duration) -> bool {
        let Ok(mut slot) = self.repo_map.lock() else {
            return false;
        };
        if slot
            .as_ref()
            .is_some_and(|c| c.last_used.elapsed() >= max_idle)
        {
            *slot = None;
            return true;
        }
        false
    }

    /// The `repo_map` block of `status.memory`: what the cache holds.
    pub(super) fn repo_map_json(&self) -> serde_json::Value {
        let held = self.repo_map.lock().ok().and_then(|s| {
            s.as_ref().map(|c| {
                serde_json::json!({
                    "loaded": true,
                    "repo": c.key.0,
                    "branch": c.key.1,
                    "symbols": c.graph.symbol_count(),
                    "edges": c.graph.edge_count(),
                    "estimated_bytes": c.graph.heap_bytes(),
                    "load_ms": c.took.as_millis() as u64,
                    "loaded_at": c.loaded_at,
                    "idle_secs": c.last_used.elapsed().as_secs(),
                })
            })
        });
        held.unwrap_or_else(|| serde_json::json!({ "loaded": false }))
    }
}

#[cfg(test)]
mod tests {
    use super::super::lookup::tests::{git, indexed_files};
    use super::super::*;
    use crate::repo_map::RepoMapQuery;

    /// A small Java application: a resource calls a service that uses a
    /// repository; a test calls the repository; the resource imports a
    /// library annotation.
    const APP: &[(&str, &str)] = &[
        (
            "src/main/java/app/core/Repo.java",
            "package app.core;\n\npublic class Repo {\n    public void save(String id) {\n    }\n}\n",
        ),
        (
            "src/main/java/app/core/Service.java",
            "package app.core;\n\npublic class Service {\n    private final Repo repo = new Repo();\n\n    public void update(String id) {\n        repo.save(id);\n    }\n}\n",
        ),
        (
            "src/main/java/app/web/Resource.java",
            "package app.web;\n\nimport jakarta.inject.Inject;\nimport app.core.Service;\n\npublic class Resource {\n    @Inject\n    Service service;\n\n    public void put(String id) {\n        service.update(id);\n    }\n}\n",
        ),
        (
            "src/test/java/app/core/RepoTest.java",
            "package app.core;\n\npublic class RepoTest {\n    private final Repo repo = new Repo();\n\n    void saves() {\n        repo.save(\"x\");\n    }\n}\n",
        ),
        (
            "rs/src/lib.rs",
            "pub fn serve() {\n    helper();\n}\n\nfn helper() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn checks() {\n        super::helper();\n    }\n}\n",
        ),
    ];

    fn map(state: &AppState, q: &RepoMapQuery) -> String {
        do_repo_map(state, q).unwrap()
    }

    fn focus(files: &[&str]) -> RepoMapQuery {
        RepoMapQuery {
            focus_files: files.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// (b) and (d) on a real index: from the resource, the service and the
    /// repository it reaches come first; test code (a test file, or a
    /// `#[cfg(test)]` module) and the library never appear; the closing line
    /// says what was shown.
    #[test]
    fn a_map_of_a_real_index() {
        let (state, repo) = indexed_files("repo_map_real", APP);
        let m = map(&state, &RepoMapQuery::default());
        for gone in ["RepoTest", "saves", "checks", "jakarta", "Inject"] {
            assert!(!m.contains(gone), "{gone}:\n{m}");
        }
        assert!(m.contains("public void update(String id)"), "{m}");
        assert!(m.contains("[devctx] repo map: "), "{m}");
        let m = map(&state, &focus(&["src/main/java/app/web/Resource.java"]));
        let first = m.lines().next().unwrap();
        assert_eq!(first, "src/main/java/app/core/Service.java", "{m}");
        assert!(!m.contains("public void put(String id)"), "{m}");
        assert!(!m.contains("RepoTest") && !m.contains("checks"), "{m}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// (f) A branch indexed by an older extractor, or with no symbol rows:
    /// an error that asks for `devctx index --full`, never an empty map.
    #[test]
    fn an_old_index_asks_for_a_full_reindex() {
        let (state, repo) = indexed_files("repo_map_stale", APP);
        {
            let store = state.open_store().unwrap();
            let (_, b) = state.repo_branch().unwrap();
            store
                .set_index_meta(
                    &state.repo_path(),
                    &b,
                    devctx_store::EXTRACTOR_META_KEY,
                    "v0-old",
                )
                .unwrap();
        }
        let e = do_repo_map(&state, &RepoMapQuery::default()).unwrap_err();
        assert!(e.contains("devctx index --full"), "{e}");
        let _ = std::fs::remove_dir_all(&repo);
        let (state, repo) = indexed_files(
            "repo_map_norows",
            &[("db/schema.sql", "CREATE TABLE things (id INT);\n")],
        );
        let e = do_repo_map(&state, &RepoMapQuery::default()).unwrap_err();
        assert!(e.contains("devctx index --full"), "{e}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// The adjacency is read once and reused; an `index` that ends drops it
    /// (the next map shows the new symbol); unused, it is released with the
    /// models and named; `status` reports it while held.
    #[test]
    fn the_adjacency_is_cached_until_an_index_or_idleness() {
        let (state, repo) = indexed_files("repo_map_cache", APP);
        let loads = || state.repo_map_loads.load(Ordering::Relaxed);
        map(&state, &RepoMapQuery::default());
        map(&state, &focus(&["Resource.java"]));
        assert_eq!(loads(), 1);
        let store = state.open_store().unwrap();
        let mem = state.memory_json(&store);
        assert_eq!(mem["repo_map"]["loaded"], true, "{mem}");
        assert!(mem["repo_map"]["estimated_bytes"].as_u64().unwrap() > 0);
        drop(store);
        std::fs::write(
            repo.join("src/main/java/app/core/Audit.java"),
            "package app.core;\n\npublic class Audit {\n    public void record(String what) {\n    }\n}\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "audit"]);
        do_index(&state, false).unwrap();
        // The run that ended dropped it, before anyone asks again.
        let store = state.open_store().unwrap();
        assert_eq!(state.memory_json(&store)["repo_map"]["loaded"], false);
        drop(store);
        let m = map(&state, &RepoMapQuery::default());
        assert_eq!(loads(), 2);
        assert!(m.contains("public void record(String what)"), "{m}");
        let released = state.release_idle_models(Duration::ZERO);
        assert!(released.contains(&"repo map graph"), "{released:?}");
        let store = state.open_store().unwrap();
        assert_eq!(state.memory_json(&store)["repo_map"]["loaded"], false);
        drop(store);
        map(&state, &RepoMapQuery::default());
        assert_eq!(loads(), 3);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Another generation of the index (a run of another process, a record
    /// rewritten) is another adjacency: the key is checked on every call.
    #[test]
    fn another_index_generation_reloads() {
        let (state, repo) = indexed_files("repo_map_generation", APP);
        let loads = || state.repo_map_loads.load(Ordering::Relaxed);
        map(&state, &RepoMapQuery::default());
        map(&state, &RepoMapQuery::default());
        assert_eq!(loads(), 1);
        {
            let store = state.open_store().unwrap();
            let (_, b) = state.repo_branch().unwrap();
            let mut rec = store
                .get_index_record(&state.repo_path(), &b)
                .unwrap()
                .unwrap();
            rec.indexed_at = "2099-01-01T00:00:00Z".into();
            store.save_index_record(&rec).unwrap();
        }
        map(&state, &RepoMapQuery::default());
        assert_eq!(loads(), 2);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Only `live_edges` rank: a call the link pass discarded lends its
    /// destination nothing (lesson of the reviews: discarded edges never
    /// count). From `run`, the file of the live callee `kept` is listed
    /// before that of `gone`, called 30 times by discarded calls and more
    /// central globally.
    #[test]
    fn a_discarded_call_lends_nothing() {
        use devctx_core::symbol_id::{file_symbol_id, symbol_id, FILE_KIND};
        use devctx_store::{StoredSymbol, StoredSymbolEdge};
        let store = Store::open_in_memory(3).unwrap();
        let sym = |file: &str, kind: &str, q: &str, parent: Option<u64>, line: i32| {
            let id = if kind == FILE_KIND {
                file_symbol_id("r", file)
            } else {
                symbol_id("r", file, kind, q, &line.to_string())
            };
            StoredSymbol {
                id,
                parent_id: parent,
                file: file.into(),
                kind: kind.into(),
                name: q.rsplit('.').next().unwrap().into(),
                qualified: q.into(),
                signature: Some(format!("fn {q}()")),
                start_line: line,
                end_line: line + 1,
                ..Default::default()
            }
        };
        let fa = sym("a.rs", FILE_KIND, "a.rs", None, 1);
        let run = sym("a.rs", "function", "run", Some(fa.id), 2);
        let fb = sym("b.rs", FILE_KIND, "b.rs", None, 1);
        let kept = sym("b.rs", "function", "kept", Some(fb.id), 2);
        let fd = sym("d.rs", FILE_KIND, "d.rs", None, 1);
        let gone = sym("d.rs", "function", "gone", Some(fd.id), 5);
        let fc = sym("c.rs", FILE_KIND, "c.rs", None, 1);
        let users: Vec<StoredSymbol> = (0..3)
            .map(|i| sym("c.rs", "function", &format!("u{i}"), Some(fc.id), 2 + 3 * i))
            .collect();
        let call = |src: &StoredSymbol, dst: &StoredSymbol, file: &str, resolution: &str| {
            StoredSymbolEdge {
                kind: "calls".into(),
                src_id: src.id,
                dst_id: Some(dst.id),
                dst_name: dst.qualified.clone(),
                file: file.into(),
                line: src.start_line,
                confidence: Some("high".into()),
                resolution: Some(resolution.into()),
                external: Some(false),
                edge_source: "treesitter".into(),
                ..Default::default()
            }
        };
        let mut a_edges = vec![call(&run, &kept, "a.rs", "same_file")];
        for _ in 0..30 {
            a_edges.push(call(&run, &gone, "a.rs", "discarded"));
        }
        store
            .replace_file_graph("r", "main", "a.rs", &[fa, run.clone()], &a_edges)
            .unwrap();
        store
            .replace_file_graph("r", "main", "b.rs", &[fb, kept.clone()], &[])
            .unwrap();
        store
            .replace_file_graph("r", "main", "d.rs", &[fd, gone.clone()], &[])
            .unwrap();
        let c_edges: Vec<StoredSymbolEdge> = users
            .iter()
            .map(|u| call(u, &gone, "c.rs", "same_file"))
            .collect();
        let mut c_syms = vec![fc];
        c_syms.extend(users);
        store
            .replace_file_graph("r", "main", "c.rs", &c_syms, &c_edges)
            .unwrap();
        let g = super::load(&store, "r", "main").unwrap();
        let m = crate::repo_map::render(&g, &focus(&["a.rs"]), 8000, &[]);
        // Files go by their symbols' summed rank: the live callee's first.
        assert!(m.starts_with("b.rs\n"), "{m}");
        assert!(m.contains("fn gone()"), "{m}");
    }
}

#[cfg(test)]
mod bench {
    use super::super::lookup::tests::Fake;
    use super::super::*;
    use crate::repo_map::{tokens, RepoMapQuery};

    /// Measurement harness (PLAN-009 TASK-011, not a test): indexes
    /// `$DEVCTX_MAP_BENCH_REPO` into `$DEVCTX_MAP_BENCH_STATE` with the
    /// model-free embedder, then, for each case of `$DEVCTX_MAP_BENCH_CASES`
    /// (the graph-eval format: `query | expected file(s) | symbol | repo |
    /// lang`) whose repo is `$DEVCTX_MAP_BENCH_NAME`, draws the map with the
    /// query as its focus (budget 1024): one JSON line per case with its
    /// tokens, whether the focus matched and whether an expected file is in
    /// the map. Then the latency: the first load (cache empty) and
    /// `$DEVCTX_MAP_BENCH_RUNS` (40) warm calls per case, interleaved, p50 /
    /// p95; and the cache's size and the process RSS around its load.
    ///
    /// `cargo test --release -p devctx-mcp --lib repo_map_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn repo_map_bench() {
        let var = |k: &str| std::env::var(k).ok();
        let (Some(repo), Some(state_dir), Some(cases), Some(name)) = (
            var("DEVCTX_MAP_BENCH_REPO"),
            var("DEVCTX_MAP_BENCH_STATE"),
            var("DEVCTX_MAP_BENCH_CASES"),
            var("DEVCTX_MAP_BENCH_NAME"),
        ) else {
            eprintln!("set DEVCTX_MAP_BENCH_REPO, _STATE, _CASES and _NAME");
            return;
        };
        let runs: usize = var("DEVCTX_MAP_BENCH_RUNS")
            .and_then(|r| r.parse().ok())
            .unwrap_or(40);
        let mut cfg = ProjectConfig::default();
        cfg.project.path = repo;
        cfg.state_dir = state_dir;
        cfg.storage.hnsw = false;
        let state = AppState::build(cfg).unwrap();
        let dim = configured_dimension(&state.cfg);
        *state.embedder.lock().unwrap() = Some(Cached {
            value: Arc::new(Fake(dim)),
            last_used: Instant::now(),
            key: "fake/fake".into(),
            loaded_at: procmem::unix_now(),
        });
        let t = Instant::now();
        do_index(&state, false).unwrap();
        eprintln!("index: {:.1} s", t.elapsed().as_secs_f64());
        let text = std::fs::read_to_string(cases).unwrap();
        let mut list: Vec<(String, Vec<String>, RepoMapQuery)> = Vec::new();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split('|').map(str::trim).collect();
            if f.len() < 4 || f[3] != name {
                continue;
            }
            let expected: Vec<String> = f[1].split(',').map(|s| s.trim().to_string()).collect();
            list.push((
                f[0].to_string(),
                expected,
                RepoMapQuery {
                    query: Some(f[0].to_string()),
                    ..Default::default()
                },
            ));
        }
        // The first load: an empty cache, then the map.
        state.drop_repo_map();
        let rss0 = procmem::rss_bytes();
        let t = Instant::now();
        let first = do_repo_map(&state, &RepoMapQuery::default()).unwrap();
        let first_ms = t.elapsed().as_secs_f64() * 1000.0;
        let rss1 = procmem::rss_bytes();
        let store = state.open_store().unwrap();
        let mem = state.memory_json(&store)["repo_map"].clone();
        drop(store);
        println!(
            "{}",
            json!({ "first_load_ms": first_ms, "cache": mem,
                    "rss_before": rss0, "rss_after": rss1,
                    "global_tokens": tokens(&first) })
        );
        if var("DEVCTX_MAP_BENCH_SHOW").is_some() {
            println!("{first}");
        }
        for (query, expected, q) in &list {
            let m = do_repo_map(&state, q).unwrap();
            let files: Vec<&str> = m
                .lines()
                .filter(|l| !l.starts_with('│') && !l.starts_with('⋮') && !l.starts_with('['))
                .collect();
            let present = expected
                .iter()
                .any(|e| files.iter().any(|f| f.contains(e.as_str())));
            let focused = m.contains("personalized to the focus");
            println!(
                "{}",
                json!({ "case": query, "tokens": tokens(&m), "focused": focused,
                        "present": present, "files": files.len() })
            );
            if var("DEVCTX_MAP_BENCH_SHOW").is_some() {
                println!("{m}");
            }
        }
        let mut ms: Vec<Vec<f64>> = vec![Vec::new(); list.len() + 1];
        let global = RepoMapQuery::default();
        for _ in 0..runs {
            for (i, q) in list
                .iter()
                .map(|(_, _, q)| q)
                .chain(std::iter::once(&global))
                .enumerate()
            {
                let t = Instant::now();
                do_repo_map(&state, q).unwrap();
                ms[i].push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        let mut all: Vec<f64> = Vec::new();
        for (i, m) in ms.iter_mut().enumerate() {
            m.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |p: f64| m[((m.len() as f64 - 1.0) * p).round() as usize];
            let label = list.get(i).map_or("(no focus)", |c| c.0.as_str());
            println!(
                "{}",
                json!({ "warm": label, "p50_ms": pct(0.5), "p95_ms": pct(0.95) })
            );
            all.extend(m.iter().copied());
        }
        all.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| all[((all.len() as f64 - 1.0) * p).round() as usize];
        println!(
            "{}",
            json!({ "warm_all": all.len(), "p50_ms": pct(0.5), "p95_ms": pct(0.95),
                    "loads": state.repo_map_loads.load(Ordering::Relaxed) })
        );
    }
}
