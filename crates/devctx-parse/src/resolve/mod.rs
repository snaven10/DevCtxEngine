//! Per-language semantics that a query cannot express (PLAN-009 DD-5).
//!
//! The patterns — what a definition, a call, an import look like — live in
//! `languages/*.json`, where the fingerprint covers them. What a pattern
//! cannot say lives here, one implementation of [`LangResolver`] per
//! language: what a file's package is when the language derives it from the
//! path, whether a definition is visible outside its file, how a Rust `use`
//! tree expands, how an import names its destination, and whether a name
//! belongs to the platform.
//!
//! TASK-004 gave it the local facts; TASK-005 the receiver's type within a
//! file ([`scope`], [`LangResolver::init_type`]) and the cross-file half —
//! which symbol an occurrence reaches, or whether it is external — in
//! [`link`], with Java's rules in [`java`].

pub mod env;
pub mod java;
pub mod link;
pub mod scope;
pub mod typescript;

use tree_sitter::Node;

use crate::facts::ImportFact;
use crate::lang::Lang;
use crate::registry::LangDef;

/// The language-specific half of a file's facts.
pub trait LangResolver: Sync {
    /// The module path of `path` for a language that derives it from the
    /// path (Python `a.b.c`, TS/JS `src/app/x`, Rust `my_crate::a::b`);
    /// `None` for a language that declares it in the source (Java, Go).
    fn package_from_path(&self, path: &str) -> Option<String>;

    /// Whether the definition `def` named `name` is visible outside its
    /// package/module (`public`, `export`, `pub`, a capital Go name, no
    /// leading `_` in Python); `None` when the language says nothing (a
    /// Rust `impl`). See [`Symbol::exported`](crate::Symbol::exported):
    /// Java package-private and Go lowercase names are `false` though their
    /// package sees them.
    fn exported(&self, def: Node<'_>, name: &str, bytes: &[u8]) -> Option<bool>;

    /// The names an `@import.tree` capture brings in (a Rust `use`
    /// tree), as facts without their line. Languages whose query captures
    /// the parts directly have none.
    fn expand_import_tree(&self, _tree: Node<'_>, _bytes: &[u8]) -> Vec<ImportFact> {
        Vec::new()
    }

    /// The destination an `imports` edge records for an import.
    fn import_target(&self, imp: &ImportFact) -> String;

    /// Whether `name` (bare or qualified as written) belongs to the platform
    /// — the JDK, `std`, the Go standard library, Python's builtins — by the
    /// language's `platform_prefixes` and `builtins` (DD-9). Evidence for
    /// `external`, which the link pass decides.
    fn is_platform(&self, def: &LangDef, name: &str) -> bool {
        def.builtins.iter().any(|b| b == name)
            || def
                .platform_prefixes
                .iter()
                .any(|p| name.starts_with(p.as_str()))
    }

    /// The type a declaration without one written takes from its
    /// initializer (`var x = new T()`, `var x = (T) e`; `var x =
    /// T.of(…)` as [`Via::Static`](scope::Via::Static)), and how. `None`:
    /// nothing local says (`var q = em.createQuery(…)`).
    fn init_type(&self, _init: Node<'_>, _bytes: &[u8]) -> Option<(scope::TypeText, scope::Via)> {
        None
    }

    /// Whether a bare name can be a member of the enclosing class (Java's
    /// implicit `this`). TypeScript/JavaScript reach a member only through
    /// `this.x`: a bare `x` is a local, an import or a global, never the
    /// field of that name.
    fn implicit_this(&self) -> bool {
        true
    }
}

/// The resolver of `lang`.
pub fn resolver_for(lang: Lang) -> &'static dyn LangResolver {
    match lang.key() {
        "java" => &java::Java,
        "typescript" | "tsx" | "javascript" => &Script,
        "python" => &Python,
        "rust" => &Rust,
        "go" => &Go,
        _ => &Generic,
    }
}

/// `path` without its last extension (`src/a.ts` → `src/a`).
fn strip_extension(path: &str) -> &str {
    match path.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() && !stem.ends_with('/') => stem,
        _ => path,
    }
}

