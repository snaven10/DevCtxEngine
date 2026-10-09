//! Rust: `impl<T> Foo<T>`, `impl Trait for T`, `Foo::bar()`, consts, type
//! aliases, fields, `use` trees (PLAN-009 TASK-004).

mod common;
use common::*;
use devctx_parse::Lang;

const PATH: &str = "crates/demo/src/cache.rs";

/// `impl<T: Clone> Cached<T>` names its methods `Cached.m`, never
/// `Cached<T>.m` (master §2 H5) — in the symbols and in `graph_edges`.
#[test]
fn a_bounded_generic_impl_names_its_methods_after_the_bare_type() {
    let pf = facts(Lang::rust(), "rust/cache.rs", PATH);
    for s in &pf.symbols {
        assert!(!s.qualified.contains('<'), "{s:?}");
    }
    let new = sym(&pf, "method", "Cached.new");
    assert_eq!(new.parent.as_deref(), Some("Cached"));
    assert_eq!(new.signature, "pub fn new(inner: T) -> Self");
    assert!(pf.edges.iter().all(|e| !e.source.contains('<')));
    assert!(pf.edges.iter().any(|e| e.source == "Cached.new"));
}

#[test]
fn rust_symbols_impls_consts_and_fields() {
    let pf = facts(Lang::rust(), "rust/cache.rs", PATH);
    assert_eq!(pf.facts.package.as_deref(), Some("demo::cache"));
    assert_eq!(
        outline(&pf),
        [
            "const LIMIT",
            "const NAME",
            "type Map",
            "struct Cached",
            "field Cached.inner",
            "field Cached.hits",
            "trait Named",
            // A trait method without a body is a symbol (TASK-007).
            "method Named.name",
            "impl Cached",
            "method Cached.new",
            "method Cached.build",
            "method Cached.build.tag",
            "impl Cached",
            "method Cached.name",
        ]
    );
    let impls: Vec<_> = pf.symbols.iter().filter(|s| s.kind == "impl").collect();
    assert_eq!(impls[0].signature, "impl<T: Clone> Cached<T>");
    assert_eq!(impls[1].trait_of.as_deref(), Some("Named"));
    assert_ne!(impls[0].id, impls[1].id);
    let cached = sym(&pf, "struct", "Cached");
    assert!(impls.iter().all(|i| i.parent_id == Some(cached.id)));
    assert_eq!(
        sym(&pf, "method", "Cached.name").parent_id,
        Some(impls[1].id)
    );
    assert_eq!(sym(&pf, "const", "LIMIT").exported, Some(true));
    assert_eq!(sym(&pf, "const", "NAME").exported, Some(false));
    assert_eq!(sym(&pf, "field", "Cached.hits").exported, Some(false));
    assert_eq!(
        inherits(&pf),
        [
            t3("inherits", "Clone", "Named"),
            t3("inherits", "Send", "Named"),
            t3("implements", "Named", "Cached"),
        ]
    );
}

/// A path call qualifies by its path: a type (or `Self`) as `Type.m`, a
/// module as `path::f` (it used to be the bare name, master §2 H5).
#[test]
fn rust_path_calls_keep_their_qualifier() {
    let pf = facts(Lang::rust(), "rust/cache.rs", PATH);
    let c = calls(&pf);
    for (src, dst) in [
        ("Cached.new", "Cached.build"),
        ("Cached.name", "Foo.bar"),
        ("Cached.name", "store::open"),
        ("Cached.name", "String.new"),
    ] {
        assert!(c.contains(&t2(src, dst)), "{src} -> {dst}: {c:?}");
    }
    let r = refs(&pf);
    assert!(r.contains(&t3("instantiates", "Cached", "Cached.build")));
    assert!(r.contains(&t3("references", "String", "Cached.name")));
    assert!(
        !r.iter().any(|(_, n, _)| n == "T"),
        "a type parameter is no reference: {r:?}"
    );
}

#[test]
fn rust_use_trees_are_flattened() {
    let pf = facts(Lang::rust(), "rust/cache.rs", PATH);
    assert_eq!(
        import_targets(&pf),
        [
            "std::collections::HashMap",
            "crate::store",
            "crate::store::Store",
            "crate::store::model::*",
        ]
    );
    assert_eq!(pf.facts.imports[2].alias.as_deref(), Some("S"));
}

/// Review of TASK-010, second pass: a `cfg` marks test code only when `test`
/// is required — alone, or inside an `all(…)` — never under `any(…)` or
/// `not(…)`, nor as part of another word or a feature name. Production code
/// must never be taken for a test (it would leave `impact` and the rank).
/// Cheap false negatives fixed too: `#[tokio::test(flavor = …)]`,
/// `#[rstest]`, a comment between the attribute and the item, `#![cfg(test)]`.
#[test]
fn only_a_required_test_cfg_marks_test_code() {
    let cases: &[(&str, bool)] = &[
        ("#[cfg(test)]", true),
        ("#[cfg(all(test, feature = \"x\"))]", true),
        ("#[cfg(all(unix, all(test, feature = \"x\")))]", true),
        ("#[cfg(any(test, feature = \"x\"))]", false),
        ("#[cfg(not(test))]", false),
        ("#[cfg(feature = \"test-utils\")]", false),
        ("#[cfg(not(feature = \"test-utils\"))]", false),
        ("#[cfg(attest)]", false),
        ("#[cfg(all(unix, any(test, feature = \"x\")))]", false),
        ("#[cfg(not(all(test, unix)))]", false),
        ("#[test]", true),
        ("#[tokio::test(flavor = \"multi_thread\")]", true),
        ("#[rstest]", true),
        ("#[cfg(test)]\n// why this exists", true),
        ("#[inline]", false),
    ];
    for (attr, want) in cases {
        let src =
            format!("pub fn before() {{}}\n\n{attr}\npub fn target() {{\n    before();\n}}\n");
        let pf = devctx_parse::parse(Lang::rust(), &src).unwrap();
        let t = pf.symbols.iter().find(|s| s.qualified == "target").unwrap();
        let marked = pf
            .facts
            .test_lines
            .iter()
            .any(|&(a, b)| a <= t.start_line && t.end_line <= b);
        assert_eq!(marked, *want, "{attr}: {:?}", pf.facts.test_lines);
    }
    // An inner `#![cfg(test)]` makes the whole file test code.
    let pf = devctx_parse::parse(Lang::rust(), "#![cfg(test)]\n\npub fn helper() {}\n").unwrap();
    assert_eq!(
        pf.facts.test_lines,
        [(1, pf.file_symbol.end_line)],
        "{:?}",
        pf.facts.test_lines
    );
}
