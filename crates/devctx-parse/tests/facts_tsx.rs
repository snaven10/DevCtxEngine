//! TSX: arrow components are functions (PLAN-009 TASK-004).

mod common;
use common::*;
use devctx_parse::Lang;

#[test]
fn tsx_arrow_components_are_functions() {
    let pf = facts(Lang::tsx(), "tsx/Button.tsx", "web/Button.tsx");
    assert_eq!(
        outline(&pf),
        [
            "function Button",
            "function Button.onClick",
            "function Panel"
        ]
    );
    assert_eq!(sym(&pf, "function", "Button").exported, Some(true));
    assert!(calls(&pf).contains(&t2("Button.onClick", "track")));
    assert!(refs(&pf).contains(&t3("references", "JSX.Element", "Panel")));
    assert_eq!(import_targets(&pf), ["react#default"]);
}
