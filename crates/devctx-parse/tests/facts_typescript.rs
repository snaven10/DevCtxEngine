//! TypeScript: arrow-consts as functions and sources of their calls, a
//! constructor with parameter properties, `implements`, structured imports
//! (PLAN-009 TASK-004).

mod common;
use common::*;
use devctx_parse::Lang;

const PATH: &str = "libs/auth/src/token.interceptor.ts";

/// `export const tokenInterceptor = (…) => …` was no symbol, and every call
/// in it was dropped for want of an enclosing function (master §2 H4).
#[test]
fn an_exported_arrow_const_is_a_function_and_the_source_of_its_calls() {
    let pf = facts(Lang::typescript(), "typescript/token.interceptor.ts", PATH);
    let ti = sym(&pf, "function", "tokenInterceptor");
    assert_eq!(ti.exported, Some(true));
    assert_eq!(ti.signature, "const tokenInterceptor: Fn = (req, next) =>");
    let c = calls(&pf);
    for target in ["inject", "next", "clone"] {
        assert!(
            c.contains(&t2("tokenInterceptor", target)),
            "{target}: {c:?}"
        );
    }
    // `graph_edges` names it as the source too.
    assert!(pf
        .edges
        .iter()
        .any(|e| e.source == "tokenInterceptor" && e.target == "inject"));
    let helper = sym(&pf, "function", "helper");
    // Declared without `export`, exported by `export { helper }` below.
    assert_eq!(helper.exported, Some(true));
    assert!(c.contains(&t2("helper", "log")));
    // An anonymous callback is no function: its call belongs to the method.
    assert!(c.contains(&t2("AuthService.ngOnInit", "audit")), "{c:?}");
    // Module level stays with the file symbol.
    assert!(c.contains(&t2(PATH, "register")));
}

#[test]
fn typescript_symbols_classes_and_members() {
    let pf = facts(Lang::typescript(), "typescript/token.interceptor.ts", PATH);
    assert_eq!(
        pf.facts.package.as_deref(),
        Some("libs/auth/src/token.interceptor")
    );
    assert_eq!(
        outline(&pf),
        [
            "function tokenInterceptor",
            "function helper",
            "interface Session",
            "type Token",
            "enum Mode",
            "class AuthService",
            "field AuthService.cache",
            "constructor AuthService.constructor",
            "method AuthService.ngOnInit",
            "method AuthService.ngOnInit.cb",
            "method AuthService.handle",
            "method AuthService.load",
        ]
    );
    let ctor = sym(&pf, "constructor", "AuthService.constructor");
    assert_eq!(
        ctor.signature,
        "constructor(private readonly http: HttpClient, store: AuthStore)"
    );
    assert_eq!(sym(&pf, "method", "AuthService.load").exported, Some(false));
    assert_eq!(
        sym(&pf, "method", "AuthService.handle").signature,
        "handle = (e: Event) =>"
    );
    let r = refs(&pf);
    // Parameter properties are typed uses of the constructor (DI, TASK-006).
    assert!(r.contains(&t3("references", "HttpClient", "AuthService.constructor")));
    assert!(r.contains(&t3("references", "AuthStore", "AuthService.constructor")));
    assert!(r.contains(&t3("instantiates", "SessionImpl", "AuthService.load")));
    assert!(r.contains(&t3("instantiates", "Map", "AuthService.cache")));
    assert_eq!(
        inherits(&pf),
        [
            t3("inherits", "Base", "Session"),
            t3("inherits", "BaseService", "AuthService"),
            t3("implements", "OnInit", "AuthService"),
            t3("implements", "Auditable", "AuthService"),
        ]
    );
}

#[test]
fn typescript_imports_are_structured() {
    let pf = facts(Lang::typescript(), "typescript/token.interceptor.ts", PATH);
    assert_eq!(
        import_targets(&pf),
        [
            "@angular/core#Injectable",
            "@angular/core#inject",
            "@angular/common/http#HttpInterceptorFn",
            "rxjs#*",
            "./auth.store#default",
            "./polyfills",
            "./barrel#Thing",
            "./barrel#Other",
            "./all#*",
        ]
    );
    // A barrel re-export is an import of what it re-exports (TASK-005
    // follows it), aliased under the name it exports.
    assert_eq!(pf.facts.imports[6].alias.as_deref(), Some("Alias"));
    let aliased = &pf.facts.imports[2];
    assert_eq!(aliased.alias.as_deref(), Some("Fn"));
    assert_eq!(pf.facts.imports[4].alias.as_deref(), Some("AuthStore"));
}

/// `graph_edges` names its source by text, and `impact_analysis` /
/// `get_references` read it: an arrow is that source only when it is a
/// symbol (a `const` or a class field). An arrow bound to an object key, to
/// `this.x` or nested in a call belongs to the function that wrote it, and
/// one bound to a local `const` is named as the symbol it is.
#[test]
fn graph_edges_name_an_arrow_only_when_it_is_a_symbol() {
    let pf = facts(Lang::typescript(), "typescript/token.interceptor.ts", PATH);
    let graph = |target: &str| -> Vec<String> {
        pf.edges
            .iter()
            .filter(|e| e.target.ends_with(target))
            .map(|e| e.source.clone())
            .collect()
    };
    // `subscribe({ next: r => this.process(r) })`: `next` is no symbol.
    assert_eq!(graph("process"), ["AuthService.ngOnInit"]);
    // `const cb = () => …` inside a method is the symbol `…ngOnInit.cb`.
    assert_eq!(graph("track"), ["AuthService.ngOnInit.cb"]);
    // `this.onTick = () => …` in the constructor.
    assert_eq!(graph("tick"), ["AuthService.constructor"]);
    // The id-based source agrees with the text one.
    let c = calls(&pf);
    assert!(
        c.contains(&t2("AuthService.ngOnInit", "AuthService.process")),
        "{c:?}"
    );
    assert!(
        c.contains(&t2("AuthService.ngOnInit.cb", "AuthService.track")),
        "{c:?}"
    );
    assert!(
        c.contains(&t2("AuthService.constructor", "AuthService.tick")),
        "{c:?}"
    );
    // Every `graph_edges` source is the qualified name of a symbol.
    let names: Vec<&str> = pf.symbols.iter().map(|s| s.qualified.as_str()).collect();
    for e in &pf.edges {
        assert!(
            names.contains(&e.source.as_str()),
            "{} is no symbol",
            e.source
        );
    }
}

/// `exported` is "visible outside the module": `export { helper }` exports
/// a symbol declared without the keyword.
#[test]
fn an_export_clause_exports_its_names() {
    let pf = facts(Lang::typescript(), "typescript/token.interceptor.ts", PATH);
    assert_eq!(sym(&pf, "function", "helper").exported, Some(true));
}
