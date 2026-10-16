//! Parse-phase benchmark (PLAN-009 DD-21): parse every file of a tree the
//! way the indexer does (`devctx_parse::parse` per file), `--runs` times, and
//! report the time per run (best and median) and the symbols by kind.
//!
//! Uses only the API every release has (`detect_lang`, `parse`), so the same
//! file builds against an older checkout for a before/after comparison:
//!
//! ```text
//! cargo run --release -p devctx-parse --example parse_bench -- <dir> [--runs N]
//! ```
//!
//! Walks `<dir>` skipping hidden directories, `target`, `node_modules` and
//! `build`; reads files as UTF-8 up front, so only parsing is timed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules" | "build")
            {
                continue;
            }
            walk(&p, out);
        } else if devctx_parse::detect_lang(&p).is_some() {
            out.push(p);
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: parse_bench <dir> [--runs N]"));
    let mut runs = 5usize;
    while let Some(a) = args.next() {
        if a == "--runs" {
            runs = args.next().and_then(|n| n.parse().ok()).unwrap_or(runs);
        }
    }
    let mut files = Vec::new();
    walk(&dir, &mut files);
    files.sort();
    let sources: Vec<(PathBuf, String)> = files
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|s| (p, s)))
        .collect();
    let mut times = Vec::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut failed = 0usize;
    for run in 0..runs {
        let t0 = Instant::now();
        for (path, src) in &sources {
            let lang = devctx_parse::detect_lang(path).expect("filtered by language");
            match devctx_parse::parse(lang, src) {
                Ok(pf) if run == 0 => {
                    for s in &pf.symbols {
                        *kinds.entry(s.kind.clone()).or_default() += 1;
                    }
                }
                Ok(_) => {}
                Err(_) if run == 0 => failed += 1,
                Err(_) => {}
            }
        }
        times.push(t0.elapsed().as_secs_f64());
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let bytes: usize = sources.iter().map(|(_, s)| s.len()).sum();
    println!(
        "files={} bytes={} failed={} runs={} best={:.3}s median={:.3}s",
        sources.len(),
        bytes,
        failed,
        runs,
        times[0],
        times[times.len() / 2]
    );
    for (k, n) in &kinds {
        println!("symbols {k} {n}");
    }
}
