//! The Python resolver (PLAN-009 TASK-007, DD-7): a package with relative and
//! absolute imports, a re-exporting `__init__.py`, instance attributes typed
//! in `__init__`, stdlib and declared packages, builtins, module-level calls
//! and scripts — parsed and linked as the indexer does, without a store.

mod common;
use common::linked::*;

const DIR: &str = "python/link";
const REPO: &str = "shop/repo.py";
const LOOKUP: &str = "shop/lookup.py";
const SERVICE: &str = "shop/service.py";
const SPECIAL: &str = "shop/special.py";

fn linked() -> Linked {
    link_dir(DIR)
}

/// Step 1: two calls to `helper()` from one method are two edges, both to
/// the function of the file (not the homonym of another module).
#[test]
fn two_calls_from_one_method_are_two_edges() {
    let l = linked();
    let both = l.all("calls", "Service.run", "helper");
    assert_eq!(both.len(), 2, "{both:?}");
    for b in both {
        assert_eq!(b, (s("helper"), s(SERVICE), "high", "same_file", false));
    }
}

/// Step 1: calls at module level are edges from the file symbol (or from
/// the module constant whose value they compute), and they resolve like any
/// other (`x = make()` types `x` by `make`'s annotation).
#[test]
fn module_level_calls_are_edges_of_the_file() {
    let l = linked();
    assert_eq!(
        l.call("x", "make"),
        (s("make"), s(SERVICE), "high", "same_file", false)
    );
    assert_eq!(
        l.call(SERVICE, "save"),
        (s("Repo.save"), s(REPO), "high", "local", false)
    );
    // Under `if __name__ == "__main__":`.
    assert_eq!(
        l.call(SERVICE, "helper"),
        (s("helper"), s(SERVICE), "high", "same_file", false)
    );
    assert_eq!(l.call("logger", "getLogger"), EXTERNAL);
}

/// Step 1: `from .lookup import lookup_id` (relative), a star import, a
/// module imported by `from . import repo as repo_mod`, and a package
/// `__init__.py` that re-exports what an absolute import names.
#[test]
fn relative_star_and_reexported_imports() {
    let l = linked();
    assert_eq!(
        l.call("Service.run", "lookup_id"),
        (s("lookup_id"), s(LOOKUP), "high", "import", false)
    );
    assert_eq!(
        l.call("Special.save", "lookup_id"),
        (s("lookup_id"), s(LOOKUP), "high", "import", false)
    );
    assert_eq!(
        l.import(SERVICE, "shop.Repo"),
        (s("Repo"), s(REPO), "high", "import", false)
    );
    assert_eq!(
        l.import(SERVICE, ".lookup.lookup_id"),
        (s("lookup_id"), s(LOOKUP), "high", "import", false)
    );
    // A module: no symbol, but the repository's.
    assert_eq!(
        l.import(SERVICE, ".repo"),
        (None, None, "high", "import", false)
    );
    // A call to a class is its instantiation: the class.
    assert_eq!(
        l.call("Service.run", "Repo"),
        (s("Repo"), s(REPO), "high", "import", false)
    );
    // A chain typed by the class, then by a declared return type (a
    // string annotation).
    assert_eq!(
        l.call("Service.run", "load"),
        (s("Repo.load"), s(REPO), "high", "return_type", false)
    );
    assert_eq!(
        l.call("Service.run", "price"),
        (s("Item.price"), s(REPO), "high", "return_type", false)
    );
}

/// Step 1: `self.repo = repo` in `__init__` (a typed parameter) and
/// `self.repo = Repo()` type the attribute; `self.x()` is the class's.
#[test]
fn instance_attributes_are_typed_in_init() {
    let l = linked();
    assert_eq!(
        l.call("Service.run", "Repo.save"),
        (s("Repo.save"), s(REPO), "high", "ctor_inject", false)
    );
    // `self.cache = Repo()`.
    assert_eq!(
        l.call("Service.run", "save"),
        (s("Repo.save"), s(REPO), "high", "field", false)
    );
    assert_eq!(
        l.call("Plain.go", "save"),
        (s("Repo.save"), s(REPO), "high", "field", false)
    );
    assert_eq!(
        l.call("Service.run", "Service.again"),
        (s("Service.again"), s(SERVICE), "high", "self", false)
    );
    // An attribute assigned an untyped parameter: nothing types it, and the
    // repository defines no `strip`.
    assert_eq!(l.call("Service.run", "strip"), DISCARDED);
}

