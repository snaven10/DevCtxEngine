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
    assert_eq!(helper.exported, Some(false));
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
        ]
    );
    let aliased = &pf.facts.imports[2];
    assert_eq!(aliased.alias.as_deref(), Some("Fn"));
    assert_eq!(pf.facts.imports[4].alias.as_deref(), Some("AuthStore"));
}
