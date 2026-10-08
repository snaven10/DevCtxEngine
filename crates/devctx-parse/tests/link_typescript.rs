//! The TypeScript/JavaScript resolver (PLAN-009 TASK-006, DD-7): a
//! multi-file Nx-style fixture — two `AuthService`s, relative imports,
//! `tsconfig` aliases, barrels, constructor parameter properties,
//! `inject(T)`, exported arrow functions, npm packages — parsed and linked as
//! the indexer does, without a store.

use std::collections::HashMap;
use std::path::Path;

use devctx_parse::resolve::link::{link_rows, LinkEdge, Outcome, RepoIndex, Resolved};
use devctx_parse::resolve::typescript::{Manifest, ScriptEnv, TsConfig};
use devctx_parse::{detect_lang, parse};

const ROOT: &str = "tests/fixtures/ts";

struct Linked {
    index: RepoIndex,
    edges: Vec<LinkEdge>,
    /// id → (qualified name, file).
    names: HashMap<u64, (String, String)>,
    symbols: Vec<devctx_parse::resolve::link::LinkSymbol>,
}

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else if detect_lang(&p).is_some() {
            out.push(p);
        }
    }
}

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(ROOT)
}

fn config() -> Option<TsConfig> {
    let text = std::fs::read_to_string(root().join("tsconfig.base.json")).unwrap();
    TsConfig::parse(&text, "")
}

fn manifest() -> Manifest {
    let text = std::fs::read_to_string(root().join("package.json")).unwrap();
    Manifest::parse(&text, "").unwrap()
}

fn linked_with(cfg: Option<TsConfig>) -> Linked {
    let root = root();
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    let (mut symbols, mut edges) = (Vec::new(), Vec::new());
    for f in files {
        let rel = f
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let lang = detect_lang(&f).unwrap();
        let mut pf = parse(lang, &std::fs::read_to_string(&f).unwrap()).unwrap();
        pf.assign_ids("repo", &rel);
        let (s, e) = link_rows(&rel, &pf);
        symbols.extend(s);
        edges.extend(e);
    }
    let names = symbols
        .iter()
        .map(|s| (s.id, (s.qualified.clone(), s.file.clone())))
        .collect();
    Linked {
        index: RepoIndex::with_script_env(
            symbols.clone(),
            &edges,
            ScriptEnv {
                tsconfig: cfg,
                manifests: vec![manifest()],
            },
        ),
        edges,
        names,
        symbols,
    }
}

fn linked() -> Linked {
    linked_with(config())
}

/// `(destination qualified, its file, confidence, resolution, external)`.
type Shown = (
    Option<String>,
    Option<String>,
    &'static str,
    &'static str,
    bool,
);

impl Linked {
    fn src_name(&self, e: &LinkEdge) -> Option<&str> {
        self.names.get(&e.src_id).map(|(q, _)| q.as_str())
    }

    /// The outcomes of the edges of `kind` from `src` (qualified) to
    /// `dst_name`, in line order.
    fn all(&self, kind: &str, src: &str, dst: &str) -> Vec<Outcome> {
        let found: Vec<Outcome> = self
            .edges
            .iter()
            .filter(|e| e.kind == kind && e.dst_name == dst)
            .filter(|e| self.src_name(e) == Some(src))
            .map(|e| self.index.resolve(e).expect("resolved"))
            .collect();
        assert!(
            !found.is_empty(),
            "no {kind} {src} -> {dst} in {:#?}",
            self.edges
                .iter()
                .filter(|e| self.src_name(e) == Some(src))
                .map(|e| format!(
                    "{} {} [{}]",
                    e.kind,
                    e.dst_name,
                    e.hint.as_deref().unwrap_or("-")
                ))
                .collect::<Vec<_>>()
        );
        found
    }

    fn call(&self, src: &str, dst: &str) -> Shown {
        self.show(&self.all("calls", src, dst).remove(0))
    }

    fn show(&self, o: &Outcome) -> Shown {
        match o {
            Outcome::Resolved(Resolved {
                dst_id,
                confidence,
                resolution,
                external,
            }) => {
                let d = dst_id.map(|d| self.names[&d].clone());
                (
                    d.as_ref().map(|(q, _)| q.clone()),
                    d.map(|(_, f)| f),
                    confidence,
                    resolution,
                    *external,
                )
            }
            Outcome::Discard => (None, None, "-", "discard", false),
        }
    }

