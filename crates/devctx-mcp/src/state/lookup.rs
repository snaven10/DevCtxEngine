//! Lookups by symbol over the symbol graph (PLAN-009 TASK-008, DD-10): the
//! new path of `read_symbol`, `get_references`, the anchoring marks of
//! `search` and the web graph view.
//!
//! A branch takes this path only when its index is current (no
//! `extractor_stale`: same extractor fingerprint and graph in step with the
//! files) and has rows in `symbols`. Otherwise every tool answers from the
//! 0.9.0 readers over `graph_edges`/`vectors` and says why (DD-19).

use devctx_store::{sym_hex, MinConfidence, Store, SymbolDefinition, SymbolReference};
use serde_json::{json, Value};

use super::{split_file_symbol, BranchChoice};

/// The line an answer from the 0.9.0 readers carries when the branch is
/// current but has no `symbols` rows (`extractor_stale` already brings its
/// own warning).
pub(super) const NO_SYMBOL_GRAPH_WARNING: &str =
    "this branch has no rows in the symbol graph, so this answer comes from the \
     0.9 call graph (names and suffixes, no confidence); if it has source files, \
     reindex (devctx index --full)";

/// The line `get_references` adds when its default hid references.
const BELOW_CONFIDENCE_HINT: &str =
    "references below the confidence shown were left out (ambiguous names or calls \
     the index could not decide); pass min_confidence: \"low\" to list them, marked";

/// How a graph tool reads a branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reader {
    /// `symbols` + `live_edges`, by id.
    SymbolGraph,
    /// The 0.9.0 readers over `graph_edges`/`vectors`; `note` when the branch
    /// is current but has no symbol rows (a stale one is already warned).
    Legacy { note: bool },
}

impl Reader {
    /// The reader for `chosen`: the symbol graph only for a current index
    /// (not `extractor_stale`) with rows in `symbols`. A failed read is the
    /// legacy path, never an empty answer.
    pub(super) fn of(chosen: &BranchChoice, store: &Store) -> Reader {
        if !chosen.indexed || chosen.extractor_stale {
            return Reader::Legacy { note: false };
        }
        match store.has_symbol_graph(&chosen.repo, &chosen.branch) {
            Ok(true) => Reader::SymbolGraph,
            _ => Reader::Legacy { note: true },
        }
    }

    /// Adds the no-symbol-graph line when this answer took the legacy path
    /// on a current branch, without overwriting a warning already there.
    pub(super) fn annotate(self, out: &mut Value) {
        if let Reader::Legacy { note: true } = self {
            if out.get("warning").is_none() {
                out["warning"] = json!(NO_SYMBOL_GRAPH_WARNING);
            }
        }
    }
}

/// The language a symbol's file is in, for a definition with no chunk.
fn language_of(file: &str) -> String {
    let ext = file.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    devctx_index::lang_for_extension(&ext.to_ascii_lowercase())
        .map(|l| l.name().to_string())
        .unwrap_or_default()
}

/// The `definitions` entries of one symbol: one per code chunk (as 0.9.0
/// answered, chunk by chunk), or one with its signature when the chunker
/// gives it no chunk (a field, a constant, a Rust `impl`). Every entry adds
/// `sym`, `kind` and `qualified` to the 0.9.0 fields.
fn definition_entries(d: &SymbolDefinition) -> Vec<Value> {
    let s = &d.symbol;
    let extra = |mut v: Value| {
        v["sym"] = json!(sym_hex(s.id));
        v["kind"] = json!(s.kind);
        v["qualified"] = json!(s.qualified);
        v
    };
    if d.chunks.is_empty() {
        return vec![extra(json!({
            "symbol": s.name,
            "type": s.kind,
            "language": language_of(&s.file),
            "file": s.file,
            "start_line": s.start_line,
            "end_line": s.end_line,
            "code": s.signature.clone().unwrap_or_default(),
        }))];
    }
    d.chunks
        .iter()
        .map(|p| {
            extra(json!({
                "symbol": p.metadata.symbol,
                "type": p.metadata.symbol_type,
                "language": p.metadata.language,
                "file": p.metadata.file,
                "start_line": p.metadata.start_line,
                "end_line": p.metadata.end_line,
                "code": p.text,
            }))
        })
        .collect()
}

