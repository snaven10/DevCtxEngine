//! `impact_analysis` (PLAN-009 TASK-009, DD-11): the blast radius of a
//! symbol over the symbol graph — by id, one query per level and direction,
//! capped before the next level, ordered by confidence, rank and name, with
//! tests and externals left out by default and counted — or, on a branch the
//! current extractor has not indexed, the 0.9.0 walk over `graph_edges` with
//! the warning that says so (DD-19).
//!
//! The JSON of 0.9.0 is a subset of this one: `symbol`, `upstream` and
//! `downstream` (each node `{symbol, depth}`), `resolved_symbols`,
//! `branch_fallback`, `warning`, `omitted`/`omitted_for_budget` (half the
//! budget per direction) keep their meaning; nodes add `sym`, `file`,
//! `line`, `confidence`, `via` and `test`/`external`/`undecided` when true,
//! and the answer adds `below_confidence` (symbols), `undecided_calls`
//! (calls to the name the index could not decide), `excluded` and
//! `omitted_by_limit`. A node reached through dispatch (TASK-017: the
//! callers of a method an implementation overrides, the implementations of
//! an interface method) has `via: "dispatch"`, never `high`, and `through`
//! (the method it went through); the methods past the dispatch cap of a node
//! are counted in `omitted_by_limit.dispatch` only (not in its `count`
//! nor in `omitted`, which count nodes);
//! for a name with no definition here, `read_symbol`'s `external` +
//! `called_from` + `next_step` (its upstream is the callers of its external
//! call sites) or `suggestions`.

use devctx_store::{sym_hex, ImpactNode, ImpactOptions, MinConfidence, Store, SymbolImpact};
use serde_json::{json, Value};

use super::lookup::{self, Reader};
use super::BranchChoice;

/// The filters of `impact_analysis` as the tool, the API and the CLI take
/// them; `None` is the default of DD-11 (Q-2 of the plan).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImpactQuery {
    /// `high`, `medium` (default) or `low`.
    pub min_confidence: Option<String>,
    /// List symbols of test files (default false: counted only).
    pub include_tests: Option<bool>,
    /// List external destinations (default false: counted only).
    pub include_external: Option<bool>,
    /// Nodes per direction (default 200; 0 = no cap).
    pub max_nodes: Option<usize>,
    /// Follow dispatch through supertypes (default true, TASK-017).
    pub dispatch: Option<bool>,
}

impl ImpactQuery {
    /// The store's options for a walk of `depth` levels.
    pub fn options(&self, depth: usize) -> Result<ImpactOptions, String> {
        Ok(ImpactOptions {
            depth,
            min_confidence: lookup::parse_min_confidence(self.min_confidence.as_deref())?,
            include_tests: self.include_tests.unwrap_or(false),
            include_external: self.include_external.unwrap_or(false),
            max_nodes: self
                .max_nodes
                .unwrap_or(devctx_store::DEFAULT_IMPACT_MAX_NODES),
            dispatch: self.dispatch.unwrap_or(true),
        })
    }

    /// `key=value` pairs of the filters that were given, for a query string
    /// (values not yet URL-encoded).
    pub fn query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if let Some(m) = &self.min_confidence {
            out.push(("min_confidence", m.clone()));
        }
        if let Some(t) = self.include_tests {
            out.push(("include_tests", t.to_string()));
        }
        if let Some(e) = self.include_external {
            out.push(("include_external", e.to_string()));
        }
        if let Some(n) = self.max_nodes {
            out.push(("max_nodes", n.to_string()));
        }
        if let Some(d) = self.dispatch {
            out.push(("dispatch", d.to_string()));
        }
        out
    }
}

/// The line the answer adds when `min_confidence` left nodes out.
const BELOW_CONFIDENCE_HINT: &str =
    "symbols reached only through calls below the confidence shown were left out and not \
     walked (counted: symbols); pass min_confidence: \"low\" to include them, marked";

/// The line the answer adds when calls to the name were left undecided.
const UNDECIDED_CALLS_HINT: &str =
    "calls written with this name whose receiver the index could not type, so they may or \
     may not reach it (counted: calls, not from tests nor from callers already listed); pass \
     min_confidence: \"low\" to list their callers, marked undecided and not walked";

/// The line the answer adds when tests or externals were left out.
const EXCLUDED_HINT: &str =
    "symbols of test files and external (library) callees are left out by default and \
     not walked; pass include_tests / include_external to list them, marked";

/// The line `omitted_by_limit.dispatch` carries.
const DISPATCH_CAPPED_HINT: &str =
    "a method implemented (or overridden) by more methods than a node is expanded into: \
     the ones past the cap were counted, not listed or walked; ask about one implementation, \
     or pass dispatch: false to follow direct calls only";

/// The line the answer adds when `max_nodes` stopped the walk.
const CAPPED_HINT: &str =
    "max_nodes was reached: the symbols of that depth past it were counted, not listed or \
     walked, and no deeper level was read; raise max_nodes (0 = no cap) or ask about a \
     narrower symbol";

fn confidence_name(m: MinConfidence) -> &'static str {
    match m {
        MinConfidence::High => "high",
        MinConfidence::Medium => "medium",
        MinConfidence::Low => "low",
    }
}

