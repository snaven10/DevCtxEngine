//! The link pass (PLAN-009 DD-6): after a run's files and before the
//! extractor stamp, resolve the branch's edges to the symbols they reach —
//! `dst_id`, `confidence`, `resolution`, `external` — or drop the calls
//! nothing can type (DD-7).
//!
//! The rules live in `devctx_parse::resolve::link` (pure, tested on parses);
//! this module loads the branch, decides which edges to re-resolve and writes
//! the answers back, one transaction per file, checking for a stop between
//! files. `contains` is never loaded nor written: it is intra-file and
//! resolved at parse time (DD-6), and the writer deletes every kind but it.
//!
//! **Incremental** (DD-6): re-resolved are (a) the edges of the files this run
//! wrote or deleted, (b) the edges of other files whose destination no longer
//! exists (its `dst_id` is gone: a deleted or renamed symbol) or whose
//! destination's last segment names a symbol of a written file (an added
//! one), and (c) those that stayed unresolved. When the written files are
//! more than a fifth of the branch, every edge is (the full pass). A file is
//! rewritten only if one of its rows changed.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use devctx_parse::resolve::link::{LinkEdge, LinkSymbol, Outcome, RepoIndex};
use devctx_store::{Store, StoredSymbolEdge};

use crate::error::Result;

/// What a link pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// Every edge was re-resolved, not only the incremental selection.
    pub full: bool,
    /// Edges re-resolved.
    pub resolved: usize,
    /// Calls dropped: an untypable receiver and a name the repository does
    /// not define (DD-7).
    pub discarded: usize,
    /// `calls` of the branch left with neither a destination nor the
    /// `external` mark, after the pass.
    pub unresolved_calls: usize,
    /// Files whose edges were rewritten.
    pub files_written: usize,
    /// Stopped between files; what it wrote is committed.
    pub cancelled: bool,
    /// Wall time, milliseconds.
    pub ms: u128,
    /// Of which loading the branch and building the index.
    pub load_ms: u128,
    /// Of which writing the rewritten files.
    pub write_ms: u128,
}

/// Files whose rewritten edges commit together. Each file's rows are written
/// whole inside one transaction — never split across two — but a transaction
/// (and a `DELETE` scan) per file cost ~10 ms each (1 254 files of a Java
/// repository: 12.9 of the pass's 14.1 s), so several files share one.
const WRITE_BATCH: usize = 64;

/// Write `pending` (whole files) in one transaction, and empty it.
fn write_batch(
    store: &Store,
    repo: &str,
    branch: &str,
    pending: &mut Vec<(&str, Vec<StoredSymbolEdge>)>,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    store.replace_files_linked_edges(repo, branch, pending)?;
    pending.clear();
    Ok(())
}

/// The fraction of a branch's files over which a run re-resolves every edge.
const FULL_PASS_DIVISOR: usize = 5;

fn leaf(name: &str) -> &str {
    let after = name.rsplit("::").next().unwrap_or(name);
    after.rsplit('.').next().unwrap_or(after)
}

fn to_link(e: &StoredSymbolEdge) -> LinkEdge {
    LinkEdge {
        kind: e.kind.clone(),
        src_id: e.src_id,
        dst_name: e.dst_name.clone(),
        hint: e.hint.clone(),
        file: e.file.clone(),
        line: e.line,
    }
}

/// Run the link pass over `branch`. `written`: the files whose graph rows
/// this run wrote or deleted; `full`: re-resolve every edge regardless.
pub(crate) fn link_branch(
    store: &Store,
    repo: &str,
    branch: &str,
    written: &HashSet<String>,
    full: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<LinkStats> {
    let started = Instant::now();
    let symbols = store.branch_symbols(repo, branch)?;
    let edges = store.branch_linkable_edges(repo, branch)?;
    let files: HashSet<&str> = symbols.iter().map(|s| s.file.as_str()).collect();
    let full = full || written.len() * FULL_PASS_DIVISOR > files.len();
    let ids: HashSet<u64> = symbols.iter().map(|s| s.id).collect();
    // Names a written file now defines: an edge elsewhere that names one may
    // reach it now (an added symbol).
    let fresh: HashSet<&str> = symbols
        .iter()
        .filter(|s| written.contains(&s.file))
        .map(|s| s.name.as_str())
        .collect();
    let facts: Vec<LinkEdge> = edges
        .iter()
        .filter(|e| matches!(e.kind.as_str(), "imports" | "inherits" | "implements"))
        .map(to_link)
        .collect();
    let index = RepoIndex::new(
        symbols
            .iter()
            .map(|s| LinkSymbol {
                id: s.id,
                parent_id: s.parent_id,
                file: s.file.clone(),
                kind: s.kind.clone(),
                name: s.name.clone(),
                qualified: s.qualified.clone(),
                package: s.package.clone(),
                signature: s.signature.clone(),
            })
            .collect(),
        &facts,
    );
    drop(facts);
    let loaded = started.elapsed().as_millis();
    let mut writing = std::time::Duration::ZERO;

    let mut stats = LinkStats {
        full,
        ..Default::default()
    };
    let mut by_file: HashMap<&str, Vec<&StoredSymbolEdge>> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for e in &edges {
        let list = by_file.entry(e.file.as_str()).or_insert_with(|| {
            order.push(e.file.as_str());
            Vec::new()
        });
        list.push(e);
    }
    let mut pending: Vec<(&str, Vec<StoredSymbolEdge>)> = Vec::new();
    for file in order {
        // Between files, never inside one: each file's rows are written
        // whole in their own transaction.
        if cancelled() {
            stats.cancelled = true;
            break;
        }
        let rows = &by_file[file];
        let file_written = written.contains(file);
        let mut out: Vec<StoredSymbolEdge> = Vec::with_capacity(rows.len());
        let mut changed = false;
        for e in rows {
            let pick = full
                || file_written
                || e.resolution.is_none()
                || e.dst_id.is_some_and(|d| !ids.contains(&d))
                || (e.dst_id.is_none() && !e.external.unwrap_or(false))
                || fresh.contains(leaf(&e.dst_name));
            if !pick {
                out.push((*e).clone());
                continue;
            }
            stats.resolved += 1;
            match index.resolve(&to_link(e)) {
                None => out.push((*e).clone()),
                Some(Outcome::Discard) => {
                    stats.discarded += 1;
                    changed = true;
                }
                Some(Outcome::Resolved(r)) => {
                    let new = StoredSymbolEdge {
                        dst_id: r.dst_id,
                        confidence: Some(r.confidence.to_string()),
                        resolution: Some(r.resolution.to_string()),
                        external: Some(r.external),
                        ..(*e).clone()
                    };
                    changed |= new != **e;
                    out.push(new);
                }
            }
        }
        stats.unresolved_calls += out
            .iter()
            .filter(|e| e.kind == "calls" && e.dst_id.is_none() && !e.external.unwrap_or(false))
            .count();
        if changed {
            pending.push((file, out));
            if pending.len() >= WRITE_BATCH {
                let w = Instant::now();
                write_batch(store, repo, branch, &mut pending)?;
                writing += w.elapsed();
            }
            stats.files_written += 1;
        }
    }
    let w = Instant::now();
    write_batch(store, repo, branch, &mut pending)?;
    writing += w.elapsed();
    stats.ms = started.elapsed().as_millis();
    stats.load_ms = loaded;
    stats.write_ms = writing.as_millis();
    Ok(stats)
}
