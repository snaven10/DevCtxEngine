//! What one file says about the code beyond its symbols and calls
//! (PLAN-009 TASK-004, DD-5): its package, its imports, its supertypes, and
//! where it instantiates or names a type. All of it local: nothing here looks
//! at another file (the link pass of TASK-005 does).
//!
//! The symbols themselves stay [`Symbol`](crate::Symbol)s (with id, qualified
//! name, signature, package and `exported`) and the calls stay
//! [`GraphEdge`](crate::GraphEdge)s, because the chunker and `graph_edges`
//! read them as they are.

/// The structured facts of one parsed file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFacts {
    /// Java/Go: the declared package. Python/TS/JS/Rust: the module path
    /// derived from the file path (set by [`ParsedFile::assign_ids`]).
    ///
    /// [`ParsedFile::assign_ids`]: crate::ParsedFile::assign_ids
    pub package: Option<String>,
    /// One per name brought in, in source order.
    pub imports: Vec<ImportFact>,
    /// One per supertype named by a type definition.
    pub inherits: Vec<InheritFact>,
    /// Instantiations and type uses, one per occurrence.
    pub refs: Vec<RefFact>,
}

/// One name an import brings in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ImportFact {
    /// The module or full path as written: `java.util.List`, `./auth`,
    /// `os.path`, `.models` (relative), `crate::store`, `net/http`.
    pub path: String,
    /// The imported member, when the statement names one apart from the
    /// path (`from x import name`, `import { name } from './x'`, `default`).
    pub name: Option<String>,
    /// The local name it is bound to (`as alias`, a default import's name,
    /// `* as ns`, a Go package alias).
    pub alias: Option<String>,
    /// `.*`, `import *`, `* as ns`, `use x::*`.
    pub wildcard: bool,
    /// TypeScript/JavaScript `export … from`: the names are re-exported
    /// (a barrel), not bound in the file (PLAN-009 TASK-006); a Rust `pub
    /// use` (TASK-007): bound and re-exported.
    pub reexport: bool,
    /// What the `imports` edge records as its destination: the path joined
    /// with the name in the language's own syntax (`java.util.List`,
    /// `os.path.join`, `crate::a::B`, `./auth#login`, `net/http`), `*` for a
    /// wildcard.
    pub target: String,
    /// 1-based line.
    pub line: u32,
}

impl ImportFact {
    /// What the `imports` edge keeps besides its destination (`edges.hint`,
    /// PLAN-009 TASK-006): `export` for a re-export, `as <local>` for the
    /// name it binds when it is not the imported one. `None` when neither.
    pub fn hint(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.reexport {
            parts.push("export".to_string());
        }
        if let Some(a) = &self.alias {
            parts.push(format!("as {a}"));
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

/// A supertype named by a type definition: `extends`/`implements`, a Rust
/// `impl Trait for T` or supertrait, a Python base class, a Go embedded type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InheritFact {
    /// `inherits` or `implements` (the edge kind).
    pub kind: String,
    /// The supertype as written, without generic arguments or whitespace
    /// (`Base`, `com.x.Base`, `fmt::Display`).
    pub name: String,
    /// 1-based line.
    pub line: u32,
    /// Byte offset of the supertype node.
    pub byte: usize,
    /// The defining symbol (the class, the `impl`, the trait), `0` until
    /// [`ParsedFile::assign_ids`](crate::ParsedFile::assign_ids) runs.
    pub src_id: u64,
}

/// An instantiation (`new T(…)`, a Rust struct literal, a Go composite
/// literal) or a use of a type (field, parameter, variable, return type).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefFact {
    /// `instantiates` or `references` (the edge kind).
    pub kind: String,
    /// The type as written, without generic arguments (`Foo`, `a.b.Foo`,
    /// `crate::x::Foo`); `Self` is the enclosing type's name.
    pub name: String,
    /// 1-based line.
    pub line: u32,
    /// Byte offset of the type node.
    pub byte: usize,
    /// Byte offset of the enclosing named callable, if any.
    pub source_byte: Option<usize>,
    /// The symbol the occurrence is in, `0` until
    /// [`ParsedFile::assign_ids`](crate::ParsedFile::assign_ids) runs.
    pub src_id: u64,
}

/// Edge kind of an instantiation.
pub const INSTANTIATES: &str = "instantiates";
/// Edge kind of a type use.
pub const REFERENCES: &str = "references";
/// Edge kind of `extends` (and of a Python base, a supertrait, a Go embed).
pub const INHERITS: &str = "inherits";
/// Edge kind of `implements` (and of a Rust `impl Trait for`).
pub const IMPLEMENTS: &str = "implements";
/// Edge kind of an import.
pub const IMPORTS: &str = "imports";
/// Edge kind of containment (parent → child).
pub const CONTAINS: &str = "contains";
