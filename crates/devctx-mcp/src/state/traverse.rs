//! `traverse` (PLAN-009 TASK-013, DD-16): the agent walks the symbol graph
//! itself — from a symbol, along the relations it picks (`calls`,
//! `instantiates`, `imports`, `inherits`, `implements`, `contains`,
//! `references`), `in`, `out` or `both`, up to four levels — over the walk of
//! `impact_analysis` (one statement per level and direction, `live_edges`,
//! DD-11's filters, dispatch when `calls` is followed), paged.
//!
//! The answer: `{root, candidates?, nodes, edges, total, next_offset?,
//! omitted?, partial?, filters, below_confidence?, excluded?,
//! omitted_by_limit?, dispatch?, omitted_for_budget?, branch_fallback?}`.
//! Nodes carry `sym` (hex) when they have a definition; an edge's `from` and
//! `to` are the `sym` of its ends (the name, for an end with no definition).
//! There is no 0.9 path: a branch without a current symbol graph is an error
//! that asks for `devctx index --full`.

use std::collections::HashSet;

use devctx_store::{
    sym_hex, MinConfidence, Store, StoredSymbol, Traversal, TraverseDirection, TraverseEdge,
    TraverseEnd, TraverseNode, TraverseOptions, TRAVERSE_KINDS, TRAVERSE_MAX_DEPTH,
};
use serde_json::{json, Value};

use super::lookup::{self, Legacy, Reader};
use super::BranchChoice;

/// Nodes per page when `limit` says nothing.
pub const DEFAULT_TRAVERSE_LIMIT: usize = 50;

/// Definitions a name may stand for as roots: `impact_analysis`'s seed
/// limit (review of TASK-013, MINOR 1). Past it, `candidates_truncated`.
const ROOTS_LIMIT: usize = 10_000;

/// The error of a branch the current extractor has not indexed (DD-19):
/// `traverse` has no 0.9 path to fall back on.
pub const TRAVERSE_NEEDS_REINDEX: &str =
    "traverse reads the symbol graph, and this branch's index predates it (an older \
     extractor, or no symbol rows): reindex with devctx index --full (vectors are reused)";

/// The error of a failed read of the symbol graph: a database error, not a
/// reason to reindex.
const TRAVERSE_READ_ERROR: &str =
    "could not read the symbol graph of this branch (a read error, not a reason to \
     reindex); retry, and see serve.log if it persists";

/// The line `partial` carries.
const PARTIAL_HINT: &str =
    "the page was full before this depth, so deeper levels were not read and `total` counts \
     only what was: ask for next_offset to read on";

/// The line `excluded` carries.
const EXCLUDED_HINT: &str =
    "symbols of test files and external (library) destinations are left out by default and \
     not walked; pass include_tests / include_external to list them, marked";

/// The line `below_confidence` carries.
const BELOW_CONFIDENCE_HINT: &str =
    "symbols reached only through edges below the confidence shown were left out and not \
     walked (counted: symbols); pass min_confidence: \"low\" to include them, marked";

/// The line `omitted_by_limit` carries.
const DISPATCH_CAPPED_HINT: &str =
    "a method implemented (or overridden) by more methods than a node is expanded into, or in \
     a type more supertype levels away than dispatch climbs (beyond_depth): those were counted, \
     not listed or walked; traverse from one implementation, or pass dispatch: false";

/// `traverse`'s parameters, as the tool, the API and the CLI take them;
/// `None` is the default of DD-16.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TraverseQuery {
    /// Where to start: a bare, qualified or `file::name` symbol, or a file
    /// path (its imports). Every definition it names is a root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// One definition by its `sym` (hex), instead of `symbol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sym: Option<String>,
    /// Relations, comma-separated, or `all` (default `calls`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<String>,
    /// `in`, `out` (default) or `both`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Levels, 1 (default) to 4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    /// `high`, `medium` (default) or `low`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<String>,
    /// List symbols of test files (default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_tests: Option<bool>,
    /// List external destinations (default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_external: Option<bool>,
    /// Follow dispatch through supertypes on `calls` (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<bool>,
    /// Nodes per page (default 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Skip this many nodes (the `next_offset` of the previous page).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

impl TraverseQuery {
    /// `(offset, limit)` of the page.
    fn page(&self) -> (usize, usize) {
        (
            self.offset.unwrap_or(0),
            self.limit.unwrap_or(DEFAULT_TRAVERSE_LIMIT).max(1),
        )
    }

    /// The store's options, or the caller's error — checked before anything
    /// is opened.
    pub fn options(&self) -> Result<TraverseOptions, String> {
        if self.symbol.as_deref().is_none_or(|s| s.trim().is_empty())
            && self.sym.as_deref().is_none_or(|s| s.trim().is_empty())
        {
            return Err("traverse needs a symbol (or a sym)".into());
        }
        let depth = self.depth.unwrap_or(1);
        if depth == 0 || depth > TRAVERSE_MAX_DEPTH {
            return Err(format!(
                "depth must be between 1 and {TRAVERSE_MAX_DEPTH}, not {depth}: traverse reads one \
                 level per hop and a level of references or imports can hold a whole repository; \
                 for a wider blast radius of calls use impact_analysis"
            ));
        }
        let direction = match self.direction.as_deref().map(str::trim) {
            None | Some("") => TraverseDirection::Out,
            Some(d) => TraverseDirection::parse(d).ok_or_else(|| {
                format!("direction must be \"in\", \"out\" or \"both\", not {d:?}")
            })?,
        };
        let (offset, limit) = self.page();
        Ok(TraverseOptions {
            kinds: parse_kinds(self.kinds.as_deref())?,
            direction,
            depth,
            min_confidence: lookup::parse_min_confidence(self.min_confidence.as_deref())?,
            include_tests: self.include_tests.unwrap_or(false),
            include_external: self.include_external.unwrap_or(false),
            dispatch: self.dispatch.unwrap_or(true),
            need: offset.saturating_add(limit),
            // Set from the root's name once it is resolved (`traverse_on`).
            asked: String::new(),
            by_name: false,
        })
    }
}

