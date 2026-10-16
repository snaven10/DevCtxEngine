//! Index a repository's graph without a model, to measure it (PLAN-009
//! TASK-005, the `scripts/graph-eval/` harness).
//!
//! `pipeline::run` with an embedder that returns constant vectors: the parse,
//! the `symbols`/`edges` writes and the link pass are the real ones, the
//! vectors are not (no search over the result is meaningful). An index of a
//! large repository costs seconds instead of the hour the embedding takes, so
//! the graph metrics and the gold edges of the harness can be measured on
//! every repository, before and after a change, with the same binary.
//!
//! ```text
//! graph_bench <repo dir> <db path> [--touch <repo-relative file>]
//! ```
//!
//! Prints the run's summary, its wall time and the process's `VmHWM` (Linux).
//! `--touch` then appends a newline to the file and indexes only it, as a
//! watcher would: the incremental cost of one file (its link pass included).
//! Writes `<db path>`; point the harness at it with a `.devctx/config.yaml`
//! whose `storage.db_path` names it. Uses only the API every release since
//! 0.9.0 has, so the same file builds against an older checkout.

use std::path::{Path, PathBuf};
use std::time::Instant;

use devctx_embed::EmbeddingProvider;
use devctx_index::{run, IndexRequest};
use devctx_store::Store;

const DIM: usize = 4;

/// Constant vectors: what is measured is everything but the embedding.
struct Constant;

impl EmbeddingProvider for Constant {
    fn embed(&self, texts: &[String]) -> devctx_embed::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.5; DIM]).collect())
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn model_name(&self) -> &str {
        "graph-bench-constant"
    }
}

/// `VmHWM` of this process in KiB, if the kernel reports it.
fn vm_hwm_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
}

fn index(store: &Store, repo: &Path, incremental: bool, paths: Option<&[String]>) {
    let started = Instant::now();
    let result = run(IndexRequest {
        store,
        embedder: &Constant,
        repo_root: repo,
        incremental,
        paths,
        branch: None,
        model_name: "graph-bench-constant",
        progress: None,
        exclude: &[],
        hnsw: None,
        embed_fingerprint: "graph-bench-constant",
    })
    .expect("the indexing run failed");
    println!("{result:#?}");
    println!(
        "wall={:.2}s vm_hwm_kib={}",
        started.elapsed().as_secs_f64(),
        vm_hwm_kib().map_or_else(|| "n/a".into(), |v| v.to_string())
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: graph_bench <repo dir> <db path> [--touch <repo-relative file>]");
        std::process::exit(2);
    }
    let repo = PathBuf::from(&args[0]);
    let store = Store::open(Path::new(&args[1]), DIM).expect("cannot open the database");
    index(&store, &repo, false, None);
    if let Some(i) = args.iter().position(|a| a == "--touch") {
        let file = args.get(i + 1).expect("--touch needs a file").clone();
        let path = repo.join(&file);
        let mut text = std::fs::read_to_string(&path).expect("cannot read the touched file");
        text.push('\n');
        std::fs::write(&path, text).expect("cannot write the touched file");
        println!("-- incremental: {file}");
        index(&store, &repo, true, Some(std::slice::from_ref(&file)));
    }
    store.checkpoint();
}
