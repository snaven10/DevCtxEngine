//! Parse-domain types: symbols, imports and the parsed-file result.

/// A code symbol (function, method, class, …) extracted from a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// Symbol name.
    pub name: String,
    /// Kind: `function`/`method`/`class`/`struct`/`enum`/`trait`/`interface`/
    /// `type`/`module`.
    pub kind: String,
    /// Source language (store `language` value).
    pub language: String,
    /// 1-based first line of the definition.
    pub start_line: u32,
    /// 1-based last line of the definition.
    pub end_line: u32,
    /// Byte offset of the definition start (multibyte-safe).
    pub start_byte: usize,
    /// Byte offset of the definition end.
    pub end_byte: usize,
    /// 1-based first line of the doc comment above the definition, or
    /// `start_line` when there is none.
    pub doc_start_line: u32,
    /// Byte offset of the doc comment above the definition, or `start_byte`
    /// when there is none. Chunks slice from here so the prose that explains a
    /// symbol travels with it.
    pub doc_start_byte: usize,
    /// Enclosing symbol name (class/impl/…), if any.
    pub parent: Option<String>,
}

/// An import/use statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// Raw statement text.
    pub statement: String,
    /// 1-based line.
    pub line: u32,
}

/// A call-graph edge: `source` calls `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdge {
    /// Enclosing symbol making the call.
    pub source: String,
    /// Called symbol (bare callee name).
    pub target: String,
    /// Edge kind (`calls`).
    pub kind: String,
    /// 1-based line of the call.
    pub line: u32,
}

/// The result of parsing one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedFile {
    /// Detected language.
    pub language: String,
    /// Extracted symbols, in source order.
    pub symbols: Vec<Symbol>,
    /// Extracted imports, in source order.
    pub imports: Vec<Import>,
    /// Extracted call-graph edges, in source order.
    pub edges: Vec<GraphEdge>,
}