/// The text of `node` without whitespace.
fn compact(node: Node<'_>, bytes: &[u8]) -> String {
    node.utf8_text(bytes)
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// A language with no resolver of its own.
struct Generic;

impl LangResolver for Generic {
    fn package_from_path(&self, _path: &str) -> Option<String> {
        None
    }
    fn exported(&self, _def: Node<'_>, _name: &str, _bytes: &[u8]) -> Option<bool> {
        None
    }
    fn import_target(&self, imp: &ImportFact) -> String {
        imp.path.clone()
    }
}

/// TypeScript, TSX and JavaScript.
struct Script;

impl LangResolver for Script {
    fn package_from_path(&self, path: &str) -> Option<String> {
        Some(strip_extension(path).to_string())
    }

    /// Top level: under an `export` statement, or named by an `export {
    /// name }` clause of the file. A class member: not
    /// `private`/`protected`/`#name`, and its class exported. Anything
    /// nested in a function: no.
    fn exported(&self, def: Node<'_>, name: &str, bytes: &[u8]) -> Option<bool> {
        if matches!(
            def.kind(),
            "method_definition" | "public_field_definition" | "field_definition"
        ) {
            if name.starts_with('#') {
                return Some(false);
            }
            let mut cursor = def.walk();
            let hidden = def.children(&mut cursor).any(|c| {
                c.kind() == "accessibility_modifier"
                    && c.utf8_text(bytes)
                        .is_ok_and(|t| t == "private" || t == "protected")
            });
            if hidden {
                return Some(false);
            }
            let class = def.parent().and_then(|body| body.parent());
            return Some(class.is_some_and(|c| {
                under_export(c) || in_export_clause(c, &class_name(c, bytes), bytes)
            }));
        }
        Some(under_export(def) || in_export_clause(def, name, bytes))
    }

    fn implicit_this(&self) -> bool {
        false
    }

    /// `new T(…)` and `x as T` are `T`; `inject(T)` (Angular's DI) is `T`,
    /// injected (DD-7 rule 4).
    fn init_type(&self, init: Node<'_>, bytes: &[u8]) -> Option<(scope::TypeText, scope::Via)> {
        typescript::init_type(init, bytes)
    }

    fn import_target(&self, imp: &ImportFact) -> String {
        match (&imp.name, imp.wildcard) {
            (_, true) => format!("{}#*", imp.path),
            (Some(n), false) => format!("{}#{n}", imp.path),
            (None, false) => imp.path.clone(),
        }
    }
}

/// Is `node` (or the declaration holding it) the child of an `export`?
fn under_export(node: Node<'_>) -> bool {
    let mut cur = node.parent();
    // `export const x = …`: lexical_declaration → export_statement.
    for _ in 0..2 {
        match cur {
            Some(n) if n.kind() == "export_statement" => return true,
            Some(n) if matches!(n.kind(), "lexical_declaration" | "variable_declaration") => {
                cur = n.parent();
            }
            _ => return false,
        }
    }
    false
}

/// Is the top-level declaration `def` named by an `export { … }` clause of
/// its file (`function foo() {}` … `export { foo }`, `export { foo as
/// bar }`), or by an `export default foo;` (`(foo)` too)? A clause with a `from`
/// re-exports another module's names.
fn in_export_clause(def: Node<'_>, name: &str, bytes: &[u8]) -> bool {
    let top = match def.parent() {
        Some(p) if p.kind() == "program" => p,
        Some(p) if matches!(p.kind(), "lexical_declaration" | "variable_declaration") => {
            match p.parent() {
                Some(r) if r.kind() == "program" => r,
                _ => return false,
            }
        }
        _ => return false,
    };
    let mut cursor = top.walk();
    let statements: Vec<Node<'_>> = top
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "export_statement" && c.child_by_field_name("source").is_none())
        .collect();
    statements.into_iter().any(|st| {
        // `export default foo;` (or `(foo)`): the exported value is the
        // bare name.
        let default = st
            .child_by_field_name("value")
            .map(|v| {
                if v.kind() == "parenthesized_expression" {
                    v.named_child(0).unwrap_or(v)
                } else {
                    v
                }
            })
            .filter(|v| v.kind() == "identifier")
            .and_then(|v| v.utf8_text(bytes).ok());
        if default == Some(name) {
            return true;
        }
        let mut c = st.walk();
        let clauses: Vec<Node<'_>> = st
            .named_children(&mut c)
            .filter(|n| n.kind() == "export_clause")
            .collect();
        clauses.into_iter().any(|clause| {
            let mut c = clause.walk();
            let found = clause.named_children(&mut c).any(|spec| {
                spec.kind() == "export_specifier"
                    && spec
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(bytes).ok())
                        == Some(name)
            });
            found
        })
    })
}

