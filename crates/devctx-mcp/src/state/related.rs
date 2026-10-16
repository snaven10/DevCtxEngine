//! The `## Related (signatures)` section of `build_context` (PLAN-009 DD-15,
//! TASK-012): the graph neighbors of the best code hits of a brief —
//! callers, callees, instantiations and supertypes —, as signatures, never
//! bodies, read once per brief.
//!
//! [`gather`] reads them (one batch over the store, see
//! `Store::related_neighbors`); `compose_context` renders them under its own
//! share of the budget. A branch without a current symbol graph gets no
//! section and a line that says why: never an empty section in silence.

use devctx_store::{ChunkRef, Store, StoredSymbol, RELATED_MAX_HOPS, RELATED_PER_SEED};
use serde_json::Value;

use super::lookup::{Legacy, Reader};
use super::BranchChoice;

/// The code hits of a brief whose neighbors are shown, at most (DD-15).
pub(super) const RELATED_SEEDS: usize = 5;

/// Share of a brief's budget the related section may take (DD-15): the code
/// that answers comes first.
pub(super) const CTX_RELATED_SHARE_PCT: usize = 20;

/// The section's heading.
pub const RELATED_HEADING: &str = "## Related (signatures)";

/// The start of the line a brief carries when it asked for the section and
/// has none to show (and why).
pub const RELATED_NOTE: &str = "[devctx] related:";

/// Whether `build_context` shows the related section when the call does not
/// say (Q-4 of the plan: on only if the harness justifies it; TASK-012).
pub const DEFAULT_RELATED: bool = false;

/// Hops of the ego-graph when the call does not say.
pub const DEFAULT_GRAPH_HOPS: u8 = 1;

/// What a `build_context` call asks of the related section; `None` is the
/// server's default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextOpts {
    /// Show the related section ([`DEFAULT_RELATED`] when `None`).
    pub related: Option<bool>,
    /// Hops of the ego-graph, 1 or 2 ([`DEFAULT_GRAPH_HOPS`] when `None`).
    pub graph_hops: Option<u8>,
}

impl ContextOpts {
    /// `(related, hops)`, or why the request is invalid.
    pub fn resolve(&self) -> Result<(bool, u8), String> {
        let hops = self.graph_hops.unwrap_or(DEFAULT_GRAPH_HOPS);
        if hops == 0 || hops > RELATED_MAX_HOPS {
            return Err(format!(
                "graph_hops must be 1 or {RELATED_MAX_HOPS}, not {hops}"
            ));
        }
        Ok((self.related.unwrap_or(DEFAULT_RELATED), hops))
    }

    /// Whether the section is on for this call, the server's default
    /// standing in for an unset `related`.
    pub fn wants_related(&self) -> bool {
        self.related.unwrap_or(DEFAULT_RELATED)
    }
}

/// One line of the section: a neighbor of a hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RelatedLine {
    /// The hit it surrounds: its index among the hits it was read from.
    pub hit: usize,
    /// The hit it surrounds (its qualified name).
    pub around: String,
    /// How it relates to the hit, read from the hit (`called by`, `calls`…).
    pub relation: &'static str,
    /// 1, or 2 for a neighbor of a neighbor.
    pub hops: u8,
    /// Its signature, never its body.
    pub signature: String,
    /// Where it is defined.
    pub file: String,
    pub line: i32,
}

/// What the section is built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Related {
    /// The neighbors, in the order of the brief's hits.
    Lines(Vec<RelatedLine>),
    /// Why there is no section (no current symbol graph, a failed read).
    Unavailable(String),
}

