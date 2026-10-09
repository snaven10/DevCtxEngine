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

const REVIEW_SRC: &str = "crates/app/src/review.rs";
const FAILURE: &str = "crates/store/src/failure.rs";

/// Review R1: a crate of the workspace counts only as the file's own or a
/// path dependency (mapped by its path, renamed by `package`, inherited from
/// the workspace); a dependency the manifest declares from the registry is
/// external though the workspace has a crate of that name; a workspace crate
/// nobody declares is undecided.
#[test]
fn declared_dependencies_decide_which_crate() {
    let l = linked();
    assert_eq!(l.call("crates", "utils::tidy_up"), EXTERNAL);
    assert_eq!(
        l.call("crates", "store2::helpers::make"),
        (s("make"), s(HELPERS), "high", "import", false)
    );
    assert_eq!(
        l.call("crates", "Store.new"),
        (s("Store.new"), s(STORE), "high", "import", false)
    );
    assert_eq!(l.call("crates", "orphan::lone"), UNDECIDED);
    let _ = REVIEW_SRC;
}

/// Review R2: with several bounds, the one whose trait has the method.
#[test]
fn every_bound_is_tried() {
    let l = linked();
    for f in ["first", "second", "third"] {
        assert_eq!(
            l.call(f, "name"),
            (s("Named.name"), s(OPS), "high", "param", false),
            "{f}"
        );
    }
}

/// Review R3: two `From` impls give no `high`; an inherent method wins over
/// a trait's of the same name.
#[test]
fn trait_impl_homonyms_are_not_sure() {
    let l = linked();
    assert_eq!(
        l.call("overloads", "Failure.from").2,
        "medium",
        "{:?}",
        l.call("overloads", "Failure.from")
    );
    assert_eq!(
        l.call("overloads", "Failure.name"),
        (s("Failure.name"), s(FAILURE), "high", "param", false)
    );
}

/// Review R4: two definitions of a name (`#[cfg]`) are no sure destination.
#[test]
fn cfg_homonyms_are_medium() {
    let l = linked();
    assert_eq!(
        l.call("platform", "demo_store::helpers::platform"),
        (s("platform"), s(HELPERS), "medium", "import", false)
    );
}

/// Review R5: a `use` inside a function is that function's.
#[test]
fn a_use_in_a_function_is_scoped_to_it() {
    let l = linked();
    assert_eq!(
        l.call("t1", "polish"),
        (s("polish"), s(UTIL), "high", "import", false)
    );
    assert_eq!(
        l.call("t2", "polish"),
        (
            s("polish"),
            s("crates/app/src/extra.rs"),
            "high",
            "import",
            false
        )
    );
}

/// Review R6: a method missing from a type with an external supertype
/// (`impl Display`) is external, `medium`.
#[test]
fn an_external_supertype_is_medium_evidence() {
    let l = linked();
    assert_eq!(
        l.call("missing", "Store.render"),
        (None, None, "medium", "inherited", true)
    );
}

/// Review minors: `impl Ext for serde_json::Value` gives the repository's
/// `Value` nothing; `Arc::new(T::f()?)` holds a `T`; a nested function
/// shadows the imported one; a capitalised associated function of an enum
/// is no variant; `format!` is no call.
#[test]
fn owners_values_and_variants() {
    let l = linked();
    assert_eq!(
        l.call("not_mine", "demo_store.helpers.Value.ext"),
        UNDECIDED
    );
    let saves = l.all("calls", "values", "save");
    assert_eq!(saves[0], (s("Store.save"), s(OPS), "high", "local", false));
    assert_eq!(saves[1], (s("Store.save"), s(OPS), "high", "local", false));
    assert_eq!(
        l.call("modes", "Mode.Parse"),
        (s("Mode.Parse"), s(HELPERS), "high", "import", false)
    );
    assert!(!l
        .edges
        .iter()
        .any(|e| e.kind == "calls" && e.dst_name == "format"));
}

/// Review measurement: `T::default()` that no `impl` of a repository type
/// defines (a derive) holds a `T` (`medium`: nothing declares it).
#[test]
fn a_derived_default_is_its_type() {
    let l = linked();
    assert_eq!(
        l.call("defaults", "save"),
        (s("Store.save"), s(OPS), "medium", "local", false)
    );
}

/// Review measurement: `let x = a.f()` makes a call on `x` a chain after
/// `a.f()`, typed by `f`'s declared return type.
#[test]
fn a_local_from_a_method_call_is_a_chain() {
    let l = linked();
    let infos = l.all("calls", "chained", "info");
    assert_eq!(infos.len(), 2, "{infos:?}");
    for i in infos {
        assert_eq!(
            i,
            (s("Logger.info"), s(HELPERS), "high", "return_type", false)
        );
    }
}