/// The `name` of a class declaration, empty if it has none.
fn class_name(class: Node<'_>, bytes: &[u8]) -> String {
    class
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(bytes).ok())
        .unwrap_or_default()
        .to_string()
}

struct Python;

impl LangResolver for Python {
    /// `pkg/sub/mod.py` → `pkg.sub.mod`; a package's `__init__.py` is the
    /// package itself.
    fn package_from_path(&self, path: &str) -> Option<String> {
        let stem = strip_extension(path);
        let stem = stem
            .strip_suffix("/__init__")
            .or_else(|| (stem == "__init__").then_some(""))
            .unwrap_or(stem);
        Some(stem.replace('/', "."))
    }

    /// No leading `_` (a dunder is public), and not nested in a function:
    /// a decorator's `wrapper` is no name of its module.
    fn exported(&self, def: Node<'_>, name: &str, _bytes: &[u8]) -> Option<bool> {
        let mut cur = def.parent();
        while let Some(n) = cur {
            if n.kind() == "function_definition" {
                return Some(false);
            }
            cur = n.parent();
        }
        Some(!name.starts_with('_') || (name.starts_with("__") && name.ends_with("__")))
    }

    fn import_target(&self, imp: &ImportFact) -> String {
        let join = |tail: &str| {
            if imp.path.chars().all(|c| c == '.') {
                format!("{}{tail}", imp.path) // `from . import x` → `.x`
            } else {
                format!("{}.{tail}", imp.path)
            }
        };
        match (&imp.name, imp.wildcard) {
            (_, true) => join("*"),
            (Some(n), false) => join(n),
            (None, false) => imp.path.clone(),
        }
    }
}

struct Rust;

impl LangResolver for Rust {
    /// `crates/my-crate/src/a/b.rs` → `my_crate::a::b`; `src/lib.rs`,
    /// `main.rs` and `mod.rs` name their directory. A file outside any
    /// `src/` (a test, an example) is its stem.
    fn package_from_path(&self, path: &str) -> Option<String> {
        let stem = strip_extension(path);
        let parts: Vec<&str> = stem.split('/').collect();
        let Some(src) = parts.iter().rposition(|p| *p == "src") else {
            return Some(parts.last().copied().unwrap_or(stem).replace('-', "_"));
        };
        let krate = match src {
            0 => "crate".to_string(),
            i => parts[i - 1].replace('-', "_"),
        };
        let mut module: Vec<&str> = parts[src + 1..].to_vec();
        if matches!(module.last(), Some(&"lib" | &"main" | &"mod")) {
            module.pop();
        }
        let mut out = krate;
        for m in module {
            out.push_str("::");
            out.push_str(m);
        }
        Some(out)
    }

    /// Any `pub` (`pub(crate)` included); an `impl` has no visibility.
    fn exported(&self, def: Node<'_>, _name: &str, _bytes: &[u8]) -> Option<bool> {
        if def.kind() == "impl_item" {
            return None;
        }
        let mut cursor = def.walk();
        let public = def
            .children(&mut cursor)
            .any(|c| c.kind() == "visibility_modifier");
        Some(public)
    }

    fn expand_import_tree(&self, tree: Node<'_>, bytes: &[u8]) -> Vec<ImportFact> {
        let mut out = Vec::new();
        expand_use(tree, "", bytes, &mut out);
        out
    }

    fn import_target(&self, imp: &ImportFact) -> String {
        if imp.wildcard {
            format!("{}::*", imp.path)
        } else {
            imp.path.clone()
        }
    }
}