    /// The import edge of `file` whose destination is `target`.
    fn import(&self, file: &str, target: &str) -> Shown {
        let e = self
            .edges
            .iter()
            .find(|e| e.kind == "imports" && e.file == file && e.dst_name == target)
            .unwrap_or_else(|| panic!("no import {target} in {file}"));
        self.show(&self.index.resolve(e).unwrap())
    }
}

fn s(x: &str) -> Option<String> {
    Some(x.to_string())
}

const AUTH: &str = "libs/auth/src/lib/services/auth.service.ts";
const AUTH_DATA: &str = "libs/auth-data/src/lib/services/auth.service.ts";
const UTIL: &str = "apps/shell/src/app/util.ts";
const STORE: &str = "apps/shell/src/app/store.ts";
const INTERCEPTOR: &str = "libs/auth/src/lib/providers/token.interceptor.ts";

/// The step-1 case: an exported arrow function that `inject`s the
/// `AuthService` it imports by a relative path calls that one, not its
/// homonym; `inject` is Angular's; `next` is an untyped parameter.
#[test]
fn the_interceptor_reaches_the_imported_auth_service() {
    let l = linked();
    assert_eq!(
        l.call("tokenInterceptor", "AuthService.token"),
        (
            s("AuthService.token"),
            s(AUTH),
            "high",
            "ctor_inject",
            false
        )
    );
    assert_eq!(
        l.call("tokenInterceptor", "inject"),
        (None, None, "high", "external_known", true)
    );
    // A parameter holding a function: nothing types it, and the repository
    // defines no `next`.
    assert_eq!(
        l.call("tokenInterceptor", "next"),
        (None, None, "-", "discard", false)
    );
}

/// `tokenInterceptor` is a symbol with its signature, a source (above) and a
/// destination imported through an alias and a barrel's named re-export.
#[test]
fn an_exported_arrow_function_is_a_symbol_source_and_destination() {
    let l = linked();
    let sym = l
        .symbols
        .iter()
        .find(|s| s.qualified == "tokenInterceptor")
        .expect("tokenInterceptor is a symbol");
    assert_eq!(sym.kind, "function");
    assert_eq!(sym.file, INTERCEPTOR);
    assert!(
        sym.signature
            .as_deref()
            .is_some_and(|s| s.starts_with("const tokenInterceptor = (req: Request")),
        "{sym:?}"
    );
    assert_eq!(
        l.call("run", "tokenInterceptor"),
        (
            s("tokenInterceptor"),
            s(INTERCEPTOR),
            "high",
            "import",
            false
        )
    );
    assert_eq!(
        l.import(
            "libs/auth/src/index.ts",
            "./lib/providers/token.interceptor#tokenInterceptor"
        ),
        (
            s("tokenInterceptor"),
            s(INTERCEPTOR),
            "high",
            "import",
            false
        )
    );
}

/// Constructor parameter properties are fields (`ctor_inject`), resolved
/// through a `tsconfig` alias and the barrel's `export *`; a plain
/// constructor parameter is a parameter; the declared return type of a
/// repository method types the next call of a chain.
#[test]
fn constructor_injection_aliases_and_barrels() {
    let l = linked();
    assert_eq!(
        l.call("LoginComponent.submit", "AuthService.login"),
        (
            s("AuthService.login"),
            s(AUTH),
            "high",
            "ctor_inject",
            false
        )
    );
    assert_eq!(
        l.call("LoginComponent.constructor", "Store.save"),
        (s("Store.save"), s(STORE), "high", "param", false)
    );
    // `login(): Observable<Session>`, `Observable` from `rxjs`.
    assert_eq!(
        l.call("LoginComponent.submit", "pipe"),
        (None, None, "high", "external_known", true)
    );
    assert_eq!(
        l.call("LoginComponent.submit", "map"),
        (None, None, "high", "external_known", true)
    );
    let refs = l.all("references", "LoginComponent.constructor", "AuthService");
    assert_eq!(
        l.show(&refs[0]),
        (s("AuthService"), s(AUTH), "high", "import", false)
    );
    assert_eq!(
        l.import(
            "apps/shell/src/app/login.component.ts",
            "@acme/auth#AuthService"
        ),
        (s("AuthService"), s(AUTH), "high", "import", false)
    );
}