/// Review measurement: a test crate's `mod common;` is its
/// `tests/common/mod.rs`.
#[test]
fn a_test_crate_reaches_its_common_module() {
    let l = linked();
    assert_eq!(
        l.call("seeds", "seed"),
        (
            s("seed"),
            s("crates/store/tests/common/mod.rs"),
            "high",
            "import",
            false
        )
    );
}

/// Second review N3: a Rust type nothing resolves (`mystery::Thing`) types
/// nothing; the call is undecided, never `unique_name` against the
/// repository's only `tidy_up`.
#[test]
fn an_unresolved_type_is_no_guess() {
    let l = linked();
    assert_eq!(l.call("mystery_typed", "mystery.Thing.tidy_up"), UNDECIDED);
}

/// Second review: a `use` inside a closure (a block) is that block's.
#[test]
fn a_use_in_a_block_is_scoped_to_it() {
    let l = linked();
    let calls = l.all("calls", "closure_use", "polish");
    assert_eq!(
        calls[0],
        (
            s("polish"),
            s("crates/app/src/extra.rs"),
            "high",
            "import",
            false
        )
    );
    assert_eq!(
        calls[1],
        (s("polish"), s(REVIEW_SRC), "high", "same_file", false)
    );
}

/// Second review: a file under no manifest — a dependency an ancestor
/// manifest declares from the registry wins over a workspace crate of that
/// name.
#[test]
fn without_a_manifest_the_ancestor_declaration_wins() {
    let l = link_dir_with(DIR, |env| {
        env.cargo.retain(|m| m.krate.as_deref() != Some("app"));
        let root = env.cargo.iter_mut().find(|m| m.krate.is_none()).unwrap();
        root.deps.push("utils".into());
        root.deps.sort();
    });
    assert_eq!(l.call("crates", "utils::tidy_up"), EXTERNAL);
}

/// Second review: two manifests giving one crate name (nested workspaces)
/// do not merge; a path dependency still reaches its own.
#[test]
fn homonymous_crates_keep_apart() {
    let l = link_dir_with(DIR, |env| {
        let mut twin = env
            .cargo
            .iter()
            .find(|m| m.krate.as_deref() == Some("demo_store"))
            .unwrap()
            .clone();
        twin.dir = "nested/store".into();
        twin.path = "nested/store/Cargo.toml".into();
        env.cargo.push(twin);
    });
    assert_eq!(
        l.call("main", "Store.open"),
        (s("Store.open"), s(STORE), "high", "import", false)
    );
}

/// Second review: `use super::super::*` from a top-level inline `mod` is
/// another module's glob: the importer filter reopens the whole file.
#[test]
fn a_glob_past_the_file_blocks_the_importer_filter() {
    let l = linked();
    assert!(l.index.rs_use_names(UTIL).is_none());
    assert!(l.index.rs_use_names(MAIN).is_some());
}

/// Third review: `self.m()` in an `impl` for a type from outside (no
/// container in the repository) is undecided, never `unique_name`.
#[test]
fn self_in_an_impl_for_an_outside_type_is_no_guess() {
    let l = linked();
    assert_eq!(l.call("Vec.size", "Vec.tidy_up"), UNDECIDED);
}

/// Third review (NIT): a receiver named by a `use` inside a closure is
/// resolved by the call's line too, as a bare call is.
#[test]
fn a_receiver_use_in_a_block_is_scoped_to_it() {
    let l = linked();
    let calls = l.all("calls", "closure_receiver", "P.shine");
    assert_eq!(
        calls[0],
        (
            s("Polisher.shine"),
            s("crates/app/src/extra.rs"),
            "high",
            "import",
            false
        )
    );
    assert_eq!(calls[1], UNDECIDED);
}

/// Review of TASK-008 (m6): in an `impl` of a repository trait for a type
/// from outside, `self.m()` is the trait's method (its supertype), not a
/// guess by name and not undecided.
#[test]
fn self_in_an_impl_of_a_repo_trait_reaches_the_trait() {
    let l = linked();
    assert_eq!(
        l.call("String.wave", "String.hello"),
        (
            s("Greeter.hello"),
            s("crates/app/src/review.rs"),
            "medium",
            "inherited",
            false
        )
    );
    // Second review (N2): an inherent method of the outside type would win,
    // and none can be known, so the trait is `medium` ...
    assert_eq!(
        l.call("PathBuf.report", "PathBuf.len"),
        (
            s("Measured.len"),
            s("crates/app/src/review.rs"),
            "medium",
            "inherited",
            false
        )
    );
    // ... and a method of the `impl` itself (an override) wins, `high`.
    assert_eq!(
        l.call("u32.greet", "u32.bow"),
        (
            s("u32.bow"),
            s("crates/app/src/review.rs"),
            "high",
            "same_file",
            false
        )
    );
    // The outside type's own method stays undecided.
    assert_eq!(l.call("Vec.size", "Vec.tidy_up"), UNDECIDED);
}
