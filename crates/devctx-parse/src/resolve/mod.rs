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
//! This is the part of DD-5's trait that TASK-004 needs (the local facts).
//! The cross-file part — which files or symbols an import reaches — is the
//! link pass's (TASK-005), and gets its methods there.

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
    /// file (`public`, `export`, `pub`, a capital Go name, no leading `_`
    /// in Python); `None` when the language says nothing (a Rust `impl`).
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
}

/// The resolver of `lang`.
pub fn resolver_for(lang: Lang) -> &'static dyn LangResolver {
    match lang.key() {
        "java" => &Java,
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

struct Java;

impl LangResolver for Java {
    fn package_from_path(&self, _path: &str) -> Option<String> {
        None // `package x.y;` in the source
    }

    /// `public`, or a member of an interface or annotation (implicitly
    /// public unless `private`).
    fn exported(&self, def: Node<'_>, _name: &str, bytes: &[u8]) -> Option<bool> {
        let modifiers = {
            let mut cursor = def.walk();
            let found = def
                .named_children(&mut cursor)
                .find(|c| c.kind() == "modifiers")
                .and_then(|m| m.utf8_text(bytes).ok())
                .unwrap_or_default();
            found
        };
        let has = |word: &str| modifiers.split_whitespace().any(|w| w == word);
        if has("public") {
            return Some(true);
        }
        if has("private") {
            return Some(false);
        }
        let in_interface = def
            .parent()
            .and_then(|body| body.parent())
            .is_some_and(|c| {
                matches!(
                    c.kind(),
                    "interface_declaration" | "annotation_type_declaration"
                )
            });
        Some(in_interface)
    }

    fn import_target(&self, imp: &ImportFact) -> String {
        if imp.wildcard {
            format!("{}.*", imp.path)
        } else {
            imp.path.clone()
        }
    }
}

/// TypeScript, TSX and JavaScript.
struct Script;

impl LangResolver for Script {
    fn package_from_path(&self, path: &str) -> Option<String> {
        Some(strip_extension(path).to_string())
    }

    /// Top level: under an `export` statement. A class member: not
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
            return Some(class.is_some_and(|c| under_export(c)));
        }
        Some(under_export(def))
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

    fn exported(&self, _def: Node<'_>, name: &str, _bytes: &[u8]) -> Option<bool> {
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
