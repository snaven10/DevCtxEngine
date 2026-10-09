//! Global PageRank over a branch's symbol graph (PLAN-009 DD-12, TASK-010):
//! computed whole at the end of every link pass that completes, written to
//! `symbols.rank`, with `symbols.in_degree` beside it.
//!
//! - Nodes: the branch's symbols (externals have no row and no node).
//! - Edges: `live_edges` with a destination, so a discarded call never
//!   counts; aggregated per `(src, dst, kind, confidence, from_test)` with
//!   their multiplicity `n`, each weighing
//!   `kind × confidence × test × sqrt(n)` ([`edge_weight`]).
//! - Power iteration: damping 0.85, tolerance 1e-6 (L1), at most 50
//!   iterations, dangling nodes redistributed uniformly. A source spreads
//!   its rank over its edges by `weight / Σ kind × sqrt(n)`: kind and
//!   multiplicity split what it spreads, confidence and tests decide how
//!   much of it (the rest is redistributed like a dangling node's), so a
//!   test method whose every edge is from a test spreads a tenth. Nodes and edges are
//!   sorted first, so the same graph gives the same ranks bit for bit.
//!
//! Own implementation (~60 lines): no `petgraph` nor any other dependency.

use std::collections::HashMap;

use devctx_store::RankEdge;

/// Damping factor.
pub(crate) const DAMPING: f64 = 0.85;

/// L1 change between two iterations under which the iteration stops.
pub(crate) const TOLERANCE: f64 = 1e-6;

/// Iterations at most. 100, not DD-12's 50 (review of TASK-010, MINOR 8):
/// a slow-mixing graph can need ~90 to reach the tolerance; a run that
/// stops on the cap says so (`LinkStats::rank_converged`).
pub(crate) const MAX_ITERATIONS: usize = 100;

/// What a relation contributes to centrality (DD-12): a call or an
/// instantiation fully, inheritance a little less, a type use half, an
/// import less, `contains` nothing (it serves `traverse` and the repo map).
pub(crate) fn kind_weight(kind: &str) -> f64 {
    match kind {
        "calls" | "instantiates" => 1.0,
        "inherits" | "implements" => 0.8,
        "references" => 0.5,
        "imports" => 0.3,
        _ => 0.0,
    }
}

/// What a confidence contributes: `high` 1, `medium` 0.6, `low` (or none)
/// 0.2.
pub(crate) fn confidence_weight(c: Option<&str>) -> f64 {
    match c {
        Some("high") => 1.0,
        Some("medium") => 0.6,
        _ => 0.2,
    }
}

/// The weight of an aggregated edge: kind × confidence × (0.1 from a test:
/// tests do say what matters, but most edges of some repositories come from
/// them) × `sqrt(n)` (a helper called 300 times must not eat the ranking).
pub(crate) fn edge_weight(e: &RankEdge) -> f64 {
    let test = if e.from_test { 0.1 } else { 1.0 };
    kind_weight(&e.kind) * confidence_weight(e.confidence.as_deref()) * test * (e.n as f64).sqrt()
}

/// The global rank of each of `nodes` (returned in the order of `nodes`),
/// over `edges`; an edge whose end is not a node, a self-loop or one of
/// weight 0 is ignored. The ranks sum to 1.
pub(crate) fn pagerank_run(nodes: &[u64], edges: &[RankEdge]) -> PageRankRun {
    let mut sorted: Vec<u64> = nodes.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let n = sorted.len();
    if n == 0 {
        return PageRankRun {
            ranks: Vec::new(),
            iterations: 0,
            converged: true,
        };
    }
    let index: HashMap<u64, usize> = sorted.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    // One (weight, share) per (src, dst), summed in a fixed order: the
    // weight is the edge's; the share, what the source would spread over it
    // if every occurrence were sure and outside tests (kind × sqrt(n)).
    let mut pairs: Vec<(usize, usize, f64, f64)> = edges
        .iter()
        .filter(|e| e.src != e.dst)
        .filter_map(|e| {
            let w = edge_weight(e);
            let (s, d) = (*index.get(&e.src)?, *index.get(&e.dst)?);
            (w > 0.0).then(|| (s, d, w, kind_weight(&e.kind) * (e.n as f64).sqrt()))
        })
        .collect();
    pairs.sort_by(|a, b| {
        (a.0, a.1)
            .cmp(&(b.0, b.1))
            .then(a.2.total_cmp(&b.2))
            .then(a.3.total_cmp(&b.3))
    });
    let mut merged: Vec<(usize, usize, f64)> = Vec::with_capacity(pairs.len());
    let mut full = vec![0.0f64; n];
    for (s, d, w, share) in pairs {
        full[s] += share;
        match merged.last_mut() {
            Some(last) if last.0 == s && last.1 == d => last.2 += w,
            _ => merged.push((s, d, w)),
        }
    }
    // A source spreads `weight / full` over each edge: all of its rank when
    // every edge is a sure call from production code, less over an edge
    // that is `low`, `medium` or from a test. What it does not spread (all
    // of it for a node with no edge out) is redistributed uniformly, like a
    // dangling node's. Normalising by the weights themselves would cancel
    // the confidence and test factors for a source whose edges all share
    // them (a test method: every edge from a test).
    let mut kept = vec![0.0f64; n];
    for &(s, _, w) in &merged {
        kept[s] += w / full[s];
    }
    let nf = n as f64;
    let mut rank = vec![1.0 / nf; n];
    let (mut iterations, mut converged) = (0, false);
    for _ in 0..MAX_ITERATIONS {
        iterations += 1;
        let leaked: f64 = (0..n).map(|i| rank[i] * (1.0 - kept[i]).max(0.0)).sum();
        let base = (1.0 - DAMPING) / nf + DAMPING * leaked / nf;
        let mut next = vec![base; n];
        for &(s, d, w) in &merged {
            next[d] += DAMPING * rank[s] * w / full[s];
        }
        let delta: f64 = next.iter().zip(&rank).map(|(a, b)| (a - b).abs()).sum();
        rank = next;
        if delta < TOLERANCE {
            converged = true;
            break;
        }
    }
    PageRankRun {
        ranks: nodes.iter().map(|id| rank[index[id]]).collect(),
        iterations,
        converged,
    }
}

