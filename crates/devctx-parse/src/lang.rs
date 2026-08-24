//! Language lookup: the handle a caller holds, and extension detection.
//!
//! Everything that describes a language — its queries, its extensions, which
//! node kinds are callables or containers — lives in `languages/*.json` and is
//! reached through [`crate::registry`]. This module is only the handle and the
//! two lookups built on it.

use tree_sitter::Language;

use crate::registry::{self, LangDef};

/// A language with a wired-in tree-sitter parser.
///
/// A handle onto its embedded definition, not a copy of it: `Copy`, and free to
/// pass around. It used to be an enum whose seven variants each fanned out into
/// six `match` arms, which is what made adding a language a nine-edit job.
#[derive(Debug, Clone, Copy)]
pub struct Lang(&'static LangDef);

impl Lang {
    /// The language registered under `name`, if any.
    pub fn named(name: &str) -> Option<Self> {
        registry::by_name(name).map(Self)
    }

    /// Python.
    pub fn python() -> Self {
        Self::named("python").expect("python is registered")
    }
    /// JavaScript (incl. JSX).
    pub fn javascript() -> Self {
        Self::named("javascript").expect("javascript is registered")
    }
    /// TypeScript.
    pub fn typescript() -> Self {
        Self::named("typescript").expect("typescript is registered")
    }
    /// TypeScript with JSX (`.tsx`).
    pub fn tsx() -> Self {
        Self::named("tsx").expect("tsx is registered")
    }
    /// Go.
    pub fn go() -> Self {
        Self::named("go").expect("go is registered")
    }
    /// Java.
    pub fn java() -> Self {
        Self::named("java").expect("java is registered")
    }
    /// Rust.
    pub fn rust() -> Self {
        Self::named("rust").expect("rust is registered")
    }

    /// The underlying definition.
    pub fn def(self) -> &'static LangDef {
        self.0
    }

    /// The store `language` string (`tsx` reports `typescript`).
    pub fn name(self) -> &'static str {
        self.0.language()
    }

    /// The registry key, which unlike [`name`](Self::name) distinguishes `tsx`.
    pub fn key(self) -> &'static str {
        &self.0.name
    }

    /// The tree-sitter grammar.
    pub fn grammar(self) -> Language {
        registry::grammar_for(&self.0.grammar).expect("a registered grammar")
    }

    /// tree-sitter query capturing symbol definitions. The capture name is the
    /// symbol kind (`function`/`class`/…); `function` is reclassified to
    /// `method` at parse time when nested in a container.
    pub fn symbol_query(self) -> &'static str {
        &self.0.symbols
    }

    /// tree-sitter query capturing call callees as `@callee`.
    pub fn calls_query(self) -> &'static str {
        &self.0.calls
    }

    /// tree-sitter query capturing `@name`/`@type` binding pairs to resolve a
    /// receiver's type. `None` for untyped languages (JavaScript).
    pub fn type_bindings_query(self) -> Option<&'static str> {
        self.0.types.as_deref()
    }

    /// tree-sitter query capturing whole import statements as `@import`.
    pub fn import_query(self) -> &'static str {
        &self.0.imports
    }

    /// Node kinds that define a callable, for resolving an edge's source.
    ///
    /// Per language, not shared. While these were one global list, adding
    /// Java's `constructor_declaration` would have added it to Python and Rust
    /// too — so it was never added, and every call inside a Java constructor
    /// was dropped for want of a source symbol.
    pub fn function_kinds(self) -> &'static [String] {
        &self.0.function_kinds
    }

    /// Node kinds that act as symbol containers, for a symbol's parent and for
    /// telling a method from a function.
    pub fn container_kinds(self) -> &'static [String] {
        &self.0.container_kinds
    }
}

impl PartialEq for Lang {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for Lang {}

/// Every language with a wired parser.
pub fn all() -> Vec<Lang> {
    registry::ALL.iter().map(Lang).collect()
}

/// Detect a parseable language from a file extension (lowercased, no dot).
pub fn lang_for_extension(ext: &str) -> Option<Lang> {
    registry::for_extension(ext).map(Lang)
}

/// Non-parseable languages that are still indexed as raw text (one file-spanning
/// chunk). Maps extension → language name.
pub fn raw_text_language(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sass" => "sass",
        "less" => "less",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "xml" => "xml",
        "md" | "markdown" => "markdown",
        "sql" => "sql",
        "graphql" | "gql" => "graphql",
        "proto" => "protobuf",
        // Kotlin has no tree-sitter grammar wired yet: index as text, but its
        // Spring routes are still extracted (see routes::extract_routes).
        "kt" | "kts" => "kotlin",
        _ => return None,
    })
}