/// `name` split into an optional file and the symbol part (`src/x.ts::f`).
fn file_and_name(name: &str) -> (Option<&str>, &str) {
    match split_file_symbol(name) {
        Some((f, n)) => (Some(f), n),
        None => (None, name),
    }
}

/// `read_symbol` over the symbol graph: same object as 0.9.0
/// (`{symbol, definitions}`, plus `suggestions`/`external`/`called_from`/
/// `next_step` on a miss).
pub(super) fn read_symbol(
    store: &Store,
    chosen: &BranchChoice,
    name: &str,
    limit: usize,
) -> Result<Value, String> {
    let (file, bare) = file_and_name(name);
    let found = store
        .lookup_definitions(&chosen.repo, &chosen.branch, bare, file, limit)
        .map_err(|e| e.to_string())?;
    let definitions: Vec<Value> = found.iter().flat_map(definition_entries).collect();
    let mut out = json!({ "symbol": name, "definitions": definitions });
    if found.is_empty() {
        not_found_hints(store, &chosen.repo, &chosen.branch, bare, name, &mut out);
    }
    Ok(out)
}

/// The miss of [`read_symbol`]: `external: true` with `called_from` and
/// `next_step` only when the link pass marked call sites of the name external
/// (DD-9; a qualified name falls back to its last segment only when the repo
/// defines no symbol of that name, pending 2 of PLAN-008); `suggestions` are
/// the definitions of the same bare name and, for a name that is not
/// external, the closest names of the repo. A library function gets no near
/// miss (`with_context` is not a slip for `build_context`, B3), only the
/// qualified forms its own call sites wrote.
fn not_found_hints(
    store: &Store,
    repo: &str,
    branch: &str,
    bare: &str,
    shown: &str,
    out: &mut Value,
) {
    const MAX_SUGGESTIONS: usize = 5;
    let external = store.lookup_external(repo, branch, bare).ok().flatten();
    let mut suggestions: Vec<String> = Vec::new();
    let add = |c: String, list: &mut Vec<String>| {
        if list.len() < MAX_SUGGESTIONS && c != shown && c != bare && !list.contains(&c) {
            list.push(c);
        }
    };
    match &external {
        Some(sites) => {
            for n in &sites.names {
                add(n.clone(), &mut suggestions);
            }
        }
        None => {
            for c in store
                .lookup_same_name(repo, branch, bare, MAX_SUGGESTIONS)
                .unwrap_or_default()
            {
                add(c, &mut suggestions);
            }
            for c in store
                .lookup_suggestions(repo, branch, bare, MAX_SUGGESTIONS)
                .unwrap_or_default()
            {
                add(c, &mut suggestions);
            }
        }
    }
    if external.is_none() || !suggestions.is_empty() {
        out["suggestions"] = json!(suggestions);
    }
    if let Some(sites) = external {
        let n = sites.count;
        out["external"] = json!(true);
        out["called_from"] = json!(n);
        out["next_step"] = json!(format!(
            "`{shown}` is called from {n} place(s) but no indexed definition was found on \
             this branch — likely a library or runtime function, though it may live in a file \
             the index excludes or on another branch; `get_references` lists the call sites"
        ));
    }
}