/// `prefix::segment`, where `self` names the prefix itself.
fn use_join(prefix: &str, segment: &str) -> String {
    match (prefix.is_empty(), segment) {
        (true, s) => s.to_string(),
        (false, "self") => prefix.to_string(),
        (false, s) => format!("{prefix}::{s}"),
    }
}

/// Flatten a Rust `use` tree: `a::{b::C, d as e, f::*}` is `a::b::C`,
/// `a::d` (alias `e`) and `a::f` (wildcard).
fn expand_use(node: Node<'_>, prefix: &str, bytes: &[u8], out: &mut Vec<ImportFact>) {
    let fact = |path: String, alias: Option<String>, wildcard: bool| ImportFact {
        path,
        alias,
        wildcard,
        ..Default::default()
    };
    match node.kind() {
        "use_as_clause" => {
            let path = node
                .child_by_field_name("path")
                .map(|p| compact(p, bytes))
                .unwrap_or_default();
            let alias = node.child_by_field_name("alias").map(|a| compact(a, bytes));
            out.push(fact(use_join(prefix, &path), alias, false));
        }
        "use_wildcard" => {
            let inner = node
                .named_child(0)
                .map(|p| compact(p, bytes))
                .unwrap_or_default();
            out.push(fact(use_join(prefix, &inner), None, true));
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                expand_use(child, prefix, bytes, out);
            }
        }
        "scoped_use_list" => {
            let path = node
                .child_by_field_name("path")
                .map(|p| compact(p, bytes))
                .unwrap_or_default();
            let prefix = use_join(prefix, &path);
            if let Some(list) = node.child_by_field_name("list") {
                expand_use(list, &prefix, bytes, out);
            }
        }
        k if k.contains("comment") => {}
        _ => out.push(fact(use_join(prefix, &compact(node, bytes)), None, false)),
    }
}

struct Go;

impl LangResolver for Go {
    fn package_from_path(&self, _path: &str) -> Option<String> {
        None // `package x` in the source
    }

    fn exported(&self, _def: Node<'_>, name: &str, _bytes: &[u8]) -> Option<bool> {
        Some(name.chars().next().is_some_and(char::is_uppercase))
    }

    fn import_target(&self, imp: &ImportFact) -> String {
        imp.path.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_derived_from_the_path() {
        let py = resolver_for(Lang::python());
        assert_eq!(
            py.package_from_path("pkg/sub/mod.py").as_deref(),
            Some("pkg.sub.mod")
        );
        assert_eq!(
            py.package_from_path("pkg/__init__.py").as_deref(),
            Some("pkg")
        );
        let rs = resolver_for(Lang::rust());
        assert_eq!(
            rs.package_from_path("crates/devctx-store/src/store.rs")
                .as_deref(),
            Some("devctx_store::store")
        );
        assert_eq!(
            rs.package_from_path("crates/x/src/lib.rs").as_deref(),
            Some("x")
        );
        assert_eq!(
            rs.package_from_path("src/a/mod.rs").as_deref(),
            Some("crate::a")
        );
        assert_eq!(
            rs.package_from_path("crates/x/tests/it.rs").as_deref(),
            Some("it")
        );
        let ts = resolver_for(Lang::typescript());
        assert_eq!(
            ts.package_from_path("src/app/x.ts").as_deref(),
            Some("src/app/x")
        );
        assert_eq!(resolver_for(Lang::java()).package_from_path("A.java"), None);
    }

    #[test]
    fn platform_names_come_from_the_definition() {
        let java = Lang::java();
        let r = resolver_for(java);
        assert!(r.is_platform(java.def(), "java.util.List"));
        assert!(r.is_platform(java.def(), "String"));
        assert!(!r.is_platform(java.def(), "com.example.Service"));
        let py = Lang::python();
        assert!(resolver_for(py).is_platform(py.def(), "len"));
        let rs = Lang::rust();
        assert!(resolver_for(rs).is_platform(rs.def(), "std::fs::read"));
        assert!(!resolver_for(rs).is_platform(rs.def(), "crate::store::Store"));
    }
}
