//! Python: a class with bases, module-level calls and constants, structured
//! imports (PLAN-009 TASK-004).

mod common;
use common::*;
use devctx_parse::Lang;

const PATH: &str = "shop/orders.py";

#[test]
fn python_symbols_bases_and_module_level_calls() {
    let pf = facts(Lang::python(), "python/orders.py", PATH);
    assert_eq!(pf.facts.package.as_deref(), Some("shop.orders"));
    assert_eq!(
        outline(&pf),
        [
            "const LIMIT",
            "class OrderService",
            "method OrderService.__init__",
            "method OrderService.find",
            "method OrderService._hidden",
            "method OrderService._hidden.step",
            "function traced",
            "function traced.wrapper",
            "function main",
        ]
    );
    assert_eq!(
        sym(&pf, "method", "OrderService.find").signature,
        "def find(self, oid: int) -> Order:"
    );
    assert_eq!(
        sym(&pf, "method", "OrderService._hidden").exported,
        Some(false)
    );
    assert_eq!(
        sym(&pf, "method", "OrderService.__init__").exported,
        Some(true)
    );
    assert_eq!(
        inherits(&pf),
        [
            t3("inherits", "BaseService", "OrderService"),
            t3("inherits", "mixins.Audited", "OrderService"),
        ]
    );
    let c = calls(&pf);
    assert!(c.contains(&t2(PATH, "main")), "a module-level call: {c:?}");
    assert!(c.contains(&t2("OrderService.find", "Repo.get")));
    let r = refs(&pf);
    assert!(r.contains(&t3("references", "Repo", "OrderService.__init__")));
    assert!(r.contains(&t3("references", "Order", "OrderService.find")));
}

#[test]
fn python_imports_are_structured() {
    let pf = facts(Lang::python(), "python/orders.py", PATH);
    assert_eq!(
        import_targets(&pf),
        ["os", "logging", ".models.Order", ".models.Line", "typing.*"]
    );
    assert_eq!(pf.facts.imports[1].alias.as_deref(), Some("log"));
    assert_eq!(pf.facts.imports[3].name.as_deref(), Some("Line"));
    assert_eq!(pf.facts.imports[3].alias.as_deref(), Some("L"));
}