/// `get_references` over the symbol graph: every occurrence (one row per
/// call, also two from the same method) of the definitions `symbol` names,
/// by file and line; for a name with no definition, its external call sites.
///
/// Each reference keeps the 0.9.0 fields (`file`, `line`, `source`) and adds
/// `confidence`, `via` (the edge kind), `sym` (the referenced definition's
/// id, when resolved) and `external`/`test`/`undecided` only when true. By
/// default `high` and `medium` are listed (`medium` is the mark: the field
/// says it); `low` and undecided rows only with `min_confidence: "low"`, and
/// what the default hid is counted in `below_confidence`, never dropped in
/// silence. Returns the references and `resolved_symbols`' names.
pub(super) fn references(
    store: &Store,
    chosen: &BranchChoice,
    symbol: &str,
    min: MinConfidence,
) -> Result<(Vec<Value>, Vec<String>, usize), String> {
    let (file, bare) = file_and_name(symbol);
    let syms = store
        .lookup_symbols(&chosen.repo, &chosen.branch, bare, file, 500)
        .map_err(|e| e.to_string())?;
    let ids: Vec<u64> = syms.iter().map(|s| s.id).collect();
    let resolved = Store::qualified_names(&syms);
    let rows = store
        .lookup_references(&chosen.repo, &chosen.branch, &ids, bare, true)
        .map_err(|e| e.to_string())?;
    let (shown, hidden): (Vec<&SymbolReference>, Vec<&SymbolReference>) =
        rows.iter().partition(|r| {
            min.admits(r.confidence.as_deref()) && (min == MinConfidence::Low || !r.undecided())
        });
    let refs = shown
        .into_iter()
        .map(|r| {
            let mut v = json!({
                "file": r.file,
                "line": r.line,
                "source": r.source,
                "confidence": r.confidence.as_deref().unwrap_or("low"),
                "via": r.kind,
            });
            if let Some(id) = r.dst_id {
                v["sym"] = json!(sym_hex(id));
            }
            if r.external {
                v["external"] = json!(true);
            }
            if r.from_test {
                v["test"] = json!(true);
            }
            if r.undecided() {
                v["undecided"] = json!(true);
            }
            v
        })
        .collect();
    Ok((refs, resolved, hidden.len()))
}

/// `below_confidence` for an answer whose default left `n` references out.
pub(super) fn below_confidence(n: usize, min: MinConfidence) -> Option<Value> {
    (n > 0).then(|| {
        json!({
            "count": n,
            "min_confidence": match min {
                MinConfidence::High => "high",
                MinConfidence::Medium => "medium",
                MinConfidence::Low => "low",
            },
            "hint": BELOW_CONFIDENCE_HINT,
        })
    })
}

/// Parse `get_references`' `min_confidence` (default `medium`).
pub(super) fn parse_min_confidence(s: Option<&str>) -> Result<MinConfidence, String> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(MinConfidence::Medium),
        Some(v) => MinConfidence::parse(v).ok_or_else(|| {
            format!("min_confidence must be \"high\", \"medium\" or \"low\", not {v:?}")
        }),
    }
}