/// A PageRank run: the ranks, the iterations it took and whether it
/// converged under [`TOLERANCE`] before [`MAX_ITERATIONS`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PageRankRun {
    pub ranks: Vec<f64>,
    pub iterations: usize,
    pub converged: bool,
}

/// [`pagerank_run`]'s ranks.
#[cfg(test)]
pub(crate) fn pagerank(nodes: &[u64], edges: &[RankEdge]) -> Vec<f64> {
    pagerank_run(nodes, edges).ranks
}

/// What [`rank_branch`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct BranchRanks {
    /// Symbols ranked.
    pub ranked: usize,
    /// Of which written: their rank or in-degree changed (review of
    /// TASK-010, MINOR 5: rewriting every row of an unchanged graph left
    /// free blocks for nothing, DD-21).
    pub written: usize,
    /// Power iterations.
    pub iterations: usize,
    /// Converged under the tolerance.
    pub converged: bool,
}

/// Rank a branch: the PageRank of `symbols` (its symbols, with the rank and
/// in-degree they hold now) over its
/// [`branch_rank_edges`](devctx_store::Store::branch_rank_edges), and each
/// symbol's `in_degree` — incoming `calls` occurrences, not from a test, of
/// `high` or `medium` confidence —; only the symbols whose values changed
/// are written to `symbols`.
pub(crate) fn rank_branch(
    store: &devctx_store::Store,
    repo: &str,
    branch: &str,
    symbols: &[devctx_store::StoredSymbol],
) -> crate::error::Result<BranchRanks> {
    let edges = store.branch_rank_edges(repo, branch)?;
    let ids: Vec<u64> = symbols.iter().map(|s| s.id).collect();
    let run = pagerank_run(&ids, &edges);
    let mut in_degree: HashMap<u64, i64> = HashMap::new();
    for e in &edges {
        if e.kind == "calls"
            && !e.from_test
            && matches!(e.confidence.as_deref(), Some("high" | "medium"))
        {
            *in_degree.entry(e.dst).or_default() += e.n as i64;
        }
    }
    let rows: Vec<(u64, f64, i32)> = symbols
        .iter()
        .zip(&run.ranks)
        .filter_map(|(s, &r)| {
            let d = in_degree.get(&s.id).copied().unwrap_or(0);
            let d = i32::try_from(d).unwrap_or(i32::MAX);
            let same = s.rank.map(f64::to_bits) == Some(r.to_bits()) && s.in_degree == Some(d);
            (!same).then_some((s.id, r, d))
        })
        .collect();
    if !rows.is_empty() {
        store.write_branch_ranks(repo, branch, &rows)?;
    }
    Ok(BranchRanks {
        ranked: symbols.len(),
        written: rows.len(),
        iterations: run.iterations,
        converged: run.converged,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(src: u64, dst: u64) -> RankEdge {
        RankEdge {
            src,
            dst,
            kind: "calls".into(),
            confidence: Some("high".into()),
            from_test: false,
            n: 1,
        }
    }

    /// (a) Six nodes with a known order: five callers of `6`, which calls
    /// `1`. `6` ranks first, `1` second (it inherits from `6`), the four
    /// plain callers tie last; the ranks sum to 1.
    #[test]
    fn a_six_node_graph_ranks_as_expected() {
        let nodes = [1, 2, 3, 4, 5, 6];
        let edges: Vec<RankEdge> = (1..=5).map(|i| e(i, 6)).chain([e(6, 1)]).collect();
        let r = pagerank(&nodes, &edges);
        assert!(r[5] > r[0], "{r:?}");
        for i in 1..5 {
            assert!(r[0] > r[i], "{r:?}");
            assert!((r[i] - r[1]).abs() < 1e-12, "{r:?}");
        }
        assert!((r.iter().sum::<f64>() - 1.0).abs() < 1e-6, "{r:?}");
        // Review MINOR 8: how many iterations it took, and that it converged.
        let run = pagerank_run(&nodes, &edges);
        assert!(run.converged && run.iterations <= MAX_ITERATIONS, "{run:?}");
        assert_eq!(run.ranks, r);
    }

    /// (a) A dangling node (no way out) hands its rank to everyone: the
    /// ranks still sum to 1 and an isolated node gets more than nothing.
    #[test]
    fn dangling_nodes_are_redistributed() {
        let nodes = [10, 20, 30, 40];
        let r = pagerank(&nodes, &[e(10, 20), e(30, 20)]);
        assert!((r.iter().sum::<f64>() - 1.0).abs() < 1e-6, "{r:?}");
        assert!(r[1] > r[0] && r[3] > 0.0, "{r:?}");
        assert!((r[0] - r[2]).abs() < 1e-12 && (r[0] - r[3]).abs() < 1e-12);
    }

    /// (a) The same graph, its nodes and edges in another order, gives the
    /// same ranks bit for bit.
    #[test]
    fn the_ranks_are_deterministic() {
        let nodes: Vec<u64> = (1..=40).map(|i| i * 7919 % 1000).collect();
        let conf = ["high", "medium", "low"];
        let edges: Vec<RankEdge> = (0..600)
            .map(|i| {
                let mut x = e(nodes[i % 40], nodes[(i * 13 + 5) % 23]);
                x.n = 1 + (i as u64 * 37 % 11);
                x.confidence = Some(conf[i % 3].into());
                x.from_test = i % 7 == 0;
                x
            })
            .collect();
        let a = pagerank(&nodes, &edges);
        let mut rn = nodes.clone();
        rn.reverse();
        let mut re = edges.clone();
        re.reverse();
        let b = pagerank(&rn, &re);
        for (i, id) in nodes.iter().enumerate() {
            let j = rn.iter().position(|x| x == id).unwrap();
            assert_eq!(a[i].to_bits(), b[j].to_bits(), "{id}");
        }
        assert_eq!(a, pagerank(&nodes, &edges));
    }

    /// (b) Weights: a call from a test weighs 0.1, a `low` one 0.2, four
    /// occurrences `sqrt(4)` = 2; `contains` nothing, an import 0.3.
    #[test]
    fn edge_weights_follow_kind_confidence_tests_and_multiplicity() {
        let mut t = e(1, 2);
        t.from_test = true;
        assert!((edge_weight(&t) - 0.1).abs() < 1e-12);
        let mut low = e(1, 2);
        low.confidence = Some("low".into());
        assert!((edge_weight(&low) - 0.2).abs() < 1e-12);
        let mut many = e(1, 2);
        many.n = 4;
        assert!((edge_weight(&many) - 2.0).abs() < 1e-12);
        let mut c = e(1, 2);
        c.kind = "contains".into();
        assert_eq!(edge_weight(&c), 0.0);
        let mut i = e(1, 2);
        i.kind = "imports".into();
        i.confidence = Some("medium".into());
        assert!((edge_weight(&i) - 0.18).abs() < 1e-12);
    }

    /// (b) In the graph: of two symbols with one caller each, the one called
    /// from production code outranks the one called from a test, and one
    /// called 100 times by the same caller does not outrank one called by
    /// two callers ten times as much as `n` would.
    #[test]
    fn weights_shape_the_ranking() {
        let mut from_test = e(2, 4);
        from_test.from_test = true;
        let r = pagerank(&[1, 2, 3, 4], &[e(1, 3), from_test]);
        assert!(r[2] > r[3], "{r:?}");
        // 1 calls 5 a hundred times and 6 twenty-five times: what it passes
        // on splits sqrt(100) : sqrt(25) = 2 : 1 (it would be 4 : 1 by the
        // raw count, review MINOR 7). 7, called by nobody, is the floor.
        let mut hundred = e(1, 5);
        hundred.n = 100;
        let mut quarter = e(1, 6);
        quarter.n = 25;
        let r = pagerank(&[1, 5, 6, 7], &[hundred, quarter]);
        let ratio = (r[1] - r[3]) / (r[2] - r[3]);
        assert!((ratio - 2.0).abs() < 1e-6, "{ratio} {r:?}");
    }

    /// A self-loop, an edge to a node that is not there and an empty graph
    /// do not break anything.
    #[test]
    fn degenerate_inputs() {
        assert!(pagerank(&[], &[e(1, 2)]).is_empty());
        let r = pagerank(&[1, 2], &[e(1, 1), e(1, 99)]);
        assert!((r[0] - r[1]).abs() < 1e-12, "{r:?}");
    }
}
