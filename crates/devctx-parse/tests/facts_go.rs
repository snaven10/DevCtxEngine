//! Go: methods with receivers, struct fields, embedding, grouped imports
//! (PLAN-009 TASK-004).

mod common;
use common::*;
use devctx_parse::Lang;

const PATH: &str = "api/server.go";

/// `func (s *Server) Handle()` is `Server.Handle`, in the symbol and as the
/// source of its calls.
#[test]
fn a_method_with_a_receiver_is_qualified_by_its_type() {
    let pf = facts(Lang::go(), "go/server.go", PATH);
    let handle = sym(&pf, "method", "Server.Handle");
    assert_eq!(parent_of(&pf, handle), "Server");
    assert_eq!(
        handle.signature,
        "func (s *Server) Handle(w h.ResponseWriter)"
    );
    assert_eq!(handle.exported, Some(true));
    assert_eq!(sym(&pf, "method", "Server.helper").exported, Some(false));
    assert!(pf
        .edges
        .iter()
        .any(|e| e.source == "Server.Handle" && e.target == "Server.helper"));
    assert!(calls(&pf).contains(&t2("Server.Handle", "Println")));
}

#[test]
fn go_symbols_imports_and_embedding() {
    let pf = facts(Lang::go(), "go/server.go", PATH);
    assert_eq!(pf.facts.package.as_deref(), Some("api"));
    assert_eq!(
        outline(&pf),
        [
            "const Version",
            "interface Handler",
            // An interface's method is a symbol (TASK-007).
            "method Handler.Serve",
            "struct Server",
            "field Server.store",
            "field Server.Name",
            "function NewServer",
            "method Server.Handle",
            "method Server.helper",
        ]
    );
    assert_eq!(
        import_targets(&pf),
        ["fmt", "net/http", "github.com/acme/demo/store"]
    );
    assert_eq!(pf.facts.imports[1].alias.as_deref(), Some("h"));
    assert_eq!(
        pf.facts.imports.iter().map(|i| i.line).collect::<Vec<_>>(),
        [4, 5, 7],
        "each import on its own line, not the block's"
    );
    assert_eq!(inherits(&pf), [t3("inherits", "Base", "Server")]);
    let r = refs(&pf);
    assert!(r.contains(&t3("instantiates", "Server", "NewServer")));
    assert!(r.contains(&t3("references", "store.Store", "Server.store")));
    assert!(r.contains(&t3("references", "h.ResponseWriter", "Server.Handle")));
}