/// The web graph view over the symbol graph, in the 0.9.0 shape (node ids
/// are the qualified names `get_references` takes back): a node is
/// `external` when an edge to it was marked external by the link pass, not
/// because it never calls anything; edges add `confidence` and `test`.
pub(super) fn graph_view(
    chosen: &BranchChoice,
    edges: &[devctx_store::ViewEdge],
    hide_external: bool,
    hide_synthetic: bool,
) -> Value {
    use std::collections::HashMap;
    let mut node_ids: Vec<String> = Vec::new();
    let mut node_file: HashMap<String, String> = HashMap::new();
    let mut node_ext: HashMap<String, bool> = HashMap::new();
    let mut out_edges: Vec<Value> = Vec::new();
    for (i, e) in edges.iter().enumerate() {
        if hide_external && e.external {
            continue;
        }
        if hide_synthetic && (super::is_synthetic(&e.source) || super::is_synthetic(&e.target)) {
            continue;
        }
        for (id, file, ext) in [
            (&e.source, e.file.as_str(), false),
            (&e.target, "", e.external),
        ] {
            if !node_file.contains_key(id) {
                node_ids.push(id.clone());
                node_file.insert(id.clone(), file.to_string());
                node_ext.insert(id.clone(), ext);
            } else {
                if node_file[id].is_empty() && !file.is_empty() {
                    node_file.insert(id.clone(), file.to_string());
                }
                if ext {
                    node_ext.insert(id.clone(), true);
                }
            }
        }
        let mut data = json!({
            "id": format!("e{i}"),
            "source": e.source,
            "target": e.target,
            "kind": e.kind,
            "file": e.file,
            "line": e.line,
            "confidence": e.confidence,
        });
        if e.external {
            data["external"] = json!(true);
        }
        if e.from_test {
            data["test"] = json!(true);
        }
        out_edges.push(json!({ "data": data }));
    }
    let nodes: Vec<Value> = node_ids
        .iter()
        .map(|id| {
            json!({
                "data": {
                    "id": id,
                    "label": super::short_label(id),
                    "file": node_file.get(id).cloned().unwrap_or_default(),
                    "repo": chosen.repo,
                    "external": node_ext.get(id).copied().unwrap_or(false),
                }
            })
        })
        .collect();
    json!({
        "repo": chosen.repo,
        "branch": chosen.branch,
        "nodes": nodes,
        "edges": out_edges,
    })
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::path::{Path, PathBuf};

    struct Fake(usize);
    impl EmbeddingProvider for Fake {
        fn embed(&self, texts: &[String]) -> devctx_embed::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    (0..self.0)
                        .map(|j| ((t.len() + j) % 10) as f32 / 10.0 + 0.05)
                        .collect()
                })
                .collect())
        }
        fn dimension(&self) -> usize {
            self.0
        }
        fn model_name(&self) -> &str {
            "fake"
        }
    }

    fn git(dir: &Path, args: &[&str]) {
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

    /// A repository of several languages, committed on `main` and indexed
    /// for real (`do_index --full`: parse, `symbols`/`edges`, link pass) with
    /// an embedder that needs no model.
    pub(crate) fn indexed(tag: &str, extra: &[(&str, &str)]) -> (AppState, PathBuf) {
        let mut files: Vec<(&str, &str)> = FIXTURE.to_vec();
        files.extend_from_slice(extra);
        indexed_files(tag, &files)
    }

    /// [`indexed`] with exactly `files`.
    pub(crate) fn indexed_files(tag: &str, files: &[(&str, &str)]) -> (AppState, PathBuf) {
        let repo = std::env::temp_dir().join(format!("devctx_lookup_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        let files = files.to_vec();
        for (path, text) in files {
            let p = repo.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        let mut cfg = ProjectConfig::default();
        cfg.project.path = repo.to_string_lossy().into_owned();
        cfg.state_dir = repo.join(".state").to_string_lossy().into_owned();
        cfg.storage.hnsw = false;
        let state = AppState::build(cfg).unwrap();
        let dim = configured_dimension(&state.cfg);
        *state.embedder.lock().unwrap() = Some(Cached {
            value: Arc::new(Fake(dim)),
            last_used: Instant::now(),
            key: "fake/fake".into(),
            loaded_at: procmem::unix_now(),
        });
        do_index(&state, true).unwrap();
        (state, repo)
    }

    const FIXTURE: &[(&str, &str)] = &[
        (
            "py/app/svc.py",
            "def helper():\n    return 1\n\n\nclass Runner:\n    def run(self):\n        helper()\n        helper()\n        return 0\n\n\nhelper()\n",
        ),
        (
            "web/package.json",
            "{\"name\": \"web\", \"dependencies\": {\"@angular/common\": \"17.0.0\"}}\n",
        ),
        (
            "web/libs/auth/src/lib/token.interceptor.ts",
            "import { HttpInterceptorFn } from '@angular/common/http';\n\nexport const tokenInterceptor: HttpInterceptorFn = (req, next) => {\n  const authReq = req.clone({ setHeaders: { Authorization: 'Bearer x' } });\n  return next(authReq);\n};\n",
        ),
        (
            "rs/Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde_json = \"1\"\nanyhow = \"1\"\n",
        ),
        (
            "rs/src/main.rs",
            "mod thing;\n\nuse anyhow::Context;\n\nfn load(p: &str) -> anyhow::Result<String> {\n    std::fs::read_to_string(p).with_context(|| format!(\"reading {p}\"))\n}\n\nfn parse(s: &str) -> serde_json::Value {\n    serde_json::from_str(s).unwrap()\n}\n\nfn build_context() -> thing::Thing {\n    thing::Thing::new()\n}\n\nfn main() {\n    let _ = std::path::PathBuf::new();\n    let _ = load(\"x\");\n    let _ = parse(\"{}\");\n    let _ = build_context();\n}\n",
        ),
        (
            "rs/src/thing.rs",
            "pub struct Thing;\n\nimpl Thing {\n    pub fn new() -> Self {\n        Thing\n    }\n}\n",
        ),
        (
            "java/src/main/java/com/example/Repo.java",
            "package com.example;\n\nimport io.quarkus.hibernate.reactive.panache.Panache;\nimport java.util.List;\n\npublic class Repo {\n    public void save() {\n        Panache.withTransaction(() -> null);\n    }\n\n    public void walk() {\n        List.of(1).forEach(s -> s.fetchOne());\n        List.of(1).forEach(s -> s.flush());\n        List.of(1).forEach(s -> s.fetchRows());\n    }\n}\n",
        ),
        (
            "java/src/main/java/com/example/Rows.java",
            "package com.example;\n\npublic class Rows {\n    public void fetchOne() {\n    }\n}\n",
        ),
        (
            "java/src/main/java/com/example/Left.java",
            "package com.example;\n\npublic class Left {\n    public void flush() {\n    }\n}\n",
        ),
        (
            "java/src/main/java/com/example/Right.java",
            "package com.example;\n\npublic class Right {\n    public void flush() {\n    }\n}\n",
        ),
    ];

    fn read(state: &AppState, name: &str) -> Value {
        serde_json::from_str(&do_read_symbol(state, name, 5).unwrap()).unwrap()
    }

    fn refs(state: &AppState, name: &str) -> Value {
        serde_json::from_str(&do_references(state, name).unwrap()).unwrap()
    }

    fn strs(v: &Value) -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// (a) pending 1 of PLAN-008 and B4: `Foo.Bar::baz` names a symbol, not
    /// the file `Foo.Bar`; `links.rs::memories_by_symbol` names a file.
    #[test]
    fn a_dotted_type_before_double_colon_is_not_a_file() {
        assert_eq!(split_file_symbol("Foo.Bar::baz"), None);
        assert_eq!(split_file_symbol("Outer.Inner::run"), None);
        assert_eq!(
            split_file_symbol("links.rs::memories_by_symbol"),
            Some(("links.rs", "memories_by_symbol"))
        );
        assert_eq!(
            split_file_symbol("src/x.ts::tokenInterceptor"),
            Some(("src/x.ts", "tokenInterceptor"))
        );
        // A path keeps working whatever its extension says.
        assert_eq!(
            split_file_symbol("a/Foo.Bar::baz"),
            Some(("a/Foo.Bar", "baz"))
        );
    }

    #[test]
    fn symbol_lookups_over_the_symbol_graph() {
        let (state, repo) = indexed("main", &[]);

        // (f) a TS arrow-const is a definition, in its file.
        let v = read(&state, "tokenInterceptor");
        let defs = v["definitions"].as_array().unwrap();
        assert_eq!(defs.len(), 1, "{v}");
        assert_eq!(
            defs[0]["file"], "web/libs/auth/src/lib/token.interceptor.ts",
            "{v}"
        );
        assert!(
            defs[0]["code"]
                .as_str()
                .unwrap()
                .contains("tokenInterceptor"),
            "{v}"
        );
        let sym = defs[0]["sym"].as_str().unwrap_or_default();
        assert_eq!(sym.len(), 16, "`sym` is 16 hex digits: {v}");
        assert!(sym.chars().all(|c| c.is_ascii_hexdigit()), "{v}");
        assert!(v.get("external").is_none(), "{v}");

        // (b) pending 2: a qualified name that does not exist is not external
        // because some `new` is called (a leaf of the repo, a library's
        // `PathBuf::new`): the repo defines `new`, so the last segment says
        // nothing. It gets suggestions.
        let v = read(&state, "Foo::new");
        assert!(v["definitions"].as_array().unwrap().is_empty(), "{v}");
        assert!(v.get("external").is_none(), "{v}");
        assert!(
            strs(&v["suggestions"]).iter().any(|s| s == "Thing.new"),
            "{v}"
        );

        // (c) B2: qualified library calls are external, with their sites.
        for q in ["serde_json::from_str", "Panache.withTransaction"] {
            let v = read(&state, q);
            assert_eq!(v["external"], true, "{q}: {v}");
            assert!(v["called_from"].as_u64().unwrap() >= 1, "{q}: {v}");
        }

        // (d) B3: an external gets no near-miss name of this repo.
        let v = read(&state, "with_context");
        assert_eq!(v["external"], true, "{v}");
        assert!(
            !strs(&v["suggestions"])
                .iter()
                .any(|s| s.contains("build_context")),
            "an external must not suggest `build_context`: {v}"
        );

        // (e) two calls to `helper()` from one method are two references.
        let v = refs(&state, "helper");
        let r = v["references"].as_array().unwrap();
        let from_run = r.iter().filter(|x| x["source"] == "Runner.run").count();
        assert_eq!(from_run, 2, "{v}");
        assert!(
            r.iter()
                .all(|x| x["confidence"].is_string() && x["via"].is_string()),
            "{v}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    fn refs_min(state: &AppState, name: &str, min: &str) -> Value {
        serde_json::from_str(&do_references_with(state, name, Some(min)).unwrap()).unwrap()
    }

    fn graph(state: &AppState) -> Value {
        serde_json::from_str(&do_graph(state, None, None, 0, false, false).unwrap()).unwrap()
    }

    /// Requirement 1 of the review: a call the link pass discarded is kept
    /// as a row (to be reopened by name) and reaches no answer — not
    /// `get_references` even with `min_confidence: low`, not `read_symbol`'s
    /// `external`, not the graph view. 0.9.0's `graph_edges` still lists it.
    #[test]
    fn discarded_calls_reach_no_answer() {
        let (state, repo) = indexed("discarded", &[]);
        let store = state.open_store().unwrap();
        let (r, b) = state.repo_branch().unwrap();
        assert!(store.branch_discarded_calls(&r, &b).unwrap() >= 1);
        let raw = store.file_symbol_edges(&r, &b, "java/src/main/java/com/example/Repo.java");
        assert!(
            raw.unwrap()
                .iter()
                .any(|e| e.dst_name == "fetchRows" && e.resolution.as_deref() == Some("discarded")),
            "the fixture must hold a discarded `fetchRows` row"
        );
        let old = store.find_references(&r, &b, "fetchRows").unwrap();
        assert_eq!(old.len(), 1, "graph_edges keeps it: {old:?}");
        drop(store);

        for min in ["low", "medium"] {
            let v = refs_min(&state, "fetchRows", min);
            assert!(v["references"].as_array().unwrap().is_empty(), "{min}: {v}");
            assert!(v.get("below_confidence").is_none(), "{min}: {v}");
        }
        let v = read(&state, "fetchRows");
        assert!(v.get("external").is_none(), "{v}");
        let g = graph(&state);
        assert!(
            !g["edges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["data"]["target"] == "fetchRows"),
            "{g}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Requirement 4: by default `high` and `medium`, each with its
    /// `confidence`; `low`/undecided only with `min_confidence: low`, marked,
    /// and what the default hid is counted, never dropped in silence.
    #[test]
    fn references_show_high_and_medium_unless_asked_for_low() {
        let (state, repo) = indexed("confidence", &[]);
        // `s.flush()` on an untyped receiver with two `flush` in the repo:
        // the link pass cannot decide (`name_only`, low).
        let v = refs(&state, "flush");
        assert!(v["references"].as_array().unwrap().is_empty(), "{v}");
        assert_eq!(v["below_confidence"]["count"], 1, "{v}");
        assert_eq!(v["below_confidence"]["min_confidence"], "medium", "{v}");
        assert!(
            v["below_confidence"]["hint"]
                .as_str()
                .unwrap()
                .contains("min_confidence"),
            "{v}"
        );
        assert_eq!(
            strs(&v["resolved_symbols"]),
            ["Left.flush", "Right.flush"],
            "{v}"
        );
        let v = refs_min(&state, "flush", "low");
        let r = v["references"].as_array().unwrap();
        assert_eq!(r.len(), 1, "{v}");
        assert_eq!(r[0]["confidence"], "low", "{v}");
        assert_eq!(r[0]["undecided"], true, "{v}");
        assert!(v.get("below_confidence").is_none(), "{v}");

        // A `medium` reference is listed by default, marked by its field.
        let v = refs(&state, "with_context");
        let r = v["references"].as_array().unwrap();
        assert_eq!(r.len(), 1, "{v}");
        assert_eq!(r[0]["confidence"], "medium", "{v}");
        assert_eq!(r[0]["external"], true, "{v}");
        assert_eq!(r[0]["source"], "load", "{v}");
        // `high` only drops it, and says so.
        let v = refs_min(&state, "with_context", "high");
        assert!(v["references"].as_array().unwrap().is_empty(), "{v}");
        assert_eq!(v["below_confidence"]["count"], 1, "{v}");

        // A resolved reference names its definition by id.
        let v = refs(&state, "Thing.new");
        let r = v["references"].as_array().unwrap();
        assert_eq!(r.len(), 1, "{v}");
        assert_eq!(r[0]["via"], "calls", "{v}");
        assert_eq!(r[0]["sym"].as_str().unwrap().len(), 16, "{v}");

        assert!(do_references_with(&state, "flush", Some("most")).is_err());
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Requirement 3: a stale index (another extractor's stamp) answers from
    /// the 0.9.0 readers with the stale warning; a current branch without
    /// symbol rows says so too. Never a silent empty answer.
    #[test]
    fn a_stale_index_takes_the_old_path_and_says_so() {
        let (state, repo) = indexed("stale", &[]);
        let fresh = refs(&state, "helper");
        assert_eq!(fresh["references"].as_array().unwrap().len(), 3, "{fresh}");
        assert!(fresh.get("warning").is_none(), "{fresh}");
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
        // 0.9.0's answers: one reference (graph_edges folds the pair), no
        // `confidence`; `Foo::new` external by the last segment.
        let v = refs(&state, "helper");
        let r = v["references"].as_array().unwrap();
        assert_eq!(r.len(), 1, "{v}");
        assert!(r[0].get("confidence").is_none(), "{v}");
        assert!(
            v["warning"].as_str().unwrap().contains("older extractor"),
            "{v}"
        );
        let v = read(&state, "Foo::new");
        assert_eq!(v["external"], true, "{v}");
        assert!(
            v["warning"].as_str().unwrap().contains("older extractor"),
            "{v}"
        );
        let g = graph(&state);
        assert!(
            g["warning"].as_str().unwrap().contains("older extractor"),
            "{g}"
        );
        let _ = std::fs::remove_dir_all(&repo);

        // Current, but nothing parseable: no `symbols` rows.
        let (state, repo) = indexed_files(
            "nographs",
            &[("db/schema.sql", "CREATE TABLE things (id INT);\n")],
        );
        let v = read(&state, "things");
        assert!(
            v["warning"]
                .as_str()
                .unwrap()
                .contains("no rows in the symbol graph"),
            "{v}"
        );
        let v = refs(&state, "things");
        assert!(
            v["warning"]
                .as_str()
                .unwrap()
                .contains("no rows in the symbol graph"),
            "{v}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Step 5: the web view reads `live_edges`; `external` is the link
    /// pass's mark (a leaf of the repo is not external because it never
    /// calls anything) and an edge from a test file says `test`.
    #[test]
    fn the_graph_view_marks_externals_and_tests_from_the_link_pass() {
        let (state, repo) = indexed(
            "view",
            &[(
                "py/tests/test_svc.py",
                "from app.svc import helper\n\n\ndef test_helper():\n    helper()\n",
            )],
        );
        let g = graph(&state);
        let node = |id: &str| {
            g["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["data"]["id"] == id)
                .cloned()
                .unwrap_or_else(|| panic!("no node {id}: {g}"))
        };
        assert_eq!(
            node("serde_json::from_str")["data"]["external"],
            true,
            "{g}"
        );
        assert_eq!(node("Thing.new")["data"]["external"], false, "{g}");
        let edges = g["edges"].as_array().unwrap();
        assert!(
            edges.iter().any(|e| e["data"]["source"] == "test_helper"
                && e["data"]["target"] == "helper"
                && e["data"]["test"] == true),
            "{g}"
        );
        assert!(edges.iter().all(|e| e["data"]["kind"] != "contains"), "{g}");
        let hidden: Value =
            serde_json::from_str(&do_graph(&state, None, None, 0, true, false).unwrap()).unwrap();
        assert!(
            hidden["edges"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["data"].get("external").is_none()),
            "{hidden}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Anchoring over the symbol graph: the definition is pinned first,
    /// `anchored` by id, and says which definition (`sym`). The answer comes
    /// from another branch with `branch_fallback`, still by id.
    #[test]
    fn anchoring_and_branch_fallback_over_the_symbol_graph() {
        let (state, repo) = indexed("anchor", &[]);
        let sym = read(&state, "tokenInterceptor")["definitions"][0]["sym"].clone();
        git(&repo, &["checkout", "-q", "-b", "feat/x"]);
        let (items, fallback) = search_items(
            &state,
            "tokenInterceptor",
            5,
            None,
            SearchMode::Hybrid,
            false,
            &devctx_search::KindSel::default(),
            false,
        )
        .unwrap();
        assert!(fallback.is_some());
        assert_eq!(items[0]["anchored"], true, "{items:?}");
        assert_eq!(items[0]["sym"], sym, "{items:?}");
        assert_eq!(
            items[0]["file"], "web/libs/auth/src/lib/token.interceptor.ts",
            "{items:?}"
        );
        assert!(
            items.iter().skip(1).all(|i| i.get("anchored").is_none()),
            "{items:?}"
        );
        let v = read(&state, "tokenInterceptor");
        assert_eq!(v["branch_fallback"]["used"], "main", "{v}");
        assert_eq!(v["definitions"][0]["sym"], sym, "{v}");
        let v = refs(&state, "helper");
        assert_eq!(v["branch_fallback"]["used"], "main", "{v}");
        assert_eq!(v["references"].as_array().unwrap().len(), 3, "{v}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Measurement harness (PLAN-009 TASK-008, not a test): indexes
    /// `$DEVCTX_LOOKUP_BENCH_REPO` into `$DEVCTX_LOOKUP_BENCH_STATE` with the
    /// model-free embedder (parse, symbol graph and link pass are the real
    /// ones), then runs each line `tool|argument` of
    /// `$DEVCTX_LOOKUP_BENCH_CASES` (`read`, `refs`, `refs_low`, `search`)
    /// `$DEVCTX_LOOKUP_BENCH_RUNS` times (default 20) and prints one JSON
    /// line per case: the first answer, p50 and p95 in milliseconds. With
    /// `DEVCTX_LOOKUP_BENCH_OLD=1` the branch is stamped by another extractor
    /// first, so every tool takes the 0.9.0 path on the same rows.
    ///
    /// `cargo test --release -p devctx-mcp --lib lookup_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn lookup_bench() {
        let var = |k: &str| std::env::var(k).ok();
        let (Some(repo), Some(state_dir), Some(cases)) = (
            var("DEVCTX_LOOKUP_BENCH_REPO"),
            var("DEVCTX_LOOKUP_BENCH_STATE"),
            var("DEVCTX_LOOKUP_BENCH_CASES"),
        ) else {
            eprintln!("set DEVCTX_LOOKUP_BENCH_REPO, _STATE and _CASES");
            return;
        };
        let runs: usize = var("DEVCTX_LOOKUP_BENCH_RUNS")
            .and_then(|r| r.parse().ok())
            .unwrap_or(20);
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
        {
            let store = state.open_store().unwrap();
            let (_, b) = state.repo_branch().unwrap();
            let stamp = if var("DEVCTX_LOOKUP_BENCH_OLD").is_some() {
                "v0-old".to_string()
            } else {
                devctx_index::extractor_fingerprint()
            };
            store
                .set_index_meta(
                    &state.repo_path(),
                    &b,
                    devctx_store::EXTRACTOR_META_KEY,
                    &stamp,
                )
                .unwrap();
        }
        let text = std::fs::read_to_string(cases).unwrap();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (tool, arg) = line.split_once('|').unwrap();
            let call = || -> String {
                match tool {
                    "read" => do_read_symbol(&state, arg, 5).unwrap(),
                    "refs" => do_references(&state, arg).unwrap(),
                    "refs_low" => do_references_with(&state, arg, Some("low")).unwrap(),
                    "search" => {
                        let (items, _) = search_items(
                            &state,
                            arg,
                            10,
                            None,
                            SearchMode::Hybrid,
                            false,
                            &devctx_search::KindSel::default(),
                            false,
                        )
                        .unwrap();
                        Value::Array(items).to_string()
                    }
                    other => panic!("unknown tool {other}"),
                }
            };
            let first = call();
            let mut ms: Vec<f64> = (0..runs)
                .map(|_| {
                    let t = Instant::now();
                    let _ = call();
                    t.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |p: f64| ms[((ms.len() as f64 - 1.0) * p).round() as usize];
            println!(
                "{}",
                json!({
                    "tool": tool,
                    "arg": arg,
                    "p50_ms": pct(0.5),
                    "p95_ms": pct(0.95),
                    "out": serde_json::from_str::<Value>(&first).unwrap_or(Value::String(first)),
                })
            );
        }
    }
}
