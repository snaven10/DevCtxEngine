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
pub mod go;
pub mod java;
pub mod link;
pub mod python;
pub mod rust;
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

    /// Whether an attribute of `self`/`this` no scope binds is a field the
    /// link pass types by its declaration (`member x this`): Rust and Go
    /// declare fields apart from the methods, Python assigns them anywhere.
    /// Java and TypeScript read such a name as before (TASK-005/006).
    fn fields_by_link(&self) -> bool {
        false
    }

    /// Whether a binding of the module's own scope is seen from inside a
    /// function whatever its position (Python: a function body runs after
    /// the module's assignments).
    fn late_module_bindings(&self) -> bool {
        false
    }

    /// The type a literal receiver of node kind `kind` has (`"x".equals` →
    /// `String`).
    fn literal_type(&self, kind: &str) -> Option<&'static str> {
        match kind {
            "string_literal" => Some("String"),
            "class_literal" => Some("Class"),
            _ => None,
        }
    }

    /// A type as written in a declaration (`node`), reduced to what a
    /// lookup needs.
    fn type_text(&self, node: Node<'_>, bytes: &[u8]) -> Option<scope::TypeText> {
        scope::TypeText::parse(node.utf8_text(bytes).ok()?)
    }

    /// What the `imports` edge of `imp` keeps besides its destination
    /// (`edges.hint`).
    fn import_hint(&self, imp: &ImportFact) -> Option<String> {
        imp.hint()
    }
}

/// The resolver of `lang`.
pub fn resolver_for(lang: Lang) -> &'static dyn LangResolver {
    match lang.key() {
        "java" => &java::Java,
        "typescript" | "tsx" | "javascript" => &Script,
        "python" => &python::Python,
        "rust" => &rust::Rust,
        "go" => &go::Go,
        _ => &Generic,
    }
}

/// `path` without its last extension (`src/a.ts` → `src/a`).
pub(crate) fn strip_extension(path: &str) -> &str {
    match path.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() && !stem.ends_with('/') => stem,
        _ => path,
    }
}

/// The text of `node` without whitespace.
pub(crate) fn compact(node: Node<'_>, bytes: &[u8]) -> String {
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
