//! The Go resolver (PLAN-009 TASK-007, DD-7, basic per Q-6): a module whose
//! `go.mod` says which imports are the repository's, methods by their
//! receiver (in another file of the package too), the standard library and
//! third-party modules as external — parsed and linked as the indexer does.

mod common;
use common::linked::*;

const DIR: &str = "go/link";
const SERVER: &str = "server/server.go";
const HELPERS: &str = "server/helpers.go";
const STORE: &str = "store/store.go";

fn linked() -> Linked {
    link_dir(DIR)
}

/// Step 1: a method with a receiver is the receiver's type's (`s.helper()`
/// in a method of `*Server`, defined in another file of the package); a
/// field of the receiver is typed by its declaration.
#[test]
fn methods_by_their_receiver() {
    let l = linked();
    assert_eq!(
        l.call("Server.Handle", "Server.helper"),
        (s("Server.helper"), s(HELPERS), "high", "self", false)
    );
    assert_eq!(
        l.call("Server.Handle", "Get"),
        (s("Store.Get"), s(STORE), "high", "field", false)
    );
    // `x := NewServer()`: what the constructor returns.
    assert_eq!(
        l.call("Server.Handle", "NewServer"),
        (s("NewServer"), s(SERVER), "high", "same_file", false)
    );
    assert_eq!(
        l.call("Server.Handle", "helper"),
        (s("Server.helper"), s(HELPERS), "high", "local", false)
    );
    assert_eq!(
        l.call("Server.Handle", "other"),
        (s("other"), s(HELPERS), "high", "same_package", false)
    );
    // An interface's method.
    assert_eq!(
        l.call("Server.Handle", "store.Getter.Get"),
        (s("Getter.Get"), s(STORE), "high", "param", false)
    );
}

/// Step 1: an import under the `module` of `go.mod` is the repository's;
/// `fmt`, a required module and an undeclared third-party one are not.
#[test]
fn the_module_decides_what_is_the_repositorys() {
    let l = linked();
    let news = l.all("calls", "Server.Handle", "New");
    assert_eq!(news[0], (s("New"), s(STORE), "high", "import", false));
    assert_eq!(news[1], EXTERNAL);
    assert_eq!(l.call("Server.Handle", "Println"), EXTERNAL);
    assert_eq!(l.call("Server.Handle", "WithContext"), EXTERNAL);
    assert_eq!(l.call("Server.Handle", "len"), EXTERNAL);
    assert_eq!(
        l.import(SERVER, "github.com/acme/demo/store"),
        (None, None, "high", "import", false)
    );
    assert_eq!(l.import(SERVER, "fmt"), EXTERNAL);
    assert_eq!(l.import(SERVER, "github.com/pkg/errors"), EXTERNAL);
    // `y := store.New()`.
    let gets = l.all("calls", "Server.Handle", "Get");
    assert_eq!(gets[1], (s("Store.Get"), s(STORE), "high", "local", false));
}

/// Lesson 3: a closure's parameter shadows the receiver of the same name.
#[test]
fn a_closure_parameter_shadows_the_receiver() {
    let l = linked();
    assert_eq!(
        l.call("Server.Handle", "store.Store.Get"),
        (s("Store.Get"), s(STORE), "high", "param", false)
    );
}

/// Without `go.mod` nothing says a dotted import path is the repository's
/// or from outside: undecided; the standard library is still external.
#[test]
fn a_module_needs_its_go_mod() {
    let l = link_dir_with(DIR, |env| env.go.clear());
    let news = l.all("calls", "Server.Handle", "New");
    assert_eq!(news[0], UNDECIDED);
    assert_eq!(news[1], UNDECIDED);
    assert_eq!(l.call("Server.Handle", "Println"), EXTERNAL);
}