/// Step 1: builtins, the standard library and declared packages are
/// external; a declared package is external though the repository has a
/// module of that name.
#[test]
fn builtins_stdlib_and_declared_packages_are_external() {
    let l = linked();
    assert_eq!(l.call("Service.run", "len"), EXTERNAL);
    assert_eq!(l.call("Service.run", "dict.get"), EXTERNAL);
    assert_eq!(l.call("Service.run", "join"), EXTERNAL);
    assert_eq!(l.call("Service.run", "info"), EXTERNAL);
    assert_eq!(l.call("Service.run", "get"), EXTERNAL);
    // `yaml` is declared (PyYAML) and also a module of the package.
    assert_eq!(l.call("Service.run", "safe_load"), EXTERNAL);
    assert_eq!(l.import(SERVICE, "yaml"), EXTERNAL);
    assert_eq!(l.import(SERVICE, "requests"), EXTERNAL);
    // The value of a call into the standard library: after an external
    // call nothing types it, and `warning` is no repository name.
    assert_eq!(
        l.call("Service.run", "warning"),
        (None, None, "medium", "chain_external", true)
    );
}

/// Lesson 2: an import that cannot be followed (a relative module with no
/// file, a package nothing declares) is undecided — never the repository's
/// homonym by name.
#[test]
fn an_unfollowable_import_is_never_a_homonym() {
    let l = linked();
    assert_eq!(l.call("Service.run", "ghost"), UNDECIDED);
    assert_eq!(l.call("Service.run", "thing"), UNDECIDED);
    assert_eq!(l.import(SERVICE, ".missing.ghost"), UNDECIDED);
    assert_eq!(l.import(SERVICE, "unknownpkg.thing"), UNDECIDED);
}

/// Lesson 3: a parameter, a lambda parameter and a comprehension variable
/// shadow the attribute of the same name: untyped, never `Repo`.
#[test]
fn local_bindings_shadow_attributes() {
    let l = linked();
    let saves = l.all("calls", "Service.shadow", "save");
    assert_eq!(saves.len(), 3, "{saves:?}");
    for s in saves {
        assert_eq!(s, UNDECIDED);
    }
    // A parameter holding a function: nothing types it, and the repository
    // defines no `cb`.
    assert_eq!(l.call("Service.run", "cb"), DISCARDED);
}

/// Inheritance: `super()`, an inherited method, a builtin base class; and
/// nested functions of sibling functions are each their own.
#[test]
fn inheritance_and_nested_functions() {
    let l = linked();
    assert_eq!(
        l.call("Special.save", "Special.save"),
        (s("Repo.save"), s(REPO), "high", "inherited", false)
    );
    assert_eq!(
        l.call("Special.save", "Special.load"),
        (s("Repo.load"), s(REPO), "high", "inherited", false)
    );
    assert_eq!(
        l.call("Failure.go", "Failure.with_traceback"),
        (None, None, "high", "inherited", true)
    );
    assert_eq!(
        l.call("first", "inner"),
        (s("first.inner"), s(SPECIAL), "high", "same_file", false)
    );
    assert_eq!(
        l.call("second", "inner"),
        (s("second.inner"), s(SPECIAL), "high", "same_file", false)
    );
}

/// A script outside any package imports a sibling module by its directory
/// (the script's own on `sys.path`) and the package by the repository root.
#[test]
fn a_script_imports_its_siblings_and_the_package() {
    let l = linked();
    assert_eq!(
        l.call("tools/run.py", "assist"),
        (s("assist"), s("tools/helpers.py"), "high", "import", false)
    );
    assert_eq!(
        l.call("tools/run.py", "Service"),
        (s("Service"), s(SERVICE), "high", "import", false)
    );
    assert_eq!(
        l.call("tools/run.py", "again"),
        (s("Service.again"), s(SERVICE), "high", "return_type", false)
    );
    assert_eq!(l.call("tools/run.py", "exit"), EXTERNAL);
}