/// `inject(T)` in a field initializer, with the import renamed (`as`) and
/// the alias pointing at the other library: its `AuthService`.
#[test]
fn inject_in_a_field_follows_a_renamed_import() {
    let l = linked();
    assert_eq!(
        l.call("DataComponent.load", "DataAuth.token"),
        (
            s("AuthService.token"),
            s(AUTH_DATA),
            "high",
            "ctor_inject",
            false
        )
    );
}

/// Lesson 3 of TASK-005: an arrow's parameter, a catch variable, a cast
/// receiver are not the field they are named like.
#[test]
fn local_bindings_shadow_fields() {
    let l = linked();
    let undecided = (None, None, "low", "name_only", false);
    assert_eq!(l.call("LoginComponent.shadow", "token"), undecided);
    let calls: Vec<Shown> = l
        .all("calls", "LoginComponent.callback", "token")
        .iter()
        .map(|o| l.show(o))
        .collect();
    assert_eq!(calls, [undecided.clone(), undecided], "{calls:#?}");
    // `this.auth.token()` between them is the injected field's.
    assert_eq!(
        l.call("LoginComponent.callback", "AuthService.token"),
        (
            s("AuthService.token"),
            s(AUTH),
            "high",
            "ctor_inject",
            false
        )
    );
}

/// Named, namespace and default imports, an imported name that a package
/// brings (and the repository happens to define: not external, DD-9), and
/// platform globals.
#[test]
fn imports_by_name_namespace_default_and_package() {
    let l = linked();
    assert_eq!(
        l.call("run", "formatName"),
        (s("formatName"), s(UTIL), "high", "import", false)
    );
    assert_eq!(
        l.call("run", "helper"),
        (s("helper"), s(UTIL), "high", "import", false)
    );
    // A default import names no symbol: matched by its local name, medium.
    assert_eq!(
        l.call("run", "DefaultStore.clear"),
        (s("DefaultStore.clear"), s(STORE), "medium", "local", false)
    );
    assert_eq!(
        l.call("run", "fromPackage"),
        (None, None, "low", "name_only", false)
    );
    assert_eq!(
        l.call("run", "of"),
        (None, None, "high", "external_known", true)
    );
    assert_eq!(
        l.import("apps/shell/src/app/use-util.ts", "acme-sdk#formatName"),
        (None, None, "low", "name_only", false)
    );
    assert_eq!(
        l.import("apps/shell/src/app/use-util.ts", "rxjs#of"),
        (None, None, "high", "external_known", true)
    );
    assert_eq!(
        l.call("Store.save", "setItem"),
        (None, None, "high", "external_known", true)
    );
    assert_eq!(
        l.call("helper", "log"),
        (None, None, "high", "external_known", true)
    );
}

/// Lesson 6: `this` in an object literal's method or in a class expression
/// is no enclosing class — never the module's function of the same name.
#[test]
fn this_in_object_literals_and_class_expressions_is_not_a_class() {
    let l = linked();
    let undecided = (None, None, "low", "name_only", false);
    assert_eq!(l.call("api.run", "stop"), undecided);
    assert_eq!(l.call("Klass.m", "n"), undecided);
}

/// JavaScript: imports, `this`, untyped parameters (rules 7-8), a package.
#[test]
fn javascript_without_types() {
    let l = linked();
    assert_eq!(
        l.call("Main.run", "Main.go"),
        (
            s("Main.go"),
            s("apps/legacy/main.js"),
            "high",
            "self",
            false
        )
    );
    assert_eq!(
        l.call("Main.run", "greet"),
        (
            s("greet"),
            s("apps/legacy/helpers.js"),
            "high",
            "import",
            false
        )
    );
    assert_eq!(
        l.call("Main.run", "vanishing"),
        (None, None, "-", "discard", false)
    );
    assert_eq!(
        l.call("Main.run", "express"),
        (None, None, "high", "external_known", true)
    );
    assert_eq!(
        l.call("Main.run", "listen"),
        (None, None, "medium", "chain_external", true)
    );
}

