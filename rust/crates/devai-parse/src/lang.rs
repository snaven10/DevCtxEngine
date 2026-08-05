//! Supported languages: grammar, tree-sitter queries, and extension mapping.

use tree_sitter::Language;

/// A language with a wired-in tree-sitter parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// Python.
    Python,
    /// JavaScript (incl. JSX).
    JavaScript,
    /// TypeScript.
    TypeScript,
    /// TypeScript with JSX (`.tsx`).
    Tsx,
    /// Go.
    Go,
    /// Java.
    Java,
    /// Rust.
    Rust,
}

impl Lang {
    /// The store `language` string for this language (`Tsx` reports `typescript`).
    pub fn name(self) -> &'static str {
        match self {
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::TypeScript | Lang::Tsx => "typescript",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::Rust => "rust",
        }
    }

    /// The tree-sitter grammar.
    pub fn grammar(self) -> Language {
        match self {
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }

    /// tree-sitter query capturing symbol definitions. The capture name is the
    /// symbol kind (`function`/`class`/…); `function` is reclassified to
    /// `method` at parse time when nested in a class/impl.
    pub fn symbol_query(self) -> &'static str {
        match self {
            Lang::Python => {
                "(function_definition name: (identifier) @function)
                 (class_definition name: (identifier) @class)"
            }
            Lang::JavaScript => {
                "(function_declaration name: (identifier) @function)
                 (class_declaration name: (identifier) @class)
                 (method_definition name: (property_identifier) @method)"
            }
            Lang::TypeScript | Lang::Tsx => {
                "(function_declaration name: (identifier) @function)
                 (class_declaration name: (type_identifier) @class)
                 (interface_declaration name: (type_identifier) @interface)
                 (method_definition name: (property_identifier) @method)"
            }
            Lang::Go => {
                "(function_declaration name: (identifier) @function)
                 (method_declaration name: (field_identifier) @method)
                 (type_declaration (type_spec name: (type_identifier) @type))"
            }
            Lang::Java => {
                "(class_declaration name: (identifier) @class)
                 (interface_declaration name: (identifier) @interface)
                 (enum_declaration name: (identifier) @enum)
                 (method_declaration name: (identifier) @method)"
            }
            Lang::Rust => {
                "(function_item name: (identifier) @function)
                 (struct_item name: (type_identifier) @struct)
                 (enum_item name: (type_identifier) @enum)
                 (trait_item name: (type_identifier) @trait)
                 (mod_item name: (identifier) @module)"
            }
        }
    }

    /// tree-sitter query capturing call callees as `@callee`.
    pub fn calls_query(self) -> &'static str {
        match self {
            Lang::Python => {
                "(call function: (identifier) @callee)
                 (call function: (attribute attribute: (identifier) @callee))"
            }
            Lang::JavaScript | Lang::TypeScript | Lang::Tsx => {
                "(call_expression function: (identifier) @callee)
                 (call_expression function: (member_expression property: (property_identifier) @callee))"
            }
            Lang::Go => {
                "(call_expression function: (identifier) @callee)
                 (call_expression function: (selector_expression field: (field_identifier) @callee))"
            }
            Lang::Java => "(method_invocation name: (identifier) @callee)",
            Lang::Rust => {
                "(call_expression function: (identifier) @callee)
                 (call_expression function: (field_expression field: (field_identifier) @callee))
                 (call_expression function: (scoped_identifier name: (identifier) @callee))"
            }
        }
    }

    /// tree-sitter query capturing whole import statements as `@import`.
    pub fn import_query(self) -> &'static str {
        match self {
            Lang::Python => "(import_statement) @import (import_from_statement) @import",
            Lang::JavaScript | Lang::TypeScript | Lang::Tsx => "(import_statement) @import",
            Lang::Go => "(import_declaration) @import",
            Lang::Java => "(import_declaration) @import",
            Lang::Rust => "(use_declaration) @import",
        }
    }
}

/// All languages with wired parsers.
pub const ALL: &[Lang] = &[
    Lang::Python,
    Lang::JavaScript,
    Lang::TypeScript,
    Lang::Tsx,
    Lang::Go,
    Lang::Java,
    Lang::Rust,
];

/// Node kinds that define a callable (for resolving an edge's source symbol).
pub const FUNCTION_KINDS: &[&str] = &[
    "function_definition",
    "function_declaration",
    "method_declaration",
    "method_definition",
    "function_item",
];

/// Node kinds that act as symbol containers (for parent + method detection).
pub const CONTAINER_KINDS: &[&str] = &[
    "class_definition",
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "impl_item",
    "trait_item",
];

/// Detect a parseable language from a file extension (lowercased, no dot).
pub fn lang_for_extension(ext: &str) -> Option<Lang> {
    Some(match ext {
        "py" | "pyi" => Lang::Python,
        "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
        "ts" | "mts" | "cts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "go" => Lang::Go,
        "java" => Lang::Java,
        "rs" => Lang::Rust,
        _ => return None,
    })
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
        _ => return None,
    })
}
