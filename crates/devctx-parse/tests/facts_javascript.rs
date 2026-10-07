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