/// `kinds` (comma-separated, or `all`; default `calls`).
pub fn parse_kinds(s: Option<&str>) -> Result<Vec<&'static str>, String> {
    let Some(s) = s.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(vec!["calls"]);
    };
    let mut out: Vec<&'static str> = Vec::new();
    for k in s.split(',').map(str::trim).filter(|k| !k.is_empty()) {
        if k.eq_ignore_ascii_case("all") {
            return Ok(TRAVERSE_KINDS.to_vec());
        }
        match TRAVERSE_KINDS.iter().find(|r| r.eq_ignore_ascii_case(k)) {
            Some(r) if !out.contains(r) => out.push(*r),
            Some(_) => {}
            None => {
                return Err(format!(
                    "kinds takes {} or \"all\", not {k:?}",
                    TRAVERSE_KINDS.join(", ")
                ))
            }
        }
    }
    Ok(out)
}

fn confidence_name(m: MinConfidence) -> &'static str {
    match m {
        MinConfidence::High => "high",
        MinConfidence::Medium => "medium",
        MinConfidence::Low => "low",
    }
}

/// A symbol as `root` and `candidates` show it.
fn symbol_json(s: &StoredSymbol) -> Value {
    json!({
        "sym": sym_hex(s.id),
        "symbol": s.qualified,
        "kind": s.kind,
        "file": s.file,
        "line": s.start_line,
    })
}

fn node_json(n: &TraverseNode) -> Value {
    let mut v = json!({
        "symbol": n.symbol,
        "depth": n.depth,
        "confidence": n.confidence.as_deref().unwrap_or("low"),
        "via": n.via,
    });
    if let Some(id) = n.id {
        v["sym"] = json!(sym_hex(id));
    }
    if let Some(k) = &n.kind {
        v["kind"] = json!(k);
    }
    if let Some(f) = &n.file {
        v["file"] = json!(f);
    }
    if let Some(l) = n.line {
        v["line"] = json!(l);
    }
    if let Some(t) = &n.through {
        v["through"] = json!(t);
    }
    if n.test {
        v["test"] = json!(true);
    }
    if n.external {
        v["external"] = json!(true);
    }
    if n.undecided() {
        v["undecided"] = json!(true);
    }
    v
}

/// An end of an edge: its `sym`, or its name when it has no definition.
fn end_json(e: &TraverseEnd) -> Value {
    match e.id {
        Some(id) => json!(sym_hex(id)),
        None => json!(e.symbol),
    }
}

fn edge_json(e: &TraverseEdge) -> Value {
    let mut v = json!({
        "from": end_json(&e.from),
        "to": end_json(&e.to),
        "kind": e.kind,
        "confidence": e.confidence.as_deref().unwrap_or("low"),
    });
    if e.dispatch {
        v["via"] = json!(devctx_store::DISPATCH_VIA);
    }
    if let Some(t) = &e.through {
        v["through"] = json!(t);
    }
    if let Some(l) = e.line {
        v["line"] = json!(l);
    }
    if e.undecided {
        v["undecided"] = json!(true);
    }
    if e.occurrences > 1 {
        v["occurrences"] = json!(e.occurrences);
    }
    v
}

/// `items` in order while they fit `budget` tokens (a hard cut, for a list
/// of many small items such as `candidates`); the rest named by `label`.
/// 0 = no budget.
fn fit_prefix(
    items: Vec<Value>,
    budget: usize,
    label: impl Fn(&Value) -> String,
) -> (Vec<Value>, Vec<String>) {
    if budget == 0 {
        return (items, Vec::new());
    }
    let room = budget * super::CHARS_PER_TOKEN;
    let (mut used, mut kept, mut dropped) = (0usize, Vec::new(), Vec::new());
    for v in items {
        let len = serde_json::to_string(&v).map(|s| s.len()).unwrap_or(0);
        if dropped.is_empty() && used + len <= room {
            used += len;
            kept.push(v);
        } else {
            dropped.push(label(&v));
        }
    }
    (kept, dropped)
}

/// `items` under `budget` tokens, with their positions: every one when
/// they all fit (review of TASK-013, MINOR 2: an even share must not drop
/// an item the total had room for), else each within an even share, as
/// `fit_json_array` does; the dropped ones named by `label`. 0 = no budget.
fn fit_whole_or_share(
    items: Vec<Value>,
    budget: usize,
    label: impl Fn(&Value) -> String,
) -> (Vec<(usize, Value)>, Vec<String>) {
    let size = |v: &Value| serde_json::to_string(v).map(|s| s.len()).unwrap_or(0);
    let total: usize = items.iter().map(size).sum();
    let room = budget * super::CHARS_PER_TOKEN;
    if budget == 0 || items.is_empty() || total <= room {
        return (items.into_iter().enumerate().collect(), Vec::new());
    }
    // The floor of `fit_json_array`: a share too small to hold anything is
    // not worth enforcing on a long tail of small items.
    let share = (budget / items.len()).max(64) * super::CHARS_PER_TOKEN;
    let (mut kept, mut dropped) = (Vec::new(), Vec::new());
    for (i, v) in items.into_iter().enumerate() {
        if size(&v) <= share {
            kept.push((i, v));
        } else {
            dropped.push(label(&v));
        }
    }
    (kept, dropped)
}