#[cfg(test)]
thread_local! {
    /// Batches [`gather`] read on this thread (lesson 4 of the reviews:
    /// nothing is read that is not shown).
    pub(super) static GATHERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The neighbors of the first [`RELATED_SEEDS`] code hits of `brief` (the
/// hits the brief shows, in its order) that map to a symbol, within `hops`.
pub(super) fn gather(store: &Store, chosen: &BranchChoice, brief: &[&Value], hops: u8) -> Related {
    #[cfg(test)]
    GATHERS.with(|c| c.set(c.get() + 1));
    if let Some(why) = unavailable(Reader::of(chosen, store)) {
        return Related::Unavailable(why);
    }
    let (repo, branch) = (chosen.repo.as_str(), chosen.branch.as_str());
    // Code only (a test, a doc page or a config file has no neighbors worth
    // the budget), and only chunks of code: a whole-file summary or a memory
    // is no symbol's.
    let code: Vec<(usize, &Value)> = brief
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, h)| super::hit_kind(h) == devctx_core::PathKind::Code)
        .filter(|(_, h)| !matches!(super::field(h, "level"), "file" | "memory" | "memory_chunk"))
        .collect();
    let line = |h: &Value, key: &str| h.get(key).and_then(Value::as_i64).unwrap_or(0) as i32;
    let chunks: Vec<ChunkRef<'_>> = code
        .iter()
        .map(|(_, h)| {
            let start = line(h, "start_line");
            ChunkRef {
                file: super::field(h, "file"),
                start_line: start,
                end_line: line(h, "end_line").max(start),
                doc: super::field(h, "level") == "doc",
            }
        })
        .collect();
    let symbols = match store.chunk_symbols(repo, branch, &chunks) {
        Ok(s) => s,
        Err(e) => {
            return Related::Unavailable(format!(
                "not shown: reading the symbol graph failed ({e})"
            ))
        }
    };
    // Each seed with the first hit of the brief that holds it.
    let mut seeds: Vec<StoredSymbol> = Vec::new();
    let mut seed_hit: Vec<usize> = Vec::new();
    for ((at, _), s) in code.iter().zip(symbols) {
        // Test code inside a code file (a Rust `mod tests`) is a test.
        let Some(s) = s.filter(|s| !s.is_test) else {
            continue;
        };
        if seeds.len() == RELATED_SEEDS {
            break;
        }
        if !seeds.iter().any(|x| x.id == s.id) {
            seeds.push(s);
            seed_hit.push(*at);
        }
    }
    // A neighbor whose definition the brief already shows is not repeated.
    let shown = |file: &str, at: i32| {
        brief.iter().any(|h| {
            let start = line(h, "start_line");
            super::field(h, "file") == file && start <= at && at <= line(h, "end_line").max(start)
        })
    };
    let neighbors =
        match store.related_neighbors(repo, branch, &seeds, hops, RELATED_PER_SEED, &shown) {
            Ok(n) => n,
            Err(e) => {
                return Related::Unavailable(format!(
                    "not shown: reading the symbol graph failed ({e})"
                ))
            }
        };
    Related::Lines(
        neighbors
            .iter()
            .filter_map(|n| {
                let k = seeds.iter().position(|s| s.id == n.seed)?;
                Some(line_of(&seeds[k], seed_hit[k], n))
            })
            .collect(),
    )
}

/// The line a branch without a current symbol graph gets instead of the
/// section.
pub(super) fn unavailable(reader: Reader) -> Option<String> {
    match reader {
        Reader::SymbolGraph => None,
        Reader::Legacy(Legacy::Stale) => Some(
            "not shown: this branch was indexed by an older extractor and has no current \
             symbol graph; run `devctx index --full`"
                .into(),
        ),
        Reader::Legacy(Legacy::NoRows) => Some(
            "not shown: this branch has no symbol rows (no parseable language was indexed)".into(),
        ),
        Reader::Legacy(Legacy::Error) => Some("not shown: reading the symbol graph failed".into()),
    }
}

/// A neighbor's line: its signature as the map shows it, on one line.
pub(super) fn line_of(
    seed: &StoredSymbol,
    hit: usize,
    n: &devctx_store::RelatedNeighbor,
) -> RelatedLine {
    let s = &n.symbol;
    RelatedLine {
        hit,
        around: seed.qualified.clone(),
        relation: n.relation.label(),
        hops: n.hops,
        signature: crate::repo_map::clean_signature(s.signature.as_deref(), &s.kind, &s.name),
        file: s.file.clone(),
        line: s.start_line,
    }
}

#[cfg(test)]
mod tests {
    use super::super::lookup::tests::indexed_files;
    use super::super::{do_build_context_with, pick_branch, AppState};
    use super::*;
    use serde_json::json;

