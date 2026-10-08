//! The Rust resolver (PLAN-009 TASK-007, DD-7): a two-crate workspace —
//! `use` (groups, `self`, aliases, globs), `pub use` re-exports, `mod`,
//! `crate::`/`super::`, `Self::`, a type's `impl`s in other files, `impl
//! Trait for T`, workspace crates and declared dependencies, macros —
//! parsed and linked as the indexer does, without a store.

mod common;
use common::linked::*;

const DIR: &str = "rust/link";
const STORE: &str = "crates/store/src/store.rs";
const OPS: &str = "crates/store/src/ops.rs";
const HELPERS: &str = "crates/store/src/helpers.rs";
const MAIN: &str = "crates/app/src/main.rs";
const UTIL: &str = "crates/app/src/util.rs";

fn linked() -> Linked {
    link_dir(DIR)
}

/// Step 1: `Store::open(…)` reaches `Store.open` through the `use` of a
/// workspace crate and its `pub use`; `Self::helper()` is the type's.
#[test]
fn paths_resolve_by_use_and_self() {
    let l = linked();
    assert_eq!(
        l.call("main", "Store.open"),
        (s("Store.open"), s(STORE), "high", "import", false)
    );
    assert_eq!(
        l.call("main", "Store.new"),
        (s("Store.new"), s(STORE), "high", "import", false)
    );
    assert_eq!(
        l.call("Store.new", "Store.helper"),
        (s("Store.helper"), s(STORE), "high", "self", false)
    );
    assert_eq!(
        l.import(MAIN, "demo_store::Store"),
        (s("Store"), s(STORE), "high", "import", false)
    );
    assert_eq!(
        l.import(MAIN, "demo_store::store::Store"),
        (s("Store"), s(STORE), "high", "import", false)
    );
    // `use util::{self, …}`: the module.
    assert_eq!(
        l.import(MAIN, "util"),
        (None, None, "high", "import", false)
    );
}

/// Step 1: a method of `impl Trait for T` is `T.m` and the `impl` gives
/// `T` its supertype; a type's methods are found in every `impl` of the
/// crate, in other files too; a trait's default method calls its own.
#[test]
fn impls_in_other_files_and_trait_impls() {
    let l = linked();
    assert_eq!(
        l.call("main", "save"),
        (s("Store.save"), s(OPS), "high", "local", false)
    );
    assert_eq!(
        l.call("main", "name"),
        (s("Store.name"), s(OPS), "high", "local", false)
    );
    assert_eq!(
        l.call("main", "get"),
        (s("Store.get"), s(STORE), "high", "local", false)
    );
    assert_eq!(
        l.call("Named.shout", "Named.name"),
        (s("Named.name"), s(OPS), "high", "self", false)
    );
    let implements = l.all("implements", "Store", "Named");
    assert_eq!(
        implements[0],
        (s("Named"), s(OPS), "high", "same_file", false)
    );
    assert_eq!(l.all("implements", "Store", "fmt::Display")[0], EXTERNAL);
    // A trait method without a body is a symbol of the trait.
    assert!(l
        .symbols
        .iter()
        .any(|s| s.qualified == "Named.name" && s.kind == "method"));
}

/// Step 1: `serde_json::from_str` and `tokio::spawn` are external — crates
/// the manifests declare, though the repository has a `spawn` of its own;
/// `std` is external; a crate nothing declares is undecided, never the
/// repository's homonym.
#[test]
fn declared_crates_and_std_are_external() {
    let l = linked();
    assert_eq!(l.call("main", "serde_json::from_str"), EXTERNAL);
    assert_eq!(l.call("main", "tokio::spawn"), EXTERNAL);
    assert_eq!(l.call("Store.new", "HashMap.new"), EXTERNAL);
    assert_eq!(l.call("Store.name", "String.from"), EXTERNAL);
    // A field typed `HashMap<…>` (from `std`).
    assert_eq!(l.call("Store.get", "get"), EXTERNAL);
    // `vec![…]` is a `Vec`.
    assert_eq!(l.call("main", "Vec.len"), EXTERNAL);
    assert_eq!(l.call("main", "Thing.go"), UNDECIDED);
    assert_eq!(l.import(MAIN, "mystery::Thing"), UNDECIDED);
}

/// Macros are no calls: `format!`, `println!`, `vec!`, `write!` and
/// `#[tokio::main]` leave no edge.
#[test]
fn macros_are_not_calls() {
    let l = linked();
    for name in ["format", "println", "vec", "write", "main", "tokio::main"] {
        assert!(
            !l.edges
                .iter()
                .any(|e| e.kind == "calls" && e.dst_name == name),
            "{name} is no call"
        );
    }
}

/// `crate::`/`super::`, module paths, `use` of a function, a `use
/// super::*` scoped to its inline `mod`, a function of the file.
#[test]
fn modules_and_functions() {
    let l = linked();
    assert_eq!(
        l.call("main", "util::tidy"),
        (s("tidy"), s(UTIL), "high", "import", false)
    );
    assert_eq!(
        l.call("main", "tidy"),
        (s("tidy"), s(UTIL), "high", "import", false)
    );
    assert_eq!(
        l.call("main", "helper_free"),
        (s("helper_free"), s(MAIN), "high", "same_file", false)
    );
    assert_eq!(
        l.call("tests.tidies", "tidy"),
        (s("tidy"), s(UTIL), "high", "same_file", false)
    );
    assert_eq!(
        l.call("main", "demo_store::helpers::make"),
        (s("make"), s(HELPERS), "high", "import", false)
    );
    // The declared return type of `make` types `.run()`.
    assert_eq!(
        l.call("main", "run"),
        (s("Svc.run"), s(HELPERS), "high", "return_type", false)
    );
}

/// `let x = T::f()?` (and `.unwrap()`) is what `f` returns, unwrapped;
/// fields are typed by their declarations; `self.x.y()`.
#[test]
fn values_and_fields() {
    let l = linked();
    assert_eq!(
        l.call("opener", "save"),
        (s("Store.save"), s(OPS), "high", "local", false)
    );
    let saves = l.all("calls", "Wrapper.go", "save");
    assert_eq!(saves.len(), 4, "{saves:?}");
    assert_eq!(saves[0], (s("Store.save"), s(OPS), "high", "field", false));
    assert_eq!(
        l.call("Wrapper.go", "Store.save"),
        (s("Store.save"), s(OPS), "high", "param", false)
    );
    assert_eq!(
        l.call("Store.get", "info"),
        (s("Logger.info"), s(HELPERS), "high", "field", false)
    );
}

/// Lesson 3: an `if let`, a closure parameter and a `match` binding shadow
/// the parameter and the field of the same name: untyped, never `Store`.
#[test]
fn patterns_and_closures_shadow() {
    let l = linked();
    let saves = l.all("calls", "Wrapper.go", "save");
    for s in &saves[1..] {
        assert_eq!(*s, UNDECIDED);
    }
    // A closure parameter calling a name the repository lacks.
    assert_eq!(l.call("main", "count_ones"), DISCARDED);
}

/// Without the manifests no crate is the workspace's nor declared: the
/// paths into them are undecided, not external and not guessed.
#[test]
fn a_crate_needs_a_manifest() {
    let l = link_dir_with(DIR, |env| env.cargo.clear());
    assert_eq!(l.call("main", "serde_json::from_str"), UNDECIDED);
    assert_eq!(l.call("main", "Store.open"), UNDECIDED);
}
