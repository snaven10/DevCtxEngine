//! Parse-domain types: symbols, imports and the parsed-file result.

use crate::facts::FileFacts;

/// A code symbol (function, method, class, …) extracted from a source file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Symbol {
    /// Symbol name.
    pub name: String,
    /// Kind: one of [`DEFINITION_KINDS`](crate::registry::DEFINITION_KINDS)
    /// — `function`/`method`/`constructor`/`class`/`interface`/`enum`/
    /// `record`/`struct`/`trait`/`type`/`module`/`impl`/`field`/`const` —
    /// or `file`. `const` covers every module-level binding that is no
    /// callable: Go `var` as well as `const`, Rust `static` as well as
    /// `const`, a Python module assignment. A Rust `mod foo;` without a
    /// body is a `module` with nothing in it (its contents live in another
    /// file).
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
    /// Name qualified by every enclosing scope — container, enclosing symbol
    /// (module, function, class), TypeScript namespace, Go receiver — as
    /// `Outer.Inner.method`, `tests.helper`, `deco.wrapper` (the file
    /// symbol's is its path, set by [`ParsedFile::assign_ids`]).
    pub qualified: String,
    /// The definition up to its body (an arrow function's body for a
    /// `const x = () => …`), whitespace collapsed, at most 200 characters
    /// (DD-17): `pub fn open(path: &Path) -> Result<Self>`.
    pub signature: String,
    /// Visible outside its package/module, by what the language declares:
    /// `public` (Java; an interface member unless `private`), `export` or
    /// `export { name }` (TS/JS; a class member: not `private`/`protected`/
    /// `#x`, its class exported), `pub` of any kind (Rust), a capital name
    /// (Go), a top-level or class-member name without a leading `_`
    /// (Python; anything nested in a function is not). `None` when the
    /// language says nothing (a Rust `impl`). Not "visible to another file":
    /// Java package-private/`protected` and Go lowercase names are `false`
    /// though the rest of their package sees them (PLAN-009 DD-6).
    pub exported: Option<bool>,
    /// Normalised parameter types (`Long,String`), only for languages that
    /// overload by them (Java); part of the id's disambiguator (DD-3).
    pub params: Option<String>,
    /// The trait of the Rust `impl Trait for Type` the symbol sits in
    /// (`Display`, `From<A>`); part of the id's disambiguator (DD-3).
    pub trait_of: Option<String>,
    /// The self type's generic arguments and `where` clause of the Rust
    /// `impl` the symbol is or sits in (`<u8>`, `<T>whereT:Copy`); part of
    /// the id's disambiguator (DD-3), so `impl Foo<u8>` and `impl Foo<u16>`
    /// (and their methods) differ.
    pub impl_args: Option<String>,
    /// Which enclosing scopes of `qualified` are callables: one entry per
    /// scope, outermost first, `f` for a function or method, `s` otherwise
    /// (`Some("sf")` for `Outer.start.run`; Java adds the callable's
    /// parameter types, `Some("sf(int)")`); `None` when none is. Part of the
    /// id's disambiguator (DD-3): a function `a` and a module `a` both
    /// qualify their contents as `a.P`.
    pub scope_shape: Option<String>,
    /// Stable id (DD-3), `0` until [`ParsedFile::assign_ids`] runs.
    pub id: u64,
    /// The container's id, or the file symbol's for a top-level symbol;
    /// `None` for the file symbol itself.
    pub parent_id: Option<u64>,
}

/// An import/use statement, as text (the file chunk lists them; the
/// structured form is [`FileFacts::imports`]).
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
    /// Package, structured imports, supertypes, instantiations and type uses
    /// (PLAN-009 TASK-004).
    pub facts: FileFacts,
}
