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
