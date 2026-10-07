//! The extractor's output is versioned, not its code (PLAN-009 DD-3).
//!
//! `EXTRACTOR_VERSION` plus a hash of `languages/*.json` decide whether an
//! index is current. A change to the Rust side of the extraction that alters
//! what a re-parse produces is invisible to the hash, and must bump the
//! version by hand — this test is what makes that impossible to forget. It
//! renders, for a fixture in every language, everything the index persists
//! from a parse, and compares it with the committed snapshot in
//! `tests/golden/extractor.txt`, whose first line records the version it was
//! generated under:
//!
//! - the package;
//! - each symbol: kind, qualified name, id, parent (the `contains` edge),
//!   container, `exported`, lines and signature;
//! - each call: source symbol → target, line, and the source name
//!   `graph_edges` records (`-` for a call with no named function around
//!   it, which `graph_edges` leaves out);
//! - each instantiation and type use, supertype and import, with its source.
//!
//! - Output changed, version not bumped → fails: bump `EXTRACTOR_VERSION`
//!   and regenerate.
//! - Version bumped, snapshot not regenerated → fails: regenerate.
//!
//! Regenerate with `DEVCTX_UPDATE_GOLDEN=1 cargo test -p devctx-parse --test
//! extractor_golden`.

use std::collections::HashMap;

use devctx_parse::{parse, Lang, EXTRACTOR_VERSION};

const GOLDEN: &str = "tests/golden/extractor.txt";
const REGEN: &str = "DEVCTX_UPDATE_GOLDEN=1 cargo test -p devctx-parse --test extractor_golden";

const FIXTURES: &[(&str, &str, &str)] = &[
    (
        "java",
        "java/OrderService.java",
        "src/main/java/com/example/shop/OrderService.java",
    ),
    (
        "typescript",
        "typescript/token.interceptor.ts",
        "libs/auth/src/token.interceptor.ts",
    ),
    ("tsx", "tsx/Button.tsx", "web/Button.tsx"),
    ("javascript", "javascript/app.js", "web/app.js"),
    ("python", "python/orders.py", "shop/orders.py"),
    ("rust", "rust/cache.rs", "crates/demo/src/cache.rs"),
    ("go", "go/server.go", "api/server.go"),
];

fn render() -> String {
    use std::fmt::Write;
    let root = env!("CARGO_MANIFEST_DIR");
    let mut out = String::new();
    let opt = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".into());
    for (lang, fixture, path) in FIXTURES {
        let src = std::fs::read_to_string(format!("{root}/tests/fixtures/{fixture}")).unwrap();
        let mut pf = parse(Lang::named(lang).unwrap(), &src).unwrap();
        pf.assign_ids("repo", path);
        let names: HashMap<u64, String> = std::iter::once(&pf.file_symbol)
            .chain(&pf.symbols)
            .map(|s| (s.id, s.qualified.clone()))
            .collect();
        let name = |id: u64| {
            names
                .get(&id)
                .cloned()
                .unwrap_or_else(|| format!("?{id:x}"))
        };
        writeln!(out, "== {path} ({lang}) package={}", opt(&pf.facts.package)).unwrap();
        for s in std::iter::once(&pf.file_symbol).chain(&pf.symbols) {
            let exported = match s.exported {
                Some(true) => "pub",
                Some(false) => "priv",
                None => "-",
            };
            writeln!(
                out,
                "sym\t{}\t{}\t{}\tparent={}\tcontainer={}\t{exported}\tL{}-{}\t{}",
                s.kind,
                s.qualified,
                devctx_parse::symbol_id::sym_hex(s.id),
                s.parent_id.map(name).unwrap_or_else(|| "-".into()),
                opt(&s.parent),
                s.start_line,
                s.end_line,
                s.signature,
            )
            .unwrap();
        }
        let mut calls: Vec<String> = pf
            .edges
            .iter()
            .map(|e| (e, e.source.as_str()))
            .chain(pf.module_edges.iter().map(|e| (e, "-")))
            .map(|(e, graph)| {
                format!(
                    "{}\tcall\t{} -> {}\tgraph={graph}",
                    e.line,
                    name(e.src_id),
                    e.target
                )
            })
            .collect();
        calls.extend(
            pf.facts
                .refs
                .iter()
                .map(|r| format!("{}\t{}\t{} -> {}", r.line, r.kind, name(r.src_id), r.name)),
        );
        calls.extend(
            pf.facts
                .inherits
                .iter()
                .map(|f| format!("{}\t{}\t{} -> {}", f.line, f.kind, name(f.src_id), f.name)),
        );
        calls.extend(pf.facts.imports.iter().map(|i| {
            format!(
                "{}\timport\t{}\tpath={} name={} alias={} wildcard={}",
                i.line,
                i.target,
                i.path,
                opt(&i.name),
                opt(&i.alias),
                i.wildcard
            )
        }));
        // Line first, then the text: a deterministic order that keeps an
        // edge next to the code it comes from.
        calls.sort_by(|a, b| {
            let line = |s: &str| s.split('\t').next().and_then(|n| n.parse::<u32>().ok());
            line(a).cmp(&line(b)).then_with(|| a.cmp(b))
        });
        for c in calls {
            writeln!(out, "L{c}").unwrap();
        }
    }
    out
}

#[test]
fn the_extractor_output_matches_its_version() {
    let path = format!("{}/{GOLDEN}", env!("CARGO_MANIFEST_DIR"));
    let now = render();
    if std::env::var_os("DEVCTX_UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, format!("version {EXTRACTOR_VERSION}\n{now}")).unwrap();
        return;
    }
    let golden = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("no se pudo leer el golden {path} ({e}): generalo con {REGEN}"));
    let (head, body) = golden.split_once('\n').unwrap_or(("", ""));
    let version: Option<u32> = head.strip_prefix("version ").and_then(|v| v.parse().ok());
    if body != now {
        assert_ne!(
            version,
            Some(EXTRACTOR_VERSION),
            "la salida del extractor cambió: subí EXTRACTOR_VERSION y regenerá el golden ({REGEN})"
        );
        panic!("EXTRACTOR_VERSION cambió y el golden no: regenerá el golden ({REGEN})");
    }
    assert_eq!(
        version,
        Some(EXTRACTOR_VERSION),
        "EXTRACTOR_VERSION cambió y el golden no: regenerá el golden ({REGEN})"
    );
}