/// The `traverse` object on a chosen branch, under `budget` tokens.
pub(super) fn traverse_on(
    store: &Store,
    chosen: &BranchChoice,
    q: &TraverseQuery,
    budget: usize,
) -> Result<Value, String> {
    let opts = q.options()?;
    match Reader::of(chosen, store) {
        Reader::SymbolGraph => {}
        Reader::Legacy(Legacy::Error) => return Err(TRAVERSE_READ_ERROR.into()),
        Reader::Legacy(_) => return Err(TRAVERSE_NEEDS_REINDEX.into()),
    }
    let (repo, branch) = (chosen.repo.as_str(), chosen.branch.as_str());
    let asked = q.symbol.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (roots, file, bare): (Vec<StoredSymbol>, Option<&str>, &str) = match q
        .sym
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(hex) => {
            let id = u64::from_str_radix(hex.trim_start_matches("0x"), 16)
                .map_err(|_| format!("sym must be a hex id (as nodes carry it), not {hex:?}"))?;
            let s = store
                    .symbol_by_id(repo, branch, id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        format!("no symbol with sym {hex} on branch {branch}: it may come from another branch or an older index")
                    })?;
            (vec![s], None, asked.unwrap_or(""))
        }
        None => {
            let name = asked.unwrap_or("");
            let (file, bare) = match super::split_file_symbol(name) {
                Some((f, n)) => (Some(f), n),
                None => (None, name),
            };
            // One more than the limit, to know whether it cut.
            let roots = store
                .traverse_roots(repo, branch, bare, file, ROOTS_LIMIT + 1)
                .map_err(|e| e.to_string())?;
            (roots, file, bare)
        }
    };
    let mut roots = roots;
    let mut candidates_dropped: Vec<String> = Vec::new();
    let roots_cut = roots.len() > ROOTS_LIMIT;
    roots.truncate(ROOTS_LIMIT);
    let mut out = json!({});
    out["root"] = match roots.as_slice() {
        [one] => symbol_json(one),
        _ => json!({ "symbol": asked.unwrap_or(bare) }),
    };
    if roots.len() > 1 {
        // An ambiguous name: the walk starts from every definition, and
        // `sym` picks one.
        // Under a quarter of the budget, in order; the rest named.
        let all: Vec<Value> = roots.iter().map(symbol_json).collect();
        let (kept, dropped) = fit_prefix(all, budget / 4, |v| {
            v["symbol"].as_str().unwrap_or("").to_string()
        });
        out["candidates"] = json!(kept);
        candidates_dropped = dropped;
    }
    if roots_cut {
        out["candidates_truncated"] = json!({
            "limit": ROOTS_LIMIT,
            "hint": "the name has more definitions than are walked from: name one (Type.name, \
                     file::name or sym)",
        });
    }
    // `impact_analysis`'s rule on roots: one a bare name stood for, reached
    // from another, is listed.
    // A name with no file also stands for the calls to it the index could
    // not decide (review MAJOR 1); a `sym` or a `file::name` never does.
    let opts = TraverseOptions {
        asked: bare.trim().replace("::", "."),
        by_name: q.sym.as_deref().is_none_or(|s| s.trim().is_empty()) && file.is_none(),
        ..opts
    };
    let t = if roots.is_empty() {
        Traversal::default()
    } else {
        store
            .traverse(repo, branch, &roots, &opts)
            .map_err(|e| e.to_string())?
    };
    let (offset, limit) = q.page();
    let total = t.nodes.len();
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    let shown = start..end;
    let half = budget.saturating_sub(if roots.len() > 1 { budget / 4 } else { 0 }) / 2;
    // The page's nodes under half the budget: all of them when they fit
    // (review of TASK-013, MINOR 2), else by share; only the edges of the
    // nodes actually shown, under the other half.
    let page: Vec<Value> = t.nodes[shown.clone()].iter().map(node_json).collect();
    let (kept, nodes_dropped) = fit_whole_or_share(page, half, |v| {
        v.get("symbol")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string()
    });
    let listed: HashSet<usize> = kept.iter().map(|(i, _)| start + i).collect();
    let nodes: Vec<Value> = kept.into_iter().map(|(_, v)| v).collect();
    let edges: Vec<Value> = t
        .edges
        .iter()
        .filter(|e| listed.contains(&e.reached))
        .map(edge_json)
        .collect();
    let (edges, edges_dropped) =
        fit_whole_or_share(edges, half, |v| format!("{} -> {}", v["from"], v["to"]));
    let edges: Vec<Value> = edges.into_iter().map(|(_, v)| v).collect();
    out["nodes"] = json!(nodes);
    out["edges"] = json!(edges);
    out["total"] = json!(total);
    let left = total - end;
    let next_offset = (left > 0).then_some(end);
    if let Some(n) = next_offset {
        out["next_offset"] = json!(n);
    }
    if let Some(note) = super::omitted_note(left, nodes_dropped.len(), next_offset) {
        out["omitted"] = note;
    }
    if !nodes_dropped.is_empty() || !edges_dropped.is_empty() || !candidates_dropped.is_empty() {
        out["omitted_for_budget"] = json!({
            "count": nodes_dropped.len(),
            "nodes": nodes_dropped,
            "edges": edges_dropped.len(),
        });
        if !candidates_dropped.is_empty() {
            out["omitted_for_budget"]["candidates"] = json!(candidates_dropped);
        }
    }
    if let Some(d) = t.unread_from {
        out["partial"] = json!({ "from_depth": d, "hint": PARTIAL_HINT });
    }
    out["filters"] = json!({
        "kinds": opts.kinds,
        "direction": opts.direction.name(),
        "depth": opts.depth,
        "min_confidence": confidence_name(opts.min_confidence),
        "include_tests": opts.include_tests,
        "include_external": opts.include_external,
        "dispatch": opts.dispatch,
    });
    if opts.kinds.contains(&"calls") {
        if let Some(note) =
            super::impact::dispatch_not_evaluated(opts.dispatch, opts.min_confidence)
        {
            out["dispatch"] = note;
        }
    }
    if t.undecided_calls > 0 {
        out["undecided_calls"] = json!({
            "count": t.undecided_calls,
            "hint": super::impact::UNDECIDED_CALLS_HINT,
        });
    }
    if t.below_confidence > 0 {
        out["below_confidence"] = json!({
            "count": t.below_confidence,
            "min_confidence": confidence_name(opts.min_confidence),
            "hint": BELOW_CONFIDENCE_HINT,
        });
    }
    if t.tests + t.external > 0 {
        out["excluded"] = json!({
            "tests": t.tests,
            "external": t.external,
            "hint": EXCLUDED_HINT,
        });
    }
    let dispatch = t.dispatch_capped + t.dispatch_beyond_depth;
    if dispatch > 0 {
        let mut d = json!({
            "count": dispatch,
            "max_per_node": devctx_store::DISPATCH_MAX_PER_NODE,
        });
        if t.dispatch_beyond_depth > 0 {
            d["beyond_depth"] = json!(t.dispatch_beyond_depth);
            d["max_depth"] = json!(devctx_store::DISPATCH_DEPTH);
        }
        out["omitted_by_limit"] = json!({ "dispatch": d, "hint": DISPATCH_CAPPED_HINT });
    }
    chosen.annotate(&mut out);
    if roots.is_empty() {
        let shown_name = asked.unwrap_or(bare);
        lookup::not_found_hints(store, repo, branch, file, bare, shown_name, &mut out);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::lookup::tests::{indexed, indexed_files};
    use super::super::*;
    use super::TraverseQuery;

    /// A small Java application: an interface and its implementation (one
    /// injected by the interface), a base class, a DTO used as a type and
    /// instantiated, a test, and imports between packages.
    const APP: &[(&str, &str)] = &[
        (
            "src/main/java/app/api/IService.java",
            "package app.api;\n\nimport app.model.OrderDto;\n\npublic interface IService {\n    void update(OrderDto order);\n}\n",
        ),
        (
            "src/main/java/app/core/BaseService.java",
            "package app.core;\n\npublic abstract class BaseService {\n    protected void audit() {\n    }\n}\n",
        ),
        (
            "src/main/java/app/core/ServiceImpl.java",
            "package app.core;\n\nimport app.api.IService;\nimport app.model.OrderDto;\n\npublic class ServiceImpl extends BaseService implements IService {\n    private final Repo repo = new Repo();\n\n    @Override\n    public void update(OrderDto order) {\n        audit();\n        repo.save(order);\n    }\n}\n",
        ),
        (
            "src/main/java/app/core/Repo.java",
            "package app.core;\n\nimport app.model.OrderDto;\n\npublic class Repo {\n    public void save(OrderDto order) {\n    }\n}\n",
        ),
        (
            "src/main/java/app/model/OrderDto.java",
            "package app.model;\n\npublic class OrderDto {\n    private String id;\n}\n",
        ),
        (
            "src/main/java/app/web/Resource.java",
            "package app.web;\n\nimport jakarta.inject.Inject;\nimport app.api.IService;\nimport app.model.OrderDto;\n\npublic class Resource {\n    @Inject\n    IService service;\n\n    public void put() {\n        service.update(new OrderDto());\n    }\n}\n",
        ),
        (
            "src/main/java/app/web/Job.java",
            "package app.web;\n\npublic class Job {\n    private final Resource resource = new Resource();\n\n    public void run() {\n        resource.put();\n    }\n}\n",
        ),
        (
            "src/test/java/app/core/RepoTest.java",
            "package app.core;\n\nimport app.model.OrderDto;\n\npublic class RepoTest {\n    private final Repo repo = new Repo();\n\n    void saves() {\n        repo.save(new OrderDto());\n    }\n}\n",
        ),
    ];

    fn run(state: &AppState, q: &TraverseQuery) -> Value {
        serde_json::from_str(&do_traverse(state, q).unwrap()).unwrap()
    }

    fn q(symbol: &str, kinds: &str, direction: &str, depth: usize) -> TraverseQuery {
        TraverseQuery {
            symbol: Some(symbol.into()),
            kinds: Some(kinds.into()),
            direction: Some(direction.into()),
            depth: Some(depth),
            ..Default::default()
        }
    }

    fn syms(v: &Value) -> Vec<String> {
        v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["symbol"].as_str().unwrap().to_string())
            .collect()
    }

    fn node<'a>(v: &'a Value, symbol: &str) -> &'a Value {
        v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["symbol"] == symbol)
            .unwrap_or_else(|| panic!("no {symbol} in {v}"))
    }

    /// (a) `in` over `calls` at depth 2: the callers and the callers of the
    /// callers — through the interface the caller holds (dispatch, with its
    /// `via` on the node and on the edge) — and the test caller counted.
    #[test]
    fn callers_and_callers_of_callers() {
        let (state, repo) = indexed_files("traverse_calls", APP);
        let v = run(&state, &q("ServiceImpl.update", "calls", "in", 2));
        assert_eq!(syms(&v), ["Resource.put", "Job.run"], "{v}");
        let n = node(&v, "Resource.put");
        assert_eq!(n["depth"], 1, "{v}");
        assert_eq!(n["via"], "dispatch", "{v}");
        assert_eq!(n["through"], "IService.update", "{v}");
        assert_eq!(n["confidence"], "medium", "{v}");
        assert_eq!(node(&v, "Job.run")["depth"], 2, "{v}");
        let root = v["root"]["sym"].as_str().unwrap();
        let e = v["edges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["to"] == root)
            .unwrap_or_else(|| panic!("{v}"));
        assert_eq!(e["from"], n["sym"], "{v}");
        assert_eq!(
            (&e["kind"], &e["via"]),
            (&json!("calls"), &json!("dispatch"))
        );
        assert_eq!(e["line"], 12, "{v}");
        assert_eq!(v["total"], 2, "{v}");
        assert!(
            v.get("next_offset").is_none() && v.get("omitted").is_none(),
            "{v}"
        );
        // Direct callers of the repository: the implementation; the test
        // is counted, and listed on request, marked.
        let v = run(&state, &q("Repo.save", "calls", "in", 1));
        assert_eq!(syms(&v), ["ServiceImpl.update"], "{v}");
        assert_eq!(v["excluded"]["tests"], 1, "{v}");
        let v = run(
            &state,
            &TraverseQuery {
                include_tests: Some(true),
                ..q("Repo.save", "calls", "in", 1)
            },
        );
        assert_eq!(node(&v, "RepoTest.saves")["test"], true, "{v}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// (b), (c) and an example per relation (the ones the tool's description
    /// gives): `contains` out of a class = its members; `inherits` +
    /// `implements` out of it = its supertypes; in = its subtypes; who
    /// instantiates, references and imports a type; what a file imports.
    #[test]
    fn an_example_per_relation() {
        let (state, repo) = indexed_files("traverse_kinds", APP);
        let mut cases: Vec<(&str, &str, &str, Vec<&str>)> = vec![
            (
                "ServiceImpl",
                "contains",
                "out",
                vec!["ServiceImpl.repo", "ServiceImpl.update"],
            ),
            (
                "ServiceImpl",
                "inherits,implements",
                "out",
                vec!["BaseService", "IService"],
            ),
            ("IService", "implements", "in", vec!["ServiceImpl"]),
            ("BaseService", "inherits", "in", vec!["ServiceImpl"]),
            ("OrderDto", "instantiates", "in", vec!["Resource.put"]),
            (
                "OrderDto",
                "references",
                "in",
                vec!["IService.update", "Repo.save", "ServiceImpl.update"],
            ),
            (
                "OrderDto",
                "imports",
                "in",
                vec![
                    "src/main/java/app/api/IService.java",
                    "src/main/java/app/core/Repo.java",
                    "src/main/java/app/core/ServiceImpl.java",
                    "src/main/java/app/web/Resource.java",
                ],
            ),
            (
                "src/main/java/app/core/ServiceImpl.java",
                "imports",
                "out",
                vec!["IService", "OrderDto"],
            ),
            ("Repo.save", "calls", "in", vec!["ServiceImpl.update"]),
        ];
        for (symbol, kinds, dir, want) in cases.drain(..) {
            let v = run(&state, &q(symbol, kinds, dir, 1));
            let mut got = syms(&v);
            got.sort_unstable();
            assert_eq!(got, want, "{symbol} {kinds} {dir}: {v}");
            for n in v["nodes"].as_array().unwrap() {
                assert!(kinds.contains(n["via"].as_str().unwrap()), "{v}");
                assert_eq!(n["sym"].as_str().unwrap().len(), 16, "{v}");
            }
        }
        // Depth 2 up the hierarchy from the implementation's method's class.
        let v = run(&state, &q("ServiceImpl", "inherits,implements", "out", 2));
        assert_eq!(v["filters"]["kinds"], json!(["inherits", "implements"]));
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// (d) An ambiguous name: the candidates, and the walk from all of them;
    /// `sym` walks from one.
    #[test]
    fn an_ambiguous_name_lists_candidates_and_sym_picks_one() {
        let (state, repo) = indexed_files("traverse_ambiguous", APP);
        let v = run(&state, &q("update", "calls", "in", 1));
        let c = v["candidates"].as_array().unwrap();
        let names: Vec<&str> = c.iter().map(|c| c["symbol"].as_str().unwrap()).collect();
        assert_eq!(names, ["IService.update", "ServiceImpl.update"], "{v}");
        assert_eq!(v["root"], json!({ "symbol": "update" }), "{v}");
        assert!(syms(&v).contains(&"Resource.put".to_string()), "{v}");
        let sym = c[1]["sym"].as_str().unwrap().to_string();
        let v = run(
            &state,
            &TraverseQuery {
                sym: Some(sym.clone()),
                kinds: Some("calls".into()),
                direction: Some("out".into()),
                ..Default::default()
            },
        );
        assert!(v.get("candidates").is_none(), "{v}");
        assert_eq!(v["root"]["sym"], sym, "{v}");
        assert_eq!(v["root"]["symbol"], "ServiceImpl.update", "{v}");
        let mut got = syms(&v);
        got.sort_unstable();
        assert_eq!(got, ["BaseService.audit", "Repo.save"], "{v}");
        // An unknown sym, a bad one, and a name that is nothing here.
        for bad in ["00000000000000ff", "zz"] {
            let e = do_traverse(
                &state,
                &TraverseQuery {
                    sym: Some(bad.into()),
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(e.contains("sym"), "{e}");
        }
        let v = run(&state, &q("updat", "calls", "in", 1));
        assert_eq!(v["total"], 0, "{v}");
        assert!(
            v["suggestions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == "update" || s == "ServiceImpl.update" || s == "IService.update"),
            "{v}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Review of TASK-013, MAJOR 1: calls to the name the index could not
    /// decide (`s.flush()` on an untyped receiver, two `flush` in the repo)
    /// are counted by default in `undecided_calls`, and with `low` their
    /// callers are listed at depth 1, `undecided: true`, never walked — as
    /// `impact_analysis` does. Not with `file::name`, nor with `sym`, nor
    /// `out`, nor without `calls`.
    #[test]
    fn undecided_calls_to_the_name_are_counted_or_listed() {
        let (state, repo) = indexed("traverse_undecided", &[]);
        let v = run(&state, &q("flush", "calls", "in", 2));
        assert!(!syms(&v).contains(&"Repo.walk".to_string()), "{v}");
        assert!(v["undecided_calls"]["count"].as_u64().unwrap() >= 1, "{v}");
        assert!(v["undecided_calls"]["hint"]
            .as_str()
            .unwrap()
            .contains("min_confidence"));
        let low = TraverseQuery {
            min_confidence: Some("low".into()),
            ..q("flush", "calls", "in", 2)
        };
        let v = run(&state, &low);
        let n = node(&v, "Repo.walk");
        assert_eq!(n["undecided"], true, "{v}");
        assert_eq!(n["depth"], 1, "{v}");
        assert_eq!(n["confidence"], "low", "{v}");
        assert!(v.get("undecided_calls").is_none(), "{v}");
        let e = v["edges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["from"] == n["sym"])
            .unwrap_or_else(|| panic!("{v}"));
        assert_eq!(e["to"], "flush", "{v}");
        assert_eq!(e["undecided"], true, "{v}");
        // Never with a file, a sym, `out`, or without `calls`.
        for q in [
            TraverseQuery {
                symbol: Some("java/src/main/java/com/example/Left.java::flush".into()),
                ..low.clone()
            },
            TraverseQuery {
                direction: Some("out".into()),
                ..low.clone()
            },
            TraverseQuery {
                kinds: Some("references".into()),
                ..low.clone()
            },
        ] {
            let v = run(&state, &q);
            assert!(!syms(&v).contains(&"Repo.walk".to_string()), "{v}");
            assert!(v.get("undecided_calls").is_none(), "{v}");
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Review of TASK-013, MINOR 1: a name with more definitions than a
    /// handful is walked from all of them (as `impact_analysis`, up to its
    /// seed limit) — 150 `execute`, the caller of the last one is listed —
    /// and the candidates go through the output budget, named when dropped.
    #[test]
    fn every_definition_of_a_name_is_a_root() {
        let mut files: Vec<(String, String)> = Vec::new();
        for i in 0..150 {
            files.push((
                format!("src/main/java/many/Job{i:03}.java"),
                format!("package many;\n\npublic class Job{i:03} {{\n    public void execute() {{\n    }}\n}}\n"),
            ));
        }
        files.push((
            "src/main/java/many/Runner.java".into(),
            "package many;\n\npublic class Runner {\n    private final Job149 job = new Job149();\n\n    public void run() {\n        job.execute();\n    }\n}\n".into(),
        ));
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let (state, repo) = indexed_files("traverse_many_roots", &refs);
        let v = run(&state, &q("execute", "calls", "in", 1));
        assert_eq!(syms(&v), ["Runner.run"], "{}", v["total"]);
        // Every definition is a candidate: listed, or named past the quarter
        // of the budget candidates get.
        let listed = v["candidates"].as_array().unwrap().len();
        let named = v["omitted_for_budget"]["candidates"]
            .as_array()
            .map_or(0, |a| a.len());
        assert_eq!(listed + named, 150, "{listed} + {named}");
        assert!(v.get("candidates_truncated").is_none());
        // Under a small budget the candidates are cut and named, not lost.
        let store = state.open_store().unwrap();
        let chosen = graph_branch(&state, &store).unwrap();
        let v = super::traverse_on(&store, &chosen, &q("execute", "calls", "in", 1), 400).unwrap();
        let kept = v["candidates"].as_array().unwrap().len();
        let dropped = v["omitted_for_budget"]["candidates"]
            .as_array()
            .unwrap()
            .len();
        assert!(kept < 150 && kept + dropped == 150, "{kept} + {dropped}");
        drop(store);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A hub: a DTO referenced from 300 files. (e) `limit`, `offset`,
    /// `next_offset` and `omitted` page it; at depth 2 a full first page
    /// reads no deeper level, and says so (`partial`).
    #[test]
    fn a_hub_is_paged_and_counted() {
        let mut files: Vec<(String, String)> = vec![(
            "src/main/java/hub/Dto.java".into(),
            "package hub;\n\npublic class Dto {\n}\n".into(),
        )];
        for i in 0..300 {
            files.push((
                format!("src/main/java/hub/User{i:03}.java"),
                format!(
                    "package hub;\n\npublic class User{i:03} {{\n    public void take(Dto d) {{\n    }}\n}}\n"
                ),
            ));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let (state, repo) = indexed_files("traverse_hub", &refs);
        let page = |offset: Option<usize>, depth: usize| {
            run(
                &state,
                &TraverseQuery {
                    limit: Some(50),
                    offset,
                    ..q("Dto", "references", "in", depth)
                },
            )
        };
        let v = page(None, 1);
        assert_eq!(v["nodes"].as_array().unwrap().len(), 50, "{}", v["total"]);
        assert_eq!(v["total"], 300);
        assert_eq!(v["next_offset"], 50);
        assert_eq!(
            v["omitted"],
            json!({ "count": 250, "reason": "limit", "next_offset": 50 })
        );
        assert_eq!(v["nodes"][0]["symbol"], "User000.take");
        // Every edge of the page reaches a node of the page.
        let shown: Vec<&Value> = v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| &n["sym"])
            .collect();
        assert!(v["edges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| shown.contains(&&e["from"])));
        assert_eq!(v["edges"].as_array().unwrap().len(), 50);
        let v = page(Some(250), 1);
        assert_eq!(v["nodes"].as_array().unwrap().len(), 50);
        assert_eq!(v["nodes"][0]["symbol"], "User250.take");
        assert!(
            v.get("next_offset").is_none() && v.get("omitted").is_none(),
            "{}",
            v["total"]
        );
        let v = page(None, 2);
        assert_eq!(v["partial"]["from_depth"], 2, "{}", v["partial"]);
        assert_eq!(v["total"], 300);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// (f) A depth over 4 (or 0), an unknown relation or direction, no
    /// symbol: refused with a message, before anything is opened.
    #[test]
    fn bad_parameters_are_refused_with_a_message() {
        for (q, want) in [
            (q("x", "calls", "in", 5), "between 1 and 4"),
            (q("x", "calls", "in", 0), "between 1 and 4"),
            (q("x", "callz", "in", 1), "kinds takes"),
            (q("x", "calls", "up", 1), "direction"),
            (TraverseQuery::default(), "needs a symbol"),
            (
                TraverseQuery {
                    min_confidence: Some("most".into()),
                    ..q("x", "calls", "in", 1)
                },
                "min_confidence",
            ),
        ] {
            let e = q.options().unwrap_err();
            assert!(e.contains(want), "{e}");
        }
        let o = q("x", "", "", 1).options().unwrap();
        assert_eq!(o.kinds, ["calls"]);
        assert_eq!(o.direction, devctx_store::TraverseDirection::Out);
        assert_eq!(o.need, super::DEFAULT_TRAVERSE_LIMIT);
        assert_eq!(q("x", "all", "both", 4).options().unwrap().kinds.len(), 7);
    }

    /// (g) A branch indexed by an older extractor (or with no symbol rows):
    /// an error that asks for `devctx index --full` — traverse has no 0.9
    /// path to answer from.
    #[test]
    fn an_old_index_asks_for_a_full_reindex() {
        let (state, repo) = indexed_files("traverse_stale", APP);
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
        let e = do_traverse(&state, &q("Repo.save", "calls", "in", 1)).unwrap_err();
        assert!(e.contains("devctx index --full"), "{e}");
        let _ = std::fs::remove_dir_all(&repo);
        let (state, repo) = indexed_files(
            "traverse_norows",
            &[("db/schema.sql", "CREATE TABLE things (id INT);\n")],
        );
        let e = do_traverse(&state, &q("things", "calls", "in", 1)).unwrap_err();
        assert!(e.contains("devctx index --full"), "{e}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Dispatch is said not to be evaluated (`calls` followed, `high` or
    /// `dispatch: false`); with another relation the note does not apply.
    #[test]
    fn dispatch_not_evaluated_is_said_on_calls() {
        let (state, repo) = indexed_files("traverse_dispatch_note", APP);
        let v = run(
            &state,
            &TraverseQuery {
                min_confidence: Some("high".into()),
                ..q("ServiceImpl.update", "calls", "in", 1)
            },
        );
        assert!(syms(&v).is_empty(), "{v}");
        assert_eq!(v["dispatch"]["reason"], "min_confidence high", "{v}");
        let v = run(
            &state,
            &TraverseQuery {
                dispatch: Some(false),
                ..q("ServiceImpl.update", "calls", "in", 1)
            },
        );
        assert_eq!(v["dispatch"]["reason"], "dispatch:false", "{v}");
        let v = run(&state, &q("ServiceImpl.update", "references", "out", 1));
        assert!(v.get("dispatch").is_none(), "{v}");
        let v = run(&state, &q("ServiceImpl.update", "calls", "in", 1));
        assert!(v.get("dispatch").is_none(), "{v}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// The output budget: nodes past it are named in `omitted_for_budget`
    /// and counted in `omitted` (`reason: budget`), never dropped silently.
    #[test]
    fn the_budget_names_what_it_drops() {
        // A node larger than the share an item keeps under a budget (a
        // long name in a deep file).
        let long = format!("Use{}", "X".repeat(200));
        let text = format!(
            "package app.web;\n\nimport app.model.OrderDto;\n\nclass {long} {{\n    public void take(OrderDto d) {{\n    }}\n}}\n"
        );
        let path = format!("src/main/java/app/web/{}Long.java", "deep/".repeat(60));
        let mut files = APP.to_vec();
        files.push((path.as_str(), text.as_str()));
        let (state, repo) = indexed_files("traverse_budget", &files);
        let store = state.open_store().unwrap();
        let chosen = graph_branch(&state, &store).unwrap();
        let v = super::traverse_on(&store, &chosen, &q("OrderDto", "all", "in", 1), 100).unwrap();
        let dropped = v["omitted_for_budget"]["count"].as_u64().unwrap();
        assert!(dropped > 0, "{v}");
        assert_eq!(v["omitted"]["reason"], "budget", "{v}");
        assert_eq!(v["omitted"]["count"].as_u64().unwrap(), dropped, "{v}");
        assert_eq!(
            v["nodes"].as_array().unwrap().len() as u64 + dropped,
            v["total"].as_u64().unwrap(),
            "{v}"
        );
        // Review of TASK-013, MINOR 2: no edge hangs off a node the budget
        // dropped.
        let shown: Vec<&Value> = v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| &n["sym"])
            .collect();
        for e in v["edges"].as_array().unwrap() {
            assert!(
                shown.contains(&&e["from"]),
                "{e} hangs off a dropped node: {v}"
            );
        }
        // MINOR 2: when the whole page fits the budget, all of it is shown,
        // even a node larger than an even share of it.
        let all = super::traverse_on(&store, &chosen, &q("OrderDto", "all", "in", 1), 0).unwrap();
        let sizes: Vec<usize> = all["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.to_string().len())
            .collect();
        let total: usize = sizes.iter().sum();
        let budget = 2 * (total / CHARS_PER_TOKEN + 1);
        let share = (budget / 2 / sizes.len()).max(64) * CHARS_PER_TOKEN;
        assert!(
            sizes.iter().any(|&n| n > share),
            "the fixture needs a large node"
        );
        let v =
            super::traverse_on(&store, &chosen, &q("OrderDto", "all", "in", 1), budget).unwrap();
        assert_eq!(v["nodes"], all["nodes"], "{v}");
        drop(store);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Measurement harness (PLAN-009 TASK-013, not a test): indexes
    /// `$DEVCTX_TRAVERSE_BENCH_REPO` into `$DEVCTX_TRAVERSE_BENCH_STATE` with
    /// the model-free embedder, then times the tool (each call opens the
    /// store, as the serve does) at depth 1 and 2 for each line of
    /// `$DEVCTX_TRAVERSE_BENCH_CASES` (`symbol|kinds|direction`), over
    /// `$DEVCTX_TRAVERSE_BENCH_RUNS` runs (40) in `$DEVCTX_TRAVERSE_BENCH_ROUNDS`
    /// interleaved rounds (2): every case and depth once per run, so load
    /// spreads over all of them. One JSON line per round, case and depth.
    ///
    /// `cargo test --release -p devctx-mcp --lib traverse_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn traverse_bench() {
        use super::super::lookup::tests::Fake;
        let var = |k: &str| std::env::var(k).ok();
        let (Some(repo), Some(state_dir), Some(cases)) = (
            var("DEVCTX_TRAVERSE_BENCH_REPO"),
            var("DEVCTX_TRAVERSE_BENCH_STATE"),
            var("DEVCTX_TRAVERSE_BENCH_CASES"),
        ) else {
            eprintln!("set DEVCTX_TRAVERSE_BENCH_REPO, _STATE and _CASES");
            return;
        };
        let num = |k: &str, d: usize| var(k).and_then(|r| r.parse().ok()).unwrap_or(d);
        let runs = num("DEVCTX_TRAVERSE_BENCH_RUNS", 40);
        let rounds = num("DEVCTX_TRAVERSE_BENCH_ROUNDS", 2);
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
        let mut queries: Vec<(String, TraverseQuery)> = Vec::new();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut f = line.split('|').map(str::trim);
            let (symbol, kinds, dir) = (f.next().unwrap(), f.next(), f.next());
            for depth in [1, 2] {
                queries.push((
                    format!("{line}|d{depth}"),
                    TraverseQuery {
                        symbol: Some(symbol.into()),
                        kinds: kinds.map(str::to_string),
                        direction: dir.map(str::to_string),
                        depth: Some(depth),
                        ..Default::default()
                    },
                ));
            }
        }
        // Warm the store and the OS cache, and keep each answer's size.
        let sizes: Vec<Value> = queries
            .iter()
            .map(|(_, q)| {
                let v = run(&state, q);
                json!({ "total": v["total"], "nodes": v["nodes"].as_array().map(|a| a.len()),
                        "edges": v["edges"].as_array().map(|a| a.len()),
                        "partial": v.get("partial").is_some(),
                        "candidates": v["candidates"].as_array().map(|a| a.len()) })
            })
            .collect();
        for round in 1..=rounds {
            let mut ms: Vec<Vec<f64>> = vec![Vec::new(); queries.len()];
            for _ in 0..runs {
                for (i, (_, q)) in queries.iter().enumerate() {
                    let t = Instant::now();
                    do_traverse(&state, q).unwrap();
                    ms[i].push(t.elapsed().as_secs_f64() * 1000.0);
                }
            }
            for (i, (label, _)) in queries.iter().enumerate() {
                let m = &mut ms[i];
                m.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let pct = |p: f64| m[((m.len() as f64 - 1.0) * p).round() as usize];
                println!(
                    "{}",
                    json!({ "round": round, "case": label, "p50_ms": pct(0.5),
                            "p95_ms": pct(0.95), "size": sizes[i] })
                );
            }
        }
    }
}