/// Without a `tsconfig` an alias is no package: an `@acme/...` import whose
/// name the repository defines is not external (DD-9), and the call stays
/// out of `high`.
#[test]
fn without_tsconfig_an_alias_is_not_taken_for_a_package() {
    let l = linked_with(None);
    let (_, _, conf, _, external) = l.call("LoginComponent.submit", "AuthService.login");
    assert_ne!(conf, "high");
    assert!(!external);
    let (_, _, conf, _, external) = l.import(
        "apps/shell/src/app/login.component.ts",
        "@acme/auth#AuthService",
    );
    assert_ne!(conf, "high");
    assert!(!external);
    // Relative imports still resolve.
    assert_eq!(
        l.call("tokenInterceptor", "AuthService.token"),
        (
            s("AuthService.token"),
            s(AUTH),
            "high",
            "ctor_inject",
            false
        )
    );
}

/// `tsconfig.base.json` with comments and trailing commas, `baseUrl` and
/// `paths` (exact and wildcard).
#[test]
fn a_tsconfig_with_comments_is_read() {
    let cfg = config().expect("parsed");
    assert_eq!(cfg.base_url.as_deref(), Some(""));
    assert!(cfg
        .paths
        .iter()
        .any(|(k, v)| k == "@acme/auth" && v == &["libs/auth/src/index.ts".to_string()]));
    assert!(cfg
        .paths
        .iter()
        .any(|(k, v)| k == "@acme/ui/*" && v == &["libs/ui/src/*".to_string()]));
    // Relative to the file's directory.
    let nested = TsConfig::parse(
        r#"{ "compilerOptions": { "baseUrl": "./src", "paths": { "~/*": ["app/*"] } } }"#,
        "web",
    )
    .unwrap();
    assert_eq!(nested.base_url.as_deref(), Some("web/src"));
    assert_eq!(nested.fingerprint(), nested.clone().fingerprint());
    assert_ne!(nested.fingerprint(), cfg.fingerprint());
}

/// TASK-006 review, M1: a name imported from a package that a repository file
/// also defines is not that file's (no `unique_name` guess): undecided.
#[test]
fn a_package_import_is_never_a_repository_homonym() {
    let l = linked();
    assert_eq!(
        l.call("usePackage", "formatName"),
        (None, None, "low", "name_only", false)
    );
}

/// TASK-006 review, M2: a package is external only with evidence — a
/// dependency of a `package.json`, or a Node builtin. An unmapped alias
/// (`@app/...`, an app's own `tsconfig` that is not read) or a module no
/// manifest declares is undecided, never external.
#[test]
fn a_package_needs_a_manifest_or_a_node_builtin() {
    let l = linked();
    assert_eq!(
        l.call("usePackage", "selectUser"),
        (None, None, "low", "name_only", false)
    );
    assert_eq!(
        l.call("usePackage", "Widget.render"),
        (None, None, "low", "name_only", false)
    );
    assert_eq!(
        l.call("usePackage", "readFileSync"),
        (None, None, "high", "external_known", true)
    );
    assert_eq!(
        l.call("usePackage", "of"),
        (None, None, "high", "external_known", true)
    );
    assert_eq!(
        l.import("apps/shell/src/app/use-package.ts", "@app/auth/reducers#*"),
        (None, None, "low", "name_only", false)
    );
}

/// TASK-006 review, m5: only Angular's `inject` injects; a local helper of
/// that name types nothing.
#[test]
fn only_angulars_inject_injects() {
    let l = linked();
    assert_eq!(
        l.call("useLocal", "token"),
        (None, None, "low", "name_only", false)
    );
}

/// TASK-006 review (NIT): a function bound in a block is not in scope in a
/// sibling block or after it: the module's function of that name is.
#[test]
fn a_block_local_function_does_not_shadow_outside_its_block() {
    let l = linked();
    let calls: Vec<Shown> = l
        .all("calls", "outer", "cb")
        .iter()
        .map(|o| l.show(o))
        .collect();
    let blocks = "apps/shell/src/app/blocks.ts";
    assert_eq!(
        calls,
        [
            (s("outer.cb"), s(blocks), "high", "same_file", false),
            (s("cb"), s(blocks), "high", "same_file", false),
        ]
    );
}
