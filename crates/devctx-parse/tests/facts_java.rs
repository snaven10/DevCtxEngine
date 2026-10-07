//! Java: constructors, records, fields (`@Inject`), inner classes, `new`,
//! structured imports and supertypes (PLAN-009 TASK-004).

mod common;
use common::*;
use devctx_parse::Lang;

const PATH: &str = "src/main/java/com/example/shop/OrderService.java";

#[test]
fn java_symbols_with_kind_parent_signature_and_visibility() {
    let pf = facts(Lang::java(), "java/OrderService.java", PATH);
    assert_eq!(pf.facts.package.as_deref(), Some("com.example.shop"));
    assert_eq!(
        outline(&pf),
        [
            "class OrderService",
            "field OrderService.repository",
            "field OrderService.LOG",
            "field OrderService.cache",
            "constructor OrderService.OrderService",
            "method OrderService.find",
            "record OrderService.Line",
            "method OrderService.Line.compareTo",
            "class OrderService.Cache",
            "method OrderService.Cache.clear",
            "method OrderService.init",
            "method OrderService.init.run",
            "interface Auditable",
            "method Auditable.audit",
        ]
    );
    let ctor = sym(&pf, "constructor", "OrderService.OrderService");
    assert_eq!(
        ctor.signature,
        "public OrderService(OrderRepository repository)"
    );
    assert_eq!(ctor.params.as_deref(), Some("OrderRepository"));
    assert_eq!(parent_of(&pf, ctor), "OrderService");
    // The head of a definition spans lines, and stops at the body.
    let find = sym(&pf, "method", "OrderService.find");
    assert_eq!(find.signature, "public Order find(Long id, String user)");
    assert_eq!(find.exported, Some(true));
    let field = sym(&pf, "field", "OrderService.repository");
    assert_eq!(field.signature, "@Inject OrderRepository repository;");
    assert_eq!(field.exported, Some(false));
    assert_eq!(field.params, None, "only callables carry parameter types");
    let line = sym(&pf, "record", "OrderService.Line");
    assert_eq!(line.params, None);
    assert_eq!(
        parent_of(&pf, sym(&pf, "method", "OrderService.Line.compareTo")),
        "OrderService.Line"
    );
    assert_eq!(parent_of(&pf, sym(&pf, "class", "OrderService")), PATH);
    // Interface members are public without saying so.
    assert_eq!(sym(&pf, "method", "Auditable.audit").exported, Some(true));
    assert_eq!(sym(&pf, "interface", "Auditable").exported, Some(false));
    // An anonymous class's method is scoped by its callable, parameters
    // included (Java overloads, DD-3).
    let run = sym(&pf, "method", "OrderService.init.run");
    assert_eq!(run.scope_shape.as_deref(), Some("sf()"));
}

#[test]
fn java_imports_are_structured() {
    let pf = facts(Lang::java(), "java/OrderService.java", PATH);
    assert_eq!(
        import_targets(&pf),
        [
            "java.util.List",
            "java.util.*",
            "java.util.Objects.requireNonNull",
            "com.example.shop.repo.OrderRepository",
        ]
    );
    assert!(pf.facts.imports[1].wildcard);
    assert_eq!(
        pf.imports.len(),
        4,
        "the statements stay for the file chunk"
    );
}

#[test]
fn java_supertypes_instantiations_and_type_uses() {
    let pf = facts(Lang::java(), "java/OrderService.java", PATH);
    assert_eq!(
        inherits(&pf),
        [
            t3("inherits", "BaseService", "OrderService"),
            t3("implements", "Auditable", "OrderService"),
            t3("implements", "Serializable", "OrderService"),
            t3("implements", "Comparable", "OrderService.Line"),
            t3("inherits", "Named", "Auditable"),
        ]
    );
    let r = refs(&pf);
    assert!(r.contains(&t3("instantiates", "ArrayList", "OrderService.cache")));
    assert!(r.contains(&t3("instantiates", "Order", "OrderService.Cache.clear")));
    assert!(r.contains(&t3(
        "references",
        "OrderRepository",
        "OrderService.repository"
    )));
    assert!(r.contains(&t3(
        "references",
        "OrderRepository",
        "OrderService.OrderService"
    )));
    assert!(r.contains(&t3("references", "Order", "OrderService.find")));
    assert!(r.contains(&t3("references", "List", "OrderService.cache")));
    assert!(
        r.contains(&t3("references", "Order", "OrderService.cache")),
        "a generic argument is a use too"
    );
    assert!(
        !r.iter().any(|(_, n, _)| n == "var"),
        "`var` is not a type: {r:?}"
    );
    // A call inside the constructor comes from the constructor now that it
    // is a symbol; a call in a field initializer from the field.
    let c = calls(&pf);
    assert!(
        c.contains(&t2("OrderService.OrderService", "init")),
        "{c:?}"
    );
    assert!(
        c.contains(&t2("OrderService.LOG", "Logger.getLogger")),
        "{c:?}"
    );
}