    const SVC: &str = "pub fn run() -> u32 {\n    helper()\n}\n\nfn helper() -> u32 {\n    let body_marker = 41;\n    body_marker + 1\n}\n";
    const CARGO: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
    const MAIN: &str = "mod svc;\n\nfn main() {\n    let _ = svc::run();\n}\n";

    fn fixture(tag: &str) -> (AppState, std::path::PathBuf) {
        indexed_files(
            tag,
            &[
                ("rs/Cargo.toml", CARGO),
                ("rs/src/svc.rs", SVC),
                ("rs/src/main.rs", MAIN),
            ],
        )
    }

    fn run_hit() -> Value {
        json!({ "file": "rs/src/svc.rs", "start_line": 1, "end_line": 3,
                "level": "function", "kind": "code", "language": "rust",
                "text": "pub fn run() -> u32 {\n    helper()\n}" })
    }

    fn gathered(state: &AppState, brief: &[&Value], hops: u8) -> Related {
        let store = state.open_store().unwrap();
        let chosen = pick_branch(state, &store).unwrap();
        gather(&store, &chosen, brief, hops)
    }

    /// On a real index: the caller and the callee of a hit, as signatures,
    /// never the callee's body.
    #[test]
    fn a_hit_shows_its_caller_and_its_callee() {
        let (state, repo) = fixture("rel_real");
        let hit = run_hit();
        let Related::Lines(lines) = gathered(&state, &[&hit], 1) else {
            panic!("a current graph")
        };
        let got: Vec<(&str, &str, &str)> = lines
            .iter()
            .map(|l| (l.around.as_str(), l.relation, l.signature.as_str()))
            .collect();
        assert!(got.contains(&("run", "called by", "fn main()")), "{got:?}");
        assert!(
            got.contains(&("run", "calls", "fn helper() -> u32")),
            "{got:?}"
        );
        assert!(lines.iter().all(|l| !l.signature.contains("body_marker")));
        assert!(
            lines.iter().all(|l| l.file.starts_with("rs/src/")),
            "{lines:?}"
        );
        // A hit that already shows the callee: it is not repeated.
        let helper = json!({ "file": "rs/src/svc.rs", "start_line": 5, "end_line": 8,
                             "level": "function", "kind": "code", "text": "fn helper()" });
        let Related::Lines(lines) = gathered(&state, &[&hit, &helper], 1) else {
            panic!("a current graph")
        };
        assert!(
            lines.iter().all(|l| !l.signature.contains("helper")),
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// The first five code hits that map to a symbol are the seeds: a sixth
    /// one, a hit in a test file and a test function inside a code file get
    /// no neighbors.
    #[test]
    fn the_first_five_code_hits_are_the_seeds() {
        let mut src = String::new();
        for i in 0..7 {
            src.push_str(&format!(
                "pub fn seed{i}() -> u32 {{\n    leaf{i}()\n}}\n\nfn leaf{i}() -> u32 {{\n    {i}\n}}\n\n"
            ));
        }
        // An inline test module of a code file: its function is a test
        // symbol (TASK-010, MAJOR 4) and never a seed.
        src.push_str("#[cfg(test)]\nmod tests {\n    #[test]\n    fn checks() {\n        super::seed0();\n    }\n}\n");
        let test_src = "use demo::seed0;\n\nfn in_test() -> u32 {\n    helper_t()\n}\n\nfn helper_t() -> u32 {\n    1\n}\n";
        let (state, repo) = indexed_files(
            "rel_five",
            &[
                ("rs/Cargo.toml", CARGO),
                ("rs/src/lib.rs", &src),
                ("rs/tests/t.rs", test_src),
            ],
        );
        let hit = |file: &str, start: i64, end: i64| {
            json!({ "file": file, "start_line": start, "end_line": end,
                    "level": "function", "language": "rust", "text": "fn x() {}" })
        };
        let mut brief: Vec<Value> = vec![hit("rs/tests/t.rs", 3, 5), hit("rs/src/lib.rs", 60, 62)];
        for i in 0..7i64 {
            brief.push(hit("rs/src/lib.rs", 1 + 8 * i, 3 + 8 * i));
        }
        for h in &mut brief {
            let kind = devctx_core::path_kind(super::super::field(h, "file"), "rust");
            h["kind"] = json!(kind.as_str());
        }
        let refs: Vec<&Value> = brief.iter().collect();
        let Related::Lines(lines) = gathered(&state, &refs, 1) else {
            panic!("a current graph")
        };
        let mut around: Vec<&str> = lines.iter().map(|l| l.around.as_str()).collect();
        around.dedup();
        assert_eq!(
            around,
            vec!["seed0", "seed1", "seed2", "seed3", "seed4"],
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Lesson 4: with the section off nothing is read; on, one batch.
    #[test]
    fn nothing_is_read_unless_the_section_is_on() {
        let (state, repo) = fixture("rel_reads");
        let sel = devctx_search::KindSel::default();
        // The default (what every call that does not say gets) reads nothing
        // and adds nothing: turning DEFAULT_RELATED on must fail here, on
        // purpose (review of TASK-012, m7).
        GATHERS.with(|c| c.set(0));
        let out = do_build_context_with(
            &state,
            "run helper",
            4096,
            false,
            &sel,
            &ContextOpts::default(),
        )
        .unwrap();
        assert_eq!(GATHERS.with(|c| c.get()), 0, "{out}");
        assert!(
            !out.contains(RELATED_HEADING) && !out.contains(RELATED_NOTE),
            "{out}"
        );
        GATHERS.with(|c| c.set(0));
        let off = ContextOpts {
            related: Some(false),
            graph_hops: None,
        };
        let out = do_build_context_with(&state, "run helper", 4096, false, &sel, &off).unwrap();
        assert_eq!(GATHERS.with(|c| c.get()), 0);
        assert!(
            !out.contains(RELATED_HEADING) && !out.contains(RELATED_NOTE),
            "{out}"
        );
        let on = ContextOpts {
            related: Some(true),
            graph_hops: Some(1),
        };
        let out = do_build_context_with(&state, "run helper", 4096, false, &sel, &on).unwrap();
        assert_eq!(GATHERS.with(|c| c.get()), 1);
        assert!(
            out.contains(RELATED_HEADING) || out.contains(RELATED_NOTE),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Lesson 2: a branch without a current graph shows no section and says
    /// why, with the action.
    #[test]
    fn an_old_index_says_why_there_is_no_section() {
        let (state, repo) = fixture("rel_stale");
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
        let hit = run_hit();
        match gathered(&state, &[&hit], 1) {
            Related::Unavailable(why) => assert!(why.contains("devctx index --full"), "{why}"),
            other => panic!("{other:?}"),
        }
        let on = ContextOpts {
            related: Some(true),
            graph_hops: None,
        };
        let sel = devctx_search::KindSel::default();
        let out = do_build_context_with(&state, "run helper", 4096, false, &sel, &on).unwrap();
        assert!(!out.contains(RELATED_HEADING), "{out}");
        assert!(out.contains("[devctx] related: not shown"), "{out}");
        let _ = std::fs::remove_dir_all(&repo);
        // No symbol rows at all.
        let (state, repo) = indexed_files(
            "rel_norows",
            &[("db/schema.sql", "CREATE TABLE t (id INT);\n")],
        );
        let hit = json!({ "file": "db/schema.sql", "start_line": 1, "end_line": 1,
                          "kind": "code", "text": "CREATE TABLE t (id INT);" });
        match gathered(&state, &[&hit], 1) {
            Related::Unavailable(why) => assert!(why.contains("no symbol rows"), "{why}"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// `graph_hops` is 1 or 2.
    #[test]
    fn graph_hops_is_one_or_two() {
        for bad in [0u8, 3] {
            let e = ContextOpts {
                related: Some(true),
                graph_hops: Some(bad),
            }
            .resolve()
            .unwrap_err();
            assert!(e.contains("graph_hops"), "{e}");
        }
        assert_eq!(ContextOpts::default().resolve(), Ok((DEFAULT_RELATED, 1)));
        let two = ContextOpts {
            related: Some(true),
            graph_hops: Some(2),
        };
        assert_eq!(two.resolve(), Ok((true, 2)));
    }
}
