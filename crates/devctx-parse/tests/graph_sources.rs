//! Every source `graph_edges` records is the qualified name of the symbol the
//! call is in (PLAN-009 TASK-004 review, m-c and NIT).
//!
//! `impact_analysis` and `get_references` read `graph_edges` by name, so a
//! source that names no symbol — or another symbol than the one the `edges`
//! row points at — loses the caller. A named callable used to be sourced as
//! `Container.name` whatever scopes sat between: a Python decorator's
//! `wrapper` was `wrapper` (the symbol is `traced.wrapper`), a Java anonymous
//! class's `run` was `OrderService.run` (it is `OrderService.init.run`), a
//! function nested in a method was `C.f` (it is `C.m.f`), and `const x =
//! function named() {}` was `named` (the symbol is `x`).

mod common;
use common::*;
use devctx_parse::Lang;

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

/// In every fixture, each `graph_edges` source is the qualified name of a
/// symbol, and of the very symbol the `edges` row is sourced from.
#[test]
fn every_graph_source_is_the_qualified_name_of_its_symbol() {
    for (lang, fixture, path) in FIXTURES {
        let pf = facts(Lang::named(lang).unwrap(), fixture, path);
        let by_id = names(&pf);
        let qualified: Vec<&str> = pf.symbols.iter().map(|s| s.qualified.as_str()).collect();
        // A fixture that lost its calls must not pass by checking nothing.
        assert!(!pf.edges.is_empty(), "{lang}: no graph_edges calls at all");
        for e in &pf.edges {
            assert!(
                qualified.contains(&e.source.as_str()),
                "{lang}: graph source {} (L{} -> {}) is no symbol",
                e.source,
                e.line,
                e.target
            );
            assert_eq!(
                by_id[&e.src_id], e.source,
                "{lang}: L{} -> {}: graph_edges and edges disagree on the source",
                e.line, e.target
            );
        }
    }
}

/// The cases the fixtures carry, named.
#[test]
fn named_callables_are_sourced_by_their_qualified_name() {
    let source_of = |lang: &str, fixture: &str, path: &str, target: &str| -> Vec<String> {
        facts(Lang::named(lang).unwrap(), fixture, path)
            .edges
            .iter()
            .filter(|e| e.target.ends_with(target))
            .map(|e| e.source.clone())
            .collect()
    };
    assert_eq!(
        source_of("python", "python/orders.py", "shop/orders.py", "record"),
        ["traced.wrapper"]
    );
    assert_eq!(
        source_of("python", "python/orders.py", "shop/orders.py", "audit"),
        ["OrderService._hidden.step"]
    );
    assert_eq!(
        source_of(
            "java",
            "java/OrderService.java",
            "src/main/java/com/example/shop/OrderService.java",
            "flush"
        ),
        ["OrderService.init.run"]
    );
    assert_eq!(
        source_of("rust", "rust/cache.rs", "crates/demo/src/cache.rs", "mark"),
        ["Cached.build.tag"]
    );
    assert_eq!(
        source_of("javascript", "javascript/app.js", "web/app.js", "notify"),
        ["onDone"]
    );
}
