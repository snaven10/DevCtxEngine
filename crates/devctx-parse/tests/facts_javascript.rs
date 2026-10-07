//! JavaScript: arrow-consts, arrow fields, constructors, `extends`, imports
//! (PLAN-009 TASK-004).

mod common;
use common::*;
use devctx_parse::Lang;

#[test]
fn javascript_arrow_consts_fields_and_constructors() {
    let pf = facts(Lang::javascript(), "javascript/app.js", "web/app.js");
    assert_eq!(
        outline(&pf),
        [
            "function start",
            "function boot",
            "class App",
            "constructor App.constructor",
            "method App.run",
            "field App.count",
            "method App.stop",
        ]
    );
    let c = calls(&pf);
    assert!(c.contains(&t2("start", "listen")));
    assert!(c.contains(&t2("App.stop", "close")));
    assert!(refs(&pf).contains(&t3("instantiates", "Server", "App.constructor")));
    assert_eq!(inherits(&pf), [t3("inherits", "Base", "App")]);
    assert_eq!(
        import_targets(&pf),
        ["express#default", "./routes.js#route"]
    );
}

/// `exports.handler = function () {…}` and `this.onTick = () => …` bind no
/// symbol: their calls are the module's and the constructor's, in
/// `graph_edges` as well as in `edges`.
#[test]
fn javascript_assignments_bind_no_graph_source() {
    let pf = facts(Lang::javascript(), "javascript/app.js", "web/app.js");
    let graph: Vec<(String, String)> = pf
        .edges
        .iter()
        .map(|e| (e.source.clone(), e.target.clone()))
        .collect();
    assert!(
        graph.contains(&t2("App.constructor", "App.tick")),
        "{graph:?}"
    );
    assert!(!graph.iter().any(|(_, t)| t == "work"), "{graph:?}");
    assert!(calls(&pf).contains(&t2("web/app.js", "work")));
    let names: Vec<&str> = pf.symbols.iter().map(|s| s.qualified.as_str()).collect();
    for e in &pf.edges {
        assert!(
            names.contains(&e.source.as_str()),
            "{} is no symbol",
            e.source
        );
    }
}

/// A JS class field names its scope by its `property`, as a TS one does
/// by its `name`.
#[test]
fn a_javascript_field_qualifies_what_it_holds() {
    let mut pf = devctx_parse::parse(
        Lang::javascript(),
        "class A { cfg = { run() { x(); } }; }\n",
    )
    .unwrap();
    pf.assign_ids("repo", "a.js");
    assert!(
        outline(&pf).contains(&"method A.cfg.run".to_string()),
        "{:?}",
        outline(&pf)
    );
}
