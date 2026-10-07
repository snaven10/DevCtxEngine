//! Parse-domain types: symbols, imports and the parsed-file result.

/// A code symbol (function, method, class, …) extracted from a source file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// Name qualified by every enclosing container, `Outer.Inner.method`
    /// (the file symbol's is its path, set by [`ParsedFile::assign_ids`]).
    pub qualified: String,
    /// Provisional signature: the definition's first line, whitespace
    /// collapsed, at most 200 characters.
    pub signature: String,
    /// Normalised parameter types (`Long,String`), only for languages that
    /// overload by them; part of the id's disambiguator (DD-3).
    pub params: Option<String>,
    /// Stable id (DD-3), `0` until [`ParsedFile::assign_ids`] runs.
    pub id: u64,
    /// The container's id, or the file symbol's for a top-level symbol;
    /// `None` for the file symbol itself.
    pub parent_id: Option<u64>,
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
    /// Byte offset of the callee.
    pub byte: usize,
    /// Byte offset of the enclosing function node, if there is one.
    pub source_byte: Option<usize>,
    /// Id of the source symbol, `0` until [`ParsedFile::assign_ids`] runs.
    pub src_id: u64,
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
    /// Extracted call-graph edges whose source is a named function, in source
    /// order. What `graph_edges` is made of.
    pub edges: Vec<GraphEdge>,
    /// Calls with no named enclosing function — module level, a class body,
    /// an anonymous function — in source order, `source` empty. They used to
    /// be dropped; the `edges` table keeps them, sourced from the innermost
    /// symbol around them or the file symbol.
    pub module_edges: Vec<GraphEdge>,
    /// The file's own symbol (`kind = file`), spanning the whole source.
    pub file_symbol: Symbol,
}