/// One node of the answer: 0.9.0's `{symbol, depth}` plus `confidence`,
/// `via`, and `sym`/`file`/`line` when it has a definition here and
/// `test`/`external`/`undecided` when true.
fn node_json(n: &ImpactNode) -> Value {
    let mut v = json!({
        "symbol": n.symbol,
        "depth": n.depth,
        "confidence": n.confidence.as_deref().unwrap_or("low"),
        "via": n.via,
    });
    if let Some(id) = n.id {
        v["sym"] = json!(sym_hex(id));
    }
    if let Some(f) = &n.file {
        v["file"] = json!(f);
    }
    if let Some(l) = n.line {
        v["line"] = json!(l);
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
    if let Some(t) = &n.through {
        v["through"] = json!(t);
    }
    v
}

/// `name` split into an optional file and the symbol part (`src/x.rs::f`).
fn file_and_name(name: &str) -> (Option<&str>, &str) {
    match super::split_file_symbol(name) {
        Some((f, n)) => (Some(f), n),
        None => (None, name),
    }
}

/// The `impact_analysis` object on a chosen branch, under `budget` tokens.
pub(super) fn impact_on(
    store: &Store,
    chosen: &BranchChoice,
    symbol: &str,
    depth: usize,
    q: &ImpactQuery,
    budget: usize,
) -> Result<Value, String> {
    let opts = q.options(depth)?;
    let (repo, branch) = (chosen.repo.as_str(), chosen.branch.as_str());
    let reader = Reader::of(chosen, store);
    if reader == Reader::SymbolGraph {
        let (file, bare) = file_and_name(symbol);
        let im = store
            .impact_graph(repo, branch, bare, file, &opts)
            .map_err(|e| e.to_string())?;
        let up = im.upstream.nodes.iter().map(node_json).collect();
        let down = im.downstream.nodes.iter().map(node_json).collect();
        // What the name became is compared with the name itself, without
        // the file of `file::name` and with `::` folded (review m6).
        let asked = bare.trim().replace("::", ".");
        let mut out = answer(
            symbol,
            &asked,
            up,
            down,
            &im.resolved,
            chosen,
            reader,
            Some((&im, &opts)),
            budget,
        );
        // No definition here: say whether it is a library's (its call
        // sites' callers are the upstream, `external` with `called_from`, as
        // `read_symbol` says it) or what it could have been meant as.
        if im.resolved.is_empty() {
            lookup::not_found_hints(store, repo, branch, file, bare, symbol, &mut out);
        }
        return Ok(out);
    }
    // The 0.9.0 walk, unchanged: no confidence, no marks, no cap.
    let resolved = store
        .resolve_symbol(repo, branch, symbol)
        .map_err(|e| e.to_string())?;
    let impact = store
        .impact_analysis(repo, branch, symbol, depth)
        .map_err(|e| e.to_string())?;
    let to_json = |v: &[(String, usize)]| -> Vec<Value> {
        v.iter()
            .map(|(s, d)| json!({ "symbol": s, "depth": d }))
            .collect()
    };
    let mut out = answer(
        symbol,
        symbol,
        to_json(&impact.upstream),
        to_json(&impact.downstream),
        &resolved,
        chosen,
        reader,
        None,
        budget,
    );
    // Filters were asked for and this path has none: say so, as a remote
    // client does for a server that ignored them (review NIT 3).
    if *q != ImpactQuery::default() {
        crate::backend::add_unapplied_impact_note(&mut out);
    }
    Ok(out)
}

/// The answer object, the same on both paths: half the budget per
/// direction (a huge upstream must not starve a small downstream),
/// `resolved_symbols` when a name stood for other or several declarations,
/// the branch and reader warnings, and what was left out — by the budget
/// (`omitted_for_budget`, named), by `max_nodes` (`omitted_by_limit`,
/// counted), by the filters (`below_confidence`, `excluded`) — with
/// `omitted.count` the sum of the first two.
#[allow(clippy::too_many_arguments)]
fn answer(
    symbol: &str,
    asked: &str,
    up: Vec<Value>,
    down: Vec<Value>,
    resolved: &[String],
    chosen: &BranchChoice,
    reader: Reader,
    graph: Option<(&SymbolImpact, &ImpactOptions)>,
    budget: usize,
) -> Value {
    let half = budget / 2;
    let label = |v: &Value| {
        v.get("symbol")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string()
    };
    let (upstream, up_dropped) = super::fit_json_array(up, half, None, label);
    let (downstream, down_dropped) = super::fit_json_array(down, half, None, label);
    let mut out = json!({
        "symbol": symbol,
        "upstream": upstream,
        "downstream": downstream,
    });
    // A bare name can stand for several methods, and the radius merges
    // them. Say so: an unannounced merge reads as one method with a wide
    // blast radius, which is a different and much more alarming fact.
    if let Some(names) = super::merged_declarations(asked, resolved) {
        out["resolved_symbols"] = json!(names);
    }
    chosen.annotate(&mut out);
    reader.annotate(&mut out);
    let mut by_limit = 0;
    if let Some((im, opts)) = graph {
        // The filters this answer applied: a client that asked for them
        // and finds none knows the server (or its index) ignored them.
        out["filters"] = json!({
            "min_confidence": confidence_name(opts.min_confidence),
            "include_tests": opts.include_tests,
            "include_external": opts.include_external,
            "max_nodes": opts.max_nodes,
            "dispatch": opts.dispatch,
        });
        let (u, d) = (&im.upstream, &im.downstream);
        let below = u.below_confidence + d.below_confidence;
        if below > 0 {
            out["below_confidence"] = json!({
                "count": below,
                "min_confidence": confidence_name(opts.min_confidence),
                "hint": BELOW_CONFIDENCE_HINT,
            });
        }
        if im.undecided_calls > 0 {
            out["undecided_calls"] = json!({
                "count": im.undecided_calls,
                "hint": UNDECIDED_CALLS_HINT,
            });
        }
        let (tests, external) = (u.tests + d.tests, u.external + d.external);
        if tests + external > 0 {
            out["excluded"] = json!({
                "tests": tests,
                "external": external,
                "hint": EXCLUDED_HINT,
            });
        }
        // The dispatch cap counts methods, not nodes: only in its own field
        // (review of TASK-017, deviation 6), never in `count` nor `omitted`.
        let dispatch = u.dispatch_capped + d.dispatch_capped;
        by_limit = u.capped + d.capped;
        if by_limit + dispatch > 0 {
            // The hint of what cut: `max_nodes`, or only the dispatch cap.
            let hint = if u.capped + d.capped > 0 {
                CAPPED_HINT
            } else {
                DISPATCH_CAPPED_HINT
            };
            let mut note = json!({
                "count": by_limit,
                "max_nodes": opts.max_nodes,
                "hint": hint,
            });
            for (name, side) in [("upstream", u), ("downstream", d)] {
                if let Some(at) = side.capped_at {
                    note[name] = json!({ "count": side.capped, "depth": at });
                }
            }
            // The dispatch cap, apart: methods a node was not expanded into.
            if dispatch > 0 {
                note["dispatch"] = json!({
                    "count": dispatch,
                    "max_per_node": devctx_store::DISPATCH_MAX_PER_NODE,
                });
            }
            out["omitted_by_limit"] = note;
        }
    }
    let by_budget = up_dropped.len() + down_dropped.len();
    if by_budget > 0 {
        out["omitted_for_budget"] = json!({
            "count": by_budget,
            "upstream": up_dropped,
            "downstream": down_dropped,
        });
    }
    if let Some(note) = super::omitted_note(by_limit, by_budget, None) {
        out["omitted"] = note;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::lookup::tests::{indexed, indexed_files};
    use super::super::*;
    use super::{answer, ImpactQuery};
    use devctx_store::{ImpactOptions, ImpactSide, SymbolImpact};

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn impact(state: &AppState, symbol: &str, q: &ImpactQuery) -> Value {
        serde_json::from_str(&do_impact_with(state, symbol, 3, q).unwrap()).unwrap()
    }

    fn all_in() -> ImpactQuery {
        ImpactQuery {
            min_confidence: Some("low".into()),
            include_tests: Some(true),
            include_external: Some(true),
            max_nodes: Some(0),
            ..Default::default()
        }
    }

    fn syms(v: &Value, side: &str) -> Vec<String> {
        v[side]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["symbol"].as_str().unwrap().to_string())
            .collect()
    }

    fn node<'a>(v: &'a Value, side: &str, symbol: &str) -> &'a Value {
        v[side]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["symbol"] == symbol)
            .unwrap_or_else(|| panic!("no {symbol} in {side}: {v}"))
    }

    /// A Java service: a resource calls it through a field, a test calls it,
    /// it calls a repository method and a library.
    const JAVA: &[(&str, &str)] = &[
        (
            "src/main/java/app/Api.java",
            "package app;\n\npublic class Api {\n    private final Service service = new Service();\n\n    public void handle() {\n        service.update();\n    }\n}\n",
        ),
        (
            "src/main/java/app/Service.java",
            "package app;\n\nimport com.fasterxml.jackson.databind.ObjectMapper;\n\npublic class Service {\n    private final Store store = new Store();\n    private final ObjectMapper mapper = new ObjectMapper();\n\n    public void update() {\n        store.save();\n        mapper.writeValueAsString(this);\n    }\n}\n",
        ),
        (
            "src/main/java/app/Store.java",
            "package app;\n\npublic class Store {\n    public void save() {\n    }\n}\n",
        ),
        (
            "src/test/java/app/ServiceTest.java",
            "package app;\n\npublic class ServiceTest {\n    private final Service service = new Service();\n\n    void updates() {\n        service.update();\n    }\n}\n",
        ),
    ];

    /// The new path: nodes by id with `sym`, `file`, `line`, `confidence`,
    /// `via`; tests and externals counted by default (c), listed and marked
    /// on request; 0.9.0's answer is a subset of the new one.
    #[test]
    fn impact_over_the_symbol_graph_marks_and_counts() {
        let (state, repo) = indexed_files("impact_java", JAVA);
        let v = impact(&state, "update", &ImpactQuery::default());
        assert!(v.get("warning").is_none(), "{v}");
        assert_eq!(syms(&v, "upstream"), ["Api.handle"], "{v}");
        let n = node(&v, "upstream", "Api.handle");
        assert_eq!(n["depth"], 1, "{v}");
        assert_eq!(n["confidence"], "high", "{v}");
        assert_eq!(n["via"], "calls", "{v}");
        assert_eq!(n["file"], "src/main/java/app/Api.java", "{v}");
        assert_eq!(n["line"], 6, "{v}");
        assert_eq!(n["sym"].as_str().unwrap().len(), 16, "{v}");
        assert!(
            n.get("test").is_none() && n.get("external").is_none(),
            "{v}"
        );
        assert_eq!(syms(&v, "downstream"), ["Store.save"], "{v}");
        assert_eq!(v["excluded"]["tests"], 1, "{v}");
        assert!(v["excluded"]["external"].as_u64().unwrap() >= 1, "{v}");
        assert!(v["excluded"]["hint"]
            .as_str()
            .unwrap()
            .contains("include_tests"));
        assert_eq!(v["resolved_symbols"], json!(["Service.update"]), "{v}");

        let all = impact(
            &state,
            "update",
            &ImpactQuery {
                include_tests: Some(true),
                include_external: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(node(&all, "upstream", "ServiceTest.updates")["test"], true);
        let ext: Vec<&Value> = all["downstream"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["external"] == true)
            .collect();
        assert!(!ext.is_empty(), "{all}");
        assert!(
            ext.iter()
                .all(|n| n.get("sym").is_none() && n.get("file").is_none()),
            "{all}"
        );
        assert!(all.get("excluded").is_none(), "{all}");

        // Requirement 2: what 0.9.0 answered (the same index stamped by
        // another extractor) is a subset of the new answer with the filters
        // off — every key, and every upstream node at its depth.
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
        let old = impact(&state, "update", &ImpactQuery::default());
        {
            let store = state.open_store().unwrap();
            let (_, b) = state.repo_branch().unwrap();
            store
                .set_index_meta(
                    &state.repo_path(),
                    &b,
                    devctx_store::EXTRACTOR_META_KEY,
                    &devctx_index::extractor_fingerprint(),
                )
                .unwrap();
        }
        let new = impact(&state, "update", &all_in());
        for key in old.as_object().unwrap().keys() {
            if key != "warning" {
                assert!(new.get(key).is_some(), "{key}: {old} vs {new}");
            }
        }
        assert_eq!(old["resolved_symbols"], new["resolved_symbols"]);
        for side in ["upstream", "downstream"] {
            for n in old[side].as_array().unwrap() {
                let m = node(&new, side, n["symbol"].as_str().unwrap());
                assert_eq!(m["depth"], n["depth"], "{side}: {old} vs {new}");
            }
            assert!(!old[side].as_array().unwrap().is_empty(), "{side}: {old}");
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A name with no definition: a library's (`external`, `called_from`,
    /// the callers of its call sites upstream), or a slip (`suggestions`).
    #[test]
    fn a_name_without_definition_says_what_it_is() {
        let (state, repo) = indexed_files("impact_nodef", JAVA);
        let v = impact(&state, "writeValueAsString", &ImpactQuery::default());
        assert_eq!(v["external"], true, "{v}");
        assert!(v["called_from"].as_u64().unwrap() >= 1, "{v}");
        assert!(v["next_step"].is_string(), "{v}");
        assert_eq!(node(&v, "upstream", "Service.update")["depth"], 1, "{v}");
        assert_eq!(node(&v, "upstream", "Api.handle")["depth"], 2, "{v}");
        assert!(v["downstream"].as_array().unwrap().is_empty(), "{v}");
        assert!(v.get("resolved_symbols").is_none(), "{v}");
        let v = impact(&state, "updat", &ImpactQuery::default());
        assert!(v.get("external").is_none(), "{v}");
        assert!(
            v["suggestions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == "Service.update"),
            "{v}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Review M1: an undecided call to the name (`s.flush()` on an untyped
    /// receiver, two `flush` in the repo) is counted by default (in
    /// `undecided_calls`, review m-a) and its
    /// caller listed with `low`, marked, as `get_references` does.
    #[test]
    fn an_undecided_call_to_the_name_is_counted_or_listed() {
        let (state, repo) = indexed("impact_undecided", &[]);
        let v = impact(&state, "flush", &ImpactQuery::default());
        assert!(
            !syms(&v, "upstream").iter().any(|s| s == "Repo.walk"),
            "{v}"
        );
        assert!(v["undecided_calls"]["count"].as_u64().unwrap() >= 1, "{v}");
        // m-a: the calls are not symbols below the confidence shown.
        assert!(v.get("below_confidence").is_none(), "{v}");
        assert!(v["undecided_calls"]["hint"]
            .as_str()
            .unwrap()
            .contains("min_confidence"));
        let v = impact(
            &state,
            "flush",
            &ImpactQuery {
                min_confidence: Some("low".into()),
                ..Default::default()
            },
        );
        let n = node(&v, "upstream", "Repo.walk");
        assert_eq!(n["undecided"], true, "{v}");
        assert_eq!(n["confidence"], "low", "{v}");
        assert_eq!(n["depth"], 1, "{v}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Review m6: `file::name` that designates exactly the name is no
    /// expansion, and neither is `Type::m` for `Type.m`; the answer of the
    /// symbol graph says which filters it applied.
    #[test]
    fn a_file_qualified_name_is_no_expansion() {
        let (state, repo) = indexed("impact_m6", &[]);
        let v = impact(&state, "py/app/svc.py::helper", &ImpactQuery::default());
        assert!(v.get("resolved_symbols").is_none(), "{v}");
        assert!(!v["upstream"].as_array().unwrap().is_empty(), "{v}");
        let v = impact(&state, "Thing::new", &ImpactQuery::default());
        assert!(v.get("resolved_symbols").is_none(), "{v}");
        let v = impact(&state, "new", &ImpactQuery::default());
        assert!(
            v["resolved_symbols"].is_array(),
            "a bare name still expands: {v}"
        );
        assert_eq!(v["filters"]["min_confidence"], "medium", "{v}");
        assert_eq!(v["filters"]["max_nodes"], 200, "{v}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Requirement 1: a discarded call reaches no impact. The fixture's
    /// discarded `fetchRows` row is also marked external and pointed at
    /// `Left.flush` (as a reader of `edges` would see it): neither side,
    /// with every filter off, may show it.
    #[test]
    fn discarded_calls_reach_no_impact() {
        let (state, repo) = indexed("impact_discarded", &[]);
        {
            let store = state.open_store().unwrap();
            let (r, b) = state.repo_branch().unwrap();
            let file = "java/src/main/java/com/example/Repo.java";
            let left = store.lookup_symbols(&r, &b, "Left.flush", None, 1).unwrap()[0].id;
            let mut rows = store.file_symbol_edges(&r, &b, file).unwrap();
            let mut found = false;
            for e in rows
                .iter_mut()
                .filter(|e| e.resolution.as_deref() == Some("discarded"))
            {
                e.external = Some(true);
                e.dst_id = Some(left);
                e.confidence = Some("high".into());
                found = true;
            }
            assert!(found, "the fixture must hold a discarded row");
            store
                .replace_file_symbol_edges(&r, &b, file, &rows)
                .unwrap();
        }
        let v = impact(&state, "Repo.walk", &all_in());
        assert!(
            !syms(&v, "downstream")
                .iter()
                .any(|s| s == "fetchRows" || s == "Left.flush"),
            "{v}"
        );
        let v = impact(&state, "Left.flush", &all_in());
        assert!(
            !syms(&v, "upstream").iter().any(|s| s == "Repo.walk"),
            "{v}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// (b) through the tool: 1 000 callers, `max_nodes` 200 → 200 listed,
    /// `omitted.count` 800 (`reason: limit`), and where it stopped.
    #[test]
    fn the_tool_caps_at_max_nodes_and_counts_the_rest() {
        let mut py = String::from("def helper():\n    return 1\n");
        for i in 0..1000 {
            py.push_str(&format!("\n\ndef caller_{i:04}():\n    helper()\n"));
        }
        let (state, repo) = indexed_files("impact_cap", &[("app/many.py", py.as_str())]);
        let v = impact(&state, "helper", &ImpactQuery::default());
        assert_eq!(
            v["upstream"].as_array().unwrap().len(),
            200,
            "{}",
            v["omitted"]
        );
        assert_eq!(v["omitted"]["count"], 800, "{}", v["omitted"]);
        assert_eq!(v["omitted"]["reason"], "limit");
        assert_eq!(v["omitted_by_limit"]["upstream"]["count"], 800);
        assert_eq!(v["omitted_by_limit"]["upstream"]["depth"], 1);
        assert_eq!(v["omitted_by_limit"]["max_nodes"], 200);
        assert!(v.get("omitted_for_budget").is_none());
        assert_eq!(v["upstream"][0]["symbol"], "caller_0000");
        let v = impact(
            &state,
            "helper",
            &ImpactQuery {
                max_nodes: Some(0),
                ..Default::default()
            },
        );
        assert!(v.get("omitted_by_limit").is_none());
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Requirement 3: a stale index answers with the 0.9.0 walk (no
    /// confidence, no cap) and the stale warning; a current branch with no
    /// symbol rows warns too. Never a silent empty answer.
    #[test]
    fn a_stale_index_walks_as_0_9_0_and_says_so() {
        let (state, repo) = indexed_files("impact_stale", JAVA);
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
        let v = impact(&state, "Service.update", &ImpactQuery::default());
        assert!(
            v["warning"].as_str().unwrap().contains("older extractor"),
            "{v}"
        );
        let up = v["upstream"].as_array().unwrap();
        assert!(!up.is_empty(), "{v}");
        // 0.9.0 had no filters: the test caller is there, unmarked.
        assert!(
            up.iter().any(|n| n["symbol"] == "ServiceTest.updates"),
            "{v}"
        );
        for n in up.iter().chain(v["downstream"].as_array().unwrap()) {
            let keys: Vec<&String> = n.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["depth", "symbol"], "{v}");
        }
        assert!(v.get("excluded").is_none() && v.get("below_confidence").is_none());
        let _ = std::fs::remove_dir_all(&repo);

        let (state, repo) = indexed_files(
            "impact_norows",
            &[("db/schema.sql", "CREATE TABLE things (id INT);\n")],
        );
        let v = impact(&state, "things", &ImpactQuery::default());
        assert!(
            v["warning"]
                .as_str()
                .unwrap()
                .contains("no rows in the symbol graph"),
            "{v}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Review of TASK-009, NIT 3: on the old path (a stale index) the
    /// filters cannot apply; a local backend that was given some says so in
    /// `warning`, as a remote one does, and only once; without filters it
    /// says nothing of the kind.
    #[test]
    fn a_stale_index_says_the_filters_were_not_applied() {
        let (state, repo) = indexed_files("impact_stale_filters", JAVA);
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
        let backend = crate::backend::Backend::Local(std::sync::Arc::new(state));
        let q = ImpactQuery {
            include_tests: Some(false),
            ..Default::default()
        };
        let v: Value =
            serde_json::from_str(&backend.impact("Service.update", 3, &q).unwrap()).unwrap();
        let w = v["warning"].as_str().unwrap();
        assert!(w.contains("older extractor"), "{v}");
        assert_eq!(w.matches("not applied").count(), 1, "{v}");
        assert!(v.get("filters").is_none(), "{v}");
        // A remote client noting the same answer again adds nothing.
        let again = crate::backend::note_unapplied_impact_filters(v.to_string());
        let again: Value = serde_json::from_str(&again).unwrap();
        assert_eq!(again["warning"], v["warning"]);
        let v: Value = serde_json::from_str(
            &backend
                .impact("Service.update", 3, &ImpactQuery::default())
                .unwrap(),
        )
        .unwrap();
        assert!(
            !v["warning"].as_str().unwrap().contains("not applied"),
            "{v}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Review of TASK-010, MINOR 1: a search without a server applies
    /// centrality under the MCP's rule — a current symbol graph — so a branch
    /// stamped by another extractor gets none, whatever ranks survive in it.
    #[test]
    fn local_centrality_follows_the_symbol_graph_rule() {
        let (state, repo) = indexed_files("centrality_local", JAVA);
        let store = state.open_store().unwrap();
        assert_eq!(local_centrality(&store, &repo, None, 0.4), 0.4);
        let (_, b) = state.repo_branch().unwrap();
        store
            .set_index_meta(
                &state.repo_path(),
                &b,
                devctx_store::EXTRACTOR_META_KEY,
                "v0-old",
            )
            .unwrap();
        assert_eq!(local_centrality(&store, &repo, None, 0.4), 0.0);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// The fallback of `a_branch_nobody_indexed_answers_the_graph_from_the_indexed_one`
    /// over the symbol graph: an unindexed branch answers from `main`, by
    /// id, and says so.
    #[test]
    fn impact_branch_fallback_over_the_symbol_graph() {
        let (state, repo) = indexed_files("impact_fallback", JAVA);
        git(&repo, &["checkout", "-q", "-b", "feat/x"]);
        let v = impact(&state, "update", &ImpactQuery::default());
        assert_eq!(v["branch_fallback"]["current"], "feat/x", "{v}");
        assert_eq!(v["branch_fallback"]["used"], "main", "{v}");
        assert!(v["branch_fallback"]["why"]
            .as_str()
            .unwrap()
            .contains("feat/x is not indexed"));
        assert!(!v["upstream"].as_array().unwrap().is_empty(), "{v}");
        assert!(v["upstream"][0]["sym"].is_string(), "{v}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// `omitted.count` adds what the budget dropped (named in
    /// `omitted_for_budget`, half the budget per direction as in 0.9.0) and
    /// what `max_nodes` cut (counted in `omitted_by_limit`); the budget is
    /// the reason when it cut anything.
    #[test]
    fn omitted_adds_the_budget_and_the_cap() {
        let chosen = BranchChoice {
            repo: "demo".into(),
            branch: "main".into(),
            fallback: None,
            indexed: true,
            extractor_stale: false,
        };
        let up = vec![
            json!({ "symbol": "Small.a", "depth": 1 }),
            json!({ "symbol": "x".repeat(4000), "depth": 1 }),
        ];
        let down = vec![json!({ "symbol": "Small.b", "depth": 1 })];
        let im = SymbolImpact {
            upstream: ImpactSide {
                capped: 3,
                capped_at: Some(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let opts = ImpactOptions::default();
        let v = answer(
            "Core.run",
            "Core.run",
            up,
            down,
            &["Core.run".to_string()],
            &chosen,
            super::super::lookup::Reader::SymbolGraph,
            Some((&im, &opts)),
            200,
        );
        assert_eq!(v["omitted"]["count"], 4, "{v}");
        assert_eq!(v["omitted"]["reason"], "budget", "{v}");
        assert_eq!(v["omitted_for_budget"]["count"], 1, "{v}");
        assert_eq!(v["omitted_by_limit"]["count"], 3, "{v}");
        assert_eq!(v["downstream"].as_array().unwrap().len(), 1, "{v}");
        assert!(v.get("resolved_symbols").is_none(), "{v}");
    }

    #[test]
    fn bad_filters_are_errors() {
        assert!(ImpactQuery {
            min_confidence: Some("most".into()),
            ..Default::default()
        }
        .options(3)
        .is_err());
        let o = ImpactQuery::default().options(2).unwrap();
        assert_eq!(
            o,
            ImpactOptions {
                depth: 2,
                ..Default::default()
            }
        );
    }

    /// TASK-017 fixtures: a Java service injected by its interface.
    const JAVA_DI: &[(&str, &str)] = &[
        (
            "src/main/java/app/IService.java",
            "package app;\n\npublic interface IService {\n    void update(String id);\n}\n",
        ),
        (
            "src/main/java/app/ServiceImpl.java",
            "package app;\n\npublic class ServiceImpl implements IService {\n    private final Repo repo = new Repo();\n\n    @Override\n    public void update(String id) {\n        repo.save(id);\n    }\n}\n",
        ),
        (
            "src/main/java/app/Repo.java",
            "package app;\n\npublic class Repo {\n    public void save(String id) {\n    }\n}\n",
        ),
        (
            "src/main/java/app/Api.java",
            "package app;\n\nimport jakarta.inject.Inject;\n\npublic class Api {\n    @Inject\n    IService service;\n\n    public void handle() {\n        service.update(\"x\");\n    }\n}\n",
        ),
    ];

    /// TS `implements`, a Rust trait, a Python base class and Go embedding.
    const POLY: &[(&str, &str)] = &[
        (
            "web/svc.ts",
            "export interface IService {\n  update(id: string): void;\n}\n\nexport class ServiceImpl implements IService {\n  update(id: string): void {\n    helper(id);\n  }\n}\n\nexport function helper(id: string): void {}\n",
        ),
        (
            "web/api.ts",
            "import { IService } from './svc';\n\nexport class Api {\n  constructor(private readonly service: IService) {}\n\n  handle(): void {\n    this.service.update('x');\n  }\n}\n",
        ),
        (
            "rs/src/lib.rs",
            "pub trait Sink {\n    fn write(&self, line: &str);\n}\n\npub struct FileSink;\n\nimpl Sink for FileSink {\n    fn write(&self, line: &str) {\n        flush(line);\n    }\n}\n\npub fn flush(_line: &str) {}\n\npub fn emit(s: &dyn Sink) {\n    s.write(\"x\");\n}\n",
        ),
        (
            "py/app/jobs.py",
            "class Job:\n    def run(self, x):\n        pass\n\n\nclass Nightly(Job):\n    def run(self, x):\n        tidy()\n\n\ndef tidy():\n    pass\n\n\ndef launch(job: Job):\n    job.run(1)\n",
        ),
        (
            "go/app/embed.go",
            "package app\n\ntype Inner struct{}\n\nfunc (i *Inner) Ping() {}\n\ntype Outer struct {\n\tInner\n}\n\nfunc (o *Outer) Ping() {}\n\nfunc Use(i *Inner) {\n\ti.Ping()\n}\n",
        ),
    ];

    /// TASK-017 (a): `impact("ServiceImpl.update")` sees who calls it
    /// through the interface its field is typed by (Java, injected):
    /// `via: "dispatch"`, `medium`, with the method it went through; with
    /// `dispatch: false` it is the answer of TASK-009, which is a subset.
    #[test]
    fn impact_sees_a_caller_through_an_injected_interface() {
        let (state, repo) = indexed_files("impact_dispatch_java", JAVA_DI);
        let v = impact(&state, "ServiceImpl.update", &ImpactQuery::default());
        let n = node(&v, "upstream", "Api.handle");
        assert_eq!(n["via"], "dispatch", "{v}");
        assert_eq!(n["confidence"], "medium", "{v}");
        assert_eq!(n["through"], "IService.update", "{v}");
        assert_eq!(n["depth"], 1, "{v}");
        assert_eq!(syms(&v, "downstream"), ["Repo.save"], "{v}");
        assert_eq!(v["filters"]["dispatch"], true, "{v}");
        let off = ImpactQuery {
            dispatch: Some(false),
            ..Default::default()
        };
        let plain = impact(&state, "ServiceImpl.update", &off);
        assert!(syms(&plain, "upstream").is_empty(), "{plain}");
        assert_eq!(plain["filters"]["dispatch"], false, "{plain}");
        // The answer without dispatch is a subset of the one with it.
        for side in ["upstream", "downstream"] {
            for n in plain[side].as_array().unwrap() {
                let m = node(&v, side, n["symbol"].as_str().unwrap());
                assert_eq!(m["depth"], n["depth"], "{side}: {plain} vs {v}");
            }
        }
        // Downstream from the interface: its implementation, then what that
        // calls; never `high`.
        let v = impact(&state, "IService.update", &ImpactQuery::default());
        assert_eq!(
            syms(&v, "downstream"),
            ["ServiceImpl.update", "Repo.save"],
            "{v}"
        );
        assert_eq!(
            node(&v, "downstream", "ServiceImpl.update")["confidence"],
            "medium"
        );
        // `min_confidence: high` leaves it out, without reading it (review
        // MAJOR 2): nothing of it is counted.
        let high = ImpactQuery {
            min_confidence: Some("high".into()),
            ..Default::default()
        };
        let v = impact(&state, "ServiceImpl.update", &high);
        assert!(syms(&v, "upstream").is_empty(), "{v}");
        assert!(v.get("below_confidence").is_none(), "{v}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// TASK-017 (d): TypeScript `implements` and a Rust trait, both ways;
    /// a Python base class too (best effort); Go embedding is no dispatch
    /// (a promoted method is not overridden).
    #[test]
    fn dispatch_follows_typescript_rust_and_python_but_not_go_embedding() {
        let (state, repo) = indexed_files("impact_dispatch_poly", POLY);
        for (imp, caller, through) in [
            ("ServiceImpl.update", "Api.handle", "IService.update"),
            ("FileSink.write", "emit", "Sink.write"),
            ("Nightly.run", "launch", "Job.run"),
        ] {
            let v = impact(&state, imp, &ImpactQuery::default());
            let n = node(&v, "upstream", caller);
            assert_eq!(n["via"], "dispatch", "{imp}: {v}");
            assert_eq!(n["through"], through, "{imp}: {v}");
            assert_ne!(n["confidence"], "high", "{imp}: {v}");
            let v = impact(&state, through, &ImpactQuery::default());
            let n = node(&v, "downstream", imp);
            assert_eq!(n["via"], "dispatch", "{through}: {v}");
            assert_eq!(n["through"], through, "{through}: {v}");
        }
        let v = impact(&state, "Outer.Ping", &all_in());
        assert!(
            !syms(&v, "upstream").iter().any(|s| s == "Use"),
            "embedding is no dispatch: {v}"
        );
        let v = impact(&state, "Inner.Ping", &all_in());
        assert!(
            !syms(&v, "downstream").iter().any(|s| s == "Outer.Ping"),
            "{v}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// TASK-017 (c) through the tool: a base class with more subclasses than
    /// a node is expanded into lists the cap and counts the rest in
    /// `omitted_by_limit.dispatch`, and only there (review of TASK-017,
    /// deviation 6): `omitted_by_limit.count` and `omitted.count` keep
    /// counting nodes `max_nodes` and the budget cut.
    #[test]
    fn the_dispatch_cap_is_counted_in_omitted_by_limit() {
        let mut py = String::from("class Base:\n    def run(self):\n        pass\n");
        for i in 0..20 {
            py.push_str(&format!(
                "\n\nclass Sub{i:02}(Base):\n    def run(self):\n        pass\n"
            ));
        }
        let (state, repo) = indexed_files("impact_dispatch_cap", &[("app/subs.py", py.as_str())]);
        let v = impact(&state, "Base.run", &ImpactQuery::default());
        let cap = devctx_store::DISPATCH_MAX_PER_NODE;
        assert_eq!(v["downstream"].as_array().unwrap().len(), cap, "{v}");
        assert_eq!(v["omitted_by_limit"]["dispatch"]["count"], 20 - cap, "{v}");
        assert_eq!(
            v["omitted_by_limit"]["dispatch"]["max_per_node"], cap,
            "{v}"
        );
        assert_eq!(v["omitted_by_limit"]["count"], 0, "{v}");
        assert!(v["omitted_by_limit"].get("downstream").is_none(), "{v}");
        assert!(
            v["omitted_by_limit"]["hint"]
                .as_str()
                .unwrap()
                .contains("dispatch: false"),
            "{v}"
        );
        assert!(v.get("omitted").is_none(), "{v}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn dispatch_is_a_filter_of_the_query() {
        let q = ImpactQuery {
            dispatch: Some(false),
            ..Default::default()
        };
        assert!(!q.options(3).unwrap().dispatch);
        assert!(ImpactQuery::default().options(3).unwrap().dispatch);
        assert_eq!(q.query_pairs(), [("dispatch", "false".to_string())]);
    }

    /// Measurement harness (PLAN-009 TASK-009, not a test): indexes
    /// `$DEVCTX_IMPACT_BENCH_REPO` into `$DEVCTX_IMPACT_BENCH_STATE` with the
    /// model-free embedder (parse, symbol graph and link pass are the real
    /// ones), then for each line (a symbol) of `$DEVCTX_IMPACT_BENCH_CASES`
    /// prints one JSON line: p50/p95 in ms over `$DEVCTX_IMPACT_BENCH_RUNS`
    /// runs (default 30) of the tool with its defaults (each call opens the
    /// store, as the serve does), of the store walk with each frontier form
    /// (store open once), and of the 0.9.0 walk on the same rows (the branch
    /// stamped by another extractor), with the sizes of each answer. With
    /// `$DEVCTX_IMPACT_BENCH_TOP`, first the most called symbols; with
    /// `$DEVCTX_IMPACT_BENCH_DUMP`, each case's whole answer. The tool is
    /// timed with dispatch (the default) and without it (TASK-017).
    ///
    /// `cargo test --release -p devctx-mcp --lib impact_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn impact_bench() {
        use super::super::lookup::tests::Fake;
        use devctx_store::FrontierSql;
        let var = |k: &str| std::env::var(k).ok();
        let (Some(repo), Some(state_dir), Some(cases)) = (
            var("DEVCTX_IMPACT_BENCH_REPO"),
            var("DEVCTX_IMPACT_BENCH_STATE"),
            var("DEVCTX_IMPACT_BENCH_CASES"),
        ) else {
            eprintln!("set DEVCTX_IMPACT_BENCH_REPO, _STATE and _CASES");
            return;
        };
        let runs: usize = var("DEVCTX_IMPACT_BENCH_RUNS")
            .and_then(|r| r.parse().ok())
            .unwrap_or(30);
        // Depth of the uncapped walk (wider frontiers than the default 3).
        let wide: usize = var("DEVCTX_IMPACT_BENCH_DEPTH")
            .and_then(|r| r.parse().ok())
            .unwrap_or(3);
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
        let (r, b) = state.repo_branch().unwrap();
        let stamp = |s: &str| {
            let store = state.open_store().unwrap();
            store
                .set_index_meta(&state.repo_path(), &b, devctx_store::EXTRACTOR_META_KEY, s)
                .unwrap();
        };
        stamp(&devctx_index::extractor_fingerprint());
        if var("DEVCTX_IMPACT_BENCH_TOP").is_some() {
            let store = state.open_store().unwrap();
            let top = store.impact_top_called(&r, &b, 25).unwrap();
            for (name, n) in top {
                println!("{}", json!({ "top": name, "calls": n }));
            }
        }
        let time = |f: &mut dyn FnMut() -> usize| -> (f64, f64, usize) {
            let size = f();
            let mut ms: Vec<f64> = (0..runs)
                .map(|_| {
                    let t = Instant::now();
                    let _ = f();
                    t.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |p: f64| ms[((ms.len() as f64 - 1.0) * p).round() as usize];
            (pct(0.5), pct(0.95), size)
        };
        let count = |v: &Value| -> usize {
            v["upstream"].as_array().map_or(0, |a| a.len())
                + v["downstream"].as_array().map_or(0, |a| a.len())
        };
        // Level queries over synthetic frontiers of `$DEVCTX_IMPACT_BENCH_FRONTIER`
        // ids (comma-separated sizes), the branch's symbols in id order: the
        // widths a real walk rarely reaches.
        if let Some(sizes) = var("DEVCTX_IMPACT_BENCH_FRONTIER") {
            let store = state.open_store().unwrap();
            let mut ids: Vec<u64> = store
                .branch_symbols(&r, &b)
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .collect();
            ids.sort_unstable();
            for n in sizes
                .split(',')
                .filter_map(|n| n.trim().parse::<usize>().ok())
            {
                let f = &ids[..n.min(ids.len())];
                let above = f.iter().filter(|&&i| i > i64::MAX as u64).count();
                for upstream in [true, false] {
                    let mut out = serde_json::Map::new();
                    for mode in [
                        FrontierSql::InList,
                        FrontierSql::InListTyped,
                        FrontierSql::ListParam,
                        FrontierSql::TempTable,
                        FrontierSql::Auto,
                    ] {
                        let (p50, p95, rows) = time(&mut || {
                            store.impact_level_bench(&r, &b, f, upstream, mode).unwrap()
                        });
                        out.insert(
                            format!("{mode:?}"),
                            json!({ "p50_ms": p50, "p95_ms": p95, "rows": rows }),
                        );
                    }
                    println!(
                        "{}",
                        json!({
                            "frontier": f.len(), "above_i64": above,
                            "upstream": upstream, "modes": out,
                        })
                    );
                }
            }
        }
        let text = std::fs::read_to_string(cases).unwrap();
        let symbols: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        for sym in &symbols {
            let mut first = Value::Null;
            let tool = time(&mut || {
                let v: Value = serde_json::from_str(
                    &do_impact_with(&state, sym, 3, &ImpactQuery::default()).unwrap(),
                )
                .unwrap();
                let n = count(&v);
                first = v;
                n
            });
            // The same tool call without dispatch (TASK-017): its cost.
            let no_dispatch = ImpactQuery {
                dispatch: Some(false),
                ..Default::default()
            };
            let mut plain = Value::Null;
            let tool_plain = time(&mut || {
                let v: Value =
                    serde_json::from_str(&do_impact_with(&state, sym, 3, &no_dispatch).unwrap())
                        .unwrap();
                let n = count(&v);
                plain = v;
                n
            });
            let dispatched = |v: &Value| -> usize {
                ["upstream", "downstream"]
                    .iter()
                    .filter_map(|s| v[*s].as_array())
                    .flatten()
                    .filter(|n| n["via"] == "dispatch")
                    .count()
            };
            let store = state.open_store().unwrap();
            let mut walks = serde_json::Map::new();
            let tool_only = var("DEVCTX_IMPACT_BENCH_TOOL_ONLY").is_some();
            for (label, opts) in [
                ("default", devctx_store::ImpactOptions::default()),
                (
                    "uncapped_all",
                    devctx_store::ImpactOptions {
                        depth: wide,
                        min_confidence: devctx_store::MinConfidence::Low,
                        include_tests: true,
                        include_external: true,
                        max_nodes: 0,
                        dispatch: true,
                    },
                ),
            ] {
                if tool_only {
                    break;
                }
                for mode in [
                    FrontierSql::InList,
                    FrontierSql::InListTyped,
                    FrontierSql::ListParam,
                    FrontierSql::TempTable,
                    FrontierSql::Auto,
                ] {
                    let (mut levels, mut widest) = (0, 0);
                    let (p50, p95, n) = time(&mut || {
                        let im = store
                            .impact_graph_bench(&r, &b, sym, None, &opts, mode)
                            .unwrap();
                        levels = im.upstream.levels + im.downstream.levels;
                        widest = im.upstream.widest.max(im.downstream.widest);
                        im.upstream.nodes.len() + im.downstream.nodes.len()
                    });
                    walks.insert(
                        format!("{label}/{mode:?}"),
                        json!({
                            "p50_ms": p50, "p95_ms": p95, "nodes": n,
                            "levels": levels, "widest": widest,
                        }),
                    );
                }
            }
            drop(store);
            // The whole answer, to check a case by hand (the gold of TASK-017).
            if var("DEVCTX_IMPACT_BENCH_DUMP").is_some() {
                println!("{}", json!({ "symbol": sym, "answer": first }));
            }
            println!(
                "{}",
                json!({
                    "symbol": sym,
                    "tool": { "p50_ms": tool.0, "p95_ms": tool.1, "nodes": tool.2 },
                    "dispatch_nodes": dispatched(&first),
                    "tool_no_dispatch": {
                        "p50_ms": tool_plain.0, "p95_ms": tool_plain.1, "nodes": tool_plain.2,
                        "dispatch_nodes": dispatched(&plain),
                    },
                    "omitted": first.get("omitted"),
                    "omitted_by_limit": first.get("omitted_by_limit"),
                    "below_confidence": first["below_confidence"].get("count"),
                    "undecided_calls": first["undecided_calls"].get("count"),
                    "excluded": first.get("excluded").map(|e| json!([e["tests"], e["external"]])),
                    "resolved": first["resolved_symbols"].as_array().map(|a| a.len()),
                    "walks": walks,
                })
            );
        }
        if var("DEVCTX_IMPACT_BENCH_TOOL_ONLY").is_some() {
            return;
        }
        // The 0.9.0 walk on the same rows.
        stamp("v0-old");
        for sym in &symbols {
            let old = time(&mut || {
                let v: Value = serde_json::from_str(&do_impact(&state, sym, 3).unwrap()).unwrap();
                count(&v)
            });
            println!(
                "{}",
                json!({ "symbol": sym, "old": { "p50_ms": old.0, "p95_ms": old.1, "nodes": old.2 } })
            );
        }
        stamp(&devctx_index::extractor_fingerprint());
    }
}