/// Without its manifest a package is not declared: `requests` is no longer
/// external (no evidence).
#[test]
fn a_package_needs_a_manifest() {
    let l = link_dir_with(DIR, |env| env.python.clear());
    assert_eq!(l.call("Service.run", "get"), UNDECIDED);
    assert_eq!(l.import(SERVICE, "requests"), UNDECIDED);
}

/// Review measurement: `x = self.repo.load()` makes a call on `x` a chain
/// after it, typed by `load`'s annotation.
#[test]
fn a_local_from_a_method_call_is_a_chain() {
    let l = linked();
    assert_eq!(
        l.call("Service.chained", "price"),
        (s("Item.price"), s(REPO), "high", "return_type", false)
    );
}

const FORMS: &str = "shop/typing_forms.py";

/// Review P1: `Any`, `object`, a `TypeVar` and a `Union` of several types
/// type nothing (never `external_known` high, never a guess by name);
/// `type[X]` is `X`; `-> Self` is the class.
#[test]
fn typing_special_forms() {
    let l = linked();
    for f in ["anything", "either", "obj"] {
        assert_eq!(l.call(f, "save"), UNDECIDED, "{f}");
    }
    assert_eq!(l.call("generic", "T.save"), UNDECIDED);
    assert_eq!(
        l.call("klass", "Repo.load"),
        (s("Repo.load"), s(REPO), "high", "param", false)
    );
    assert_eq!(
        l.call("Builder.go", "done"),
        (s("Builder.done"), s(FORMS), "high", "return_type", false)
    );
}

/// Review P3: a module named like the standard library, or like a name
/// guessed from a distribution (`python-utils` → `utils`), that the
/// repository has: never external with that evidence.
#[test]
fn weak_external_evidence_loses_to_the_repository() {
    let l = linked();
    assert_eq!(l.call("tools/cal.py", "plan"), UNDECIDED);
    assert_eq!(
        l.call("tools/cal.py", "helper2"),
        (s("helper2"), s("utils.py"), "high", "import", false)
    );
    // An exact declared name still wins (`yaml`, `requests`).
    assert_eq!(l.import(SERVICE, "requests"), EXTERNAL);
}

/// Review minors: an import in `try`/`except ImportError` bound twice is no
/// sure destination; decorators; `import a.b as c`, `import a.b`, `from ..a
/// import b`, and `from ...` past the top package.
#[test]
fn import_forms_and_fallbacks() {
    let l = linked();
    assert_eq!(l.call("build", "Store").2, "medium");
    assert_eq!(
        l.call("use", "decorated"),
        (
            s("decorated"),
            s("shop/compat.py"),
            "high",
            "same_file",
            false
        )
    );
    assert_eq!(
        l.call("go", "Repo"),
        (s("Repo"), s(REPO), "high", "import", false)
    );
    assert_eq!(
        l.call("go", "Item"),
        (s("Item"), s(REPO), "high", "import", false)
    );
    assert_eq!(
        l.call("make", "Repo"),
        (s("Repo"), s(REPO), "high", "import", false)
    );
    assert_eq!(l.call("make", "ghost2"), UNDECIDED);
    assert_eq!(l.import("shop/sub/deep.py", "...nothing.ghost2"), UNDECIDED);
}

/// Review minor: a module re-exporting through a star import of a package
/// and one that cannot be followed: undecided, not external.
#[test]
fn an_unfollowable_star_import_is_not_outweighed_by_a_package() {
    let l = linked();
    assert_eq!(l.call("use_mixed", "thing2"), UNDECIDED);
}

/// Second review n2: a `requirements/base.txt` declares for the directory
/// above `requirements/`.
#[test]
fn a_requirements_directory_declares_for_its_parent() {
    let l = link_dir_with(DIR, |env| {
        env.python = vec![devctx_parse::resolve::env::PyManifest::parse(
            "requests\n",
            "requirements/base.txt",
        )
        .unwrap()];
    });
    assert_eq!(l.call("Service.run", "get"), EXTERNAL);
}

/// Second review n1: a directory with no Python in it (a TypeScript
/// `types/`) is no module that shadows the standard library's.
#[test]
fn a_directory_without_python_shadows_nothing() {
    let l = linked();
    assert_eq!(l.call("tools/typing_use.py", "SimpleNamespace"), EXTERNAL);
}
