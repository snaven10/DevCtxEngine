//! The language registry: what DevCtxEngine knows how to parse.
//!
//! A language is a JSON file under `languages/`, embedded into the binary at
//! compile time by [`include_str!`]. Nothing is read from disk at runtime and
//! there is no user override — the files are sources of this crate, like any
//! other.
//!
//! The point is not configurability. It is that a language's definition used to
//! be spread across six `match` arms and two shared constants, so reading "what
//! does Java do" meant jumping around a file and mentally reassembling it.
//! Adding one meant nine separate edits. Now it is one file you can read
//! top to bottom, plus a dependency and a line in [`grammar_for`].
//!
//! **The grammar cannot come from the JSON.** Grammars are compiled C linked at
//! build time (`tree_sitter_java::LANGUAGE` and friends), so [`grammar_for`]
//! stays a hand-written table. Loading them from `.so` files would mean an
//! unstable ABI and trusting third-party binaries; compiling them at runtime
//! would mean a C toolchain on the user's machine. Neither belongs in a tool
//! people install with one command.
//!
//! The cost of moving queries out of Rust is that a malformed one stops being a
//! compile error. [`tree_sitter::Query::new`] rejects node kinds that do not
//! exist in the grammar, and `every_definition_compiles` checks all of them, so
//! CI catches what the compiler used to. What neither catches is a query that
//! is valid and matches nothing — the failure mode that made the call graph
//! look empty for months.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;
use tree_sitter::Language;

/// Everything about one language except its grammar.
#[derive(Debug, Clone, Deserialize)]
pub struct LangDef {
    /// Registry key, and the `language` recorded on a symbol unless
    /// `store_language` overrides it.
    pub name: String,
    /// Which compiled grammar to use (see [`grammar_for`]).
    pub grammar: String,
    /// File extensions, lowercased and without the dot.
    pub extensions: Vec<String>,
    /// What to record as the symbol's language. `tsx` reports `typescript`,
    /// because the distinction is a grammar detail and nobody searches for it.
    #[serde(default)]
    pub store_language: Option<String>,
    /// Query capturing definitions, `tags.scm` style (PLAN-009 DD-5): the
    /// definition node as `@definition.<kind>` and its name as `@name`. One
    /// node captured by several patterns keeps the first pattern's kind (an
    /// arrow-valued field is a `method`, not a `field`).
    pub definitions: String,
    /// Query capturing references: `@reference.call` with the callee as
    /// `@name` (and, for a path call like Rust's `Foo::bar`, the path as
    /// `@path`); `@reference.new` with the instantiated type as `@type`;
    /// `@reference.type` around a type use (a field's, a parameter's, a
    /// variable's or a return type), the node itself or its `@type` child,
    /// whose type names ([`type_names`](Self::type_names)) are each one
    /// reference.
    pub references: String,
    /// Query capturing supertypes inside a type definition:
    /// `@inherit.extends` / `@inherit.implements` on the supertype node.
    #[serde(default)]
    pub inherits: Option<String>,
    /// Query capturing `@name`/`@type` pairs to resolve a receiver's type.
    /// Absent for untyped languages.
    #[serde(default)]
    pub types: Option<String>,
    /// Query capturing imports: the statement as `@import`, and what it
    /// brings in as `@import.path` (module or full path), `@import.name`,
    /// `@import.alias`, `@import.wildcard`, `@import.default` (a default
    /// import's local name), `@import.namespace` (`* as x`), or
    /// `@import.tree` for a language whose import is a tree the resolver
    /// expands (a Rust `use`).
    pub imports: String,
    /// Query capturing the file's declared package as `@package` (Java,
    /// Go). The others derive it from the path.
    #[serde(default)]
    pub package: Option<String>,
    /// Node kinds that name a type inside a `@reference.type` capture: each
    /// outermost one is a reference (`List<Foo>` is `List` and `Foo`).
    #[serde(default)]
    pub type_names: Vec<String>,
    /// Node kinds that define a callable, for resolving an edge's source.
    pub function_kinds: Vec<String>,
    /// Node kinds that act as symbol containers, for the parent of a symbol and
    /// for telling a method from a function.
    pub container_kinds: Vec<String>,
    /// Node kinds that are not symbols but name a scope (a TypeScript
    /// `namespace`): they enter the qualified name of what they enclose, as
    /// every enclosing symbol and container does (PLAN-009 DD-3).
    #[serde(default)]
    pub scope_kinds: Vec<String>,
    /// Whether the language overloads callables by parameter types with
    /// separate bodies (Java), so that a symbol's id carries them (PLAN-009
    /// DD-3). Not TypeScript: its overload signatures are not symbols.
    #[serde(default)]
    pub overloads: bool,
    /// Prefixes of names that belong to the platform (JDK, `std`, the Go
    /// standard library) — evidence for `external` (PLAN-009 DD-9).
    #[serde(default)]
    pub platform_prefixes: Vec<String>,
    /// Bare names that belong to the platform (Python builtins, JS globals,
    /// `java.lang` types): evidence for `external` too.
    #[serde(default)]
    pub builtins: Vec<String>,
}

impl LangDef {
    /// The `language` string recorded on this language's symbols.
    pub fn language(&self) -> &str {
        self.store_language.as_deref().unwrap_or(&self.name)
    }
}

/// Every kind a definitions query may capture (`@definition.<kind>`).
///
/// A kind is part of the symbol id through its class
/// ([`devctx_core::symbol_id::kind_class`]): a misspelt one
/// (`@definition.contructor`) would be a class of its own and move every id
/// it touches without anything failing, so `every_definition_compiles`
/// rejects any kind not listed here. `method` and `constructor` are also
/// derived at parse time (a `function` in a container, a JS `constructor`).
pub const DEFINITION_KINDS: &[&str] = &[
    "function",
    "method",
    "constructor",
    "class",
    "interface",
    "enum",
    "record",
    "struct",
    "trait",
    "type",
    "module",
    "impl",
    "field",
    "const",
];

/// The embedded definitions, in registry order.
const SOURCES: &[&str] = &[
    include_str!("../languages/python.json"),
    include_str!("../languages/javascript.json"),
    include_str!("../languages/typescript.json"),
    include_str!("../languages/tsx.json"),
    include_str!("../languages/go.json"),
    include_str!("../languages/java.json"),
    include_str!("../languages/rust.json"),
];

/// Manual version of the extraction *logic* in `parser.rs` (receiver
/// normalisation, edge resolution, symbol kinds). Bump it whenever a change
/// there alters what a re-parse of the same source produces; the embedded
/// `languages/*.json` are covered by the hash and need no bump.
///
/// What the fingerprint does NOT cover: the tree-sitter grammar versions
/// (a `Cargo.lock` bump that changes parse trees) and the route extraction in
/// `devctx-index`'s `routes.rs`. A change to either needs a manual bump here,
/// or existing indexes keep reading as fresh. Only a full run (`index --full`)
/// stamps the fingerprint; incremental runs never re-stamp an older index.
///
/// 2: the `symbols` and `edges` tables (PLAN-009 TASK-003) — an index made
/// before them has neither, so it must read as stale.
/// 3: structured extraction (PLAN-009 TASK-004) — new symbol kinds, `impl`
/// symbols, the Java overload shape in ids, every edge kind.
/// 4: review of TASK-004 — a Rust `impl`'s generic arguments and `where`
/// clause without trailing commas (ids stable under `rustfmt`); a named
/// callable's `graph_edges` source is its symbol's qualified name
/// (`traced.wrapper`, `C.init.run`, `C.m.f`; `const x = function named()`
/// is `x`); `export default name;` exports `name`.
/// 5: review of 3ad7d54 — comments inside an `impl`'s `where` clause or
/// generic arguments are no part of its id; `(u8,)` keeps its comma (a
/// one-element tuple is not `u8`); `export default (name);` exports `name`.
///
/// The fingerprint hashes the JSON, not the Rust code, on purpose: a
/// refactor, a comment or `rustfmt` must not make every user run `--full`.
/// What is versioned is the *output*: `tests/extractor_golden.rs` pins the
/// kinds, qualified names and ids the extractor produces for a fixture in
/// every language, together with the version they were produced under, and
/// fails when the output changes without a bump here.
pub const EXTRACTOR_VERSION: u32 = 5;

/// FNV-1a 64-bit — stable across platforms and releases, unlike `DefaultHasher`.
fn fnv1a(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// `v<EXTRACTOR_VERSION>-<fnv1a of every source>` for the given definitions.
fn fingerprint_of(version: u32, sources: &[&str]) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for src in sources {
        h = fnv1a(h, src.as_bytes());
        h = fnv1a(h, &[0]); // keeps ["ab", "c"] apart from ["a", "bc"]
    }
    format!("v{version}-{h:016x}")
}

/// What produced an index: [`EXTRACTOR_VERSION`] plus a hash of the embedded
/// language definitions. An index stamped with a different value was built by
/// an extractor that may have produced different symbols and edges.
pub fn extractor_fingerprint() -> String {
    fingerprint_of(EXTRACTOR_VERSION, SOURCES)
}

/// Every language with a wired parser.
///
/// Parsed once. A malformed file panics here rather than degrading into "that
/// language silently stopped being indexed", which is the failure nobody
/// notices — and `every_definition_compiles` means it cannot reach a release.
pub static ALL: LazyLock<Vec<LangDef>> = LazyLock::new(|| {
    SOURCES
        .iter()
        .map(|raw| serde_json::from_str(raw).expect("an embedded language definition is malformed"))
        .collect()
});

/// Extension → definition, built once from [`ALL`].
static BY_EXTENSION: LazyLock<HashMap<&'static str, &'static LangDef>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for def in ALL.iter() {
        for ext in &def.extensions {
            map.insert(ext.as_str(), def);
        }
    }
    map
});

/// The compiled grammar for a `grammar` key.
///
/// The one hand-written table left, and the reason a genuinely new language
/// still costs a dependency and a line here rather than only a JSON file.
pub fn grammar_for(key: &str) -> Option<Language> {
    Some(match key {
        "python" => tree_sitter_python::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        _ => return None,
    })
}

/// The definition for a file extension (lowercased, no dot).
pub fn for_extension(ext: &str) -> Option<&'static LangDef> {
    BY_EXTENSION.get(ext).copied()
}

/// The definition registered under `name`.
pub fn by_name(name: &str) -> Option<&'static LangDef> {
    ALL.iter().find(|d| d.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Query;

    /// What the compiler used to do for free.
    ///
    /// Moving the queries into JSON traded a compile error for a runtime one.
    /// This is the trade being paid back: every embedded definition resolves its
    /// grammar and compiles every query it has (definitions, references,
    /// inherits, imports, package, types), with only the captures the parser
    /// reads, and every node kind it names exists — so a bad node kind or a
    /// misspelt capture fails here, naming the language and the query, rather
    /// than in somebody's index run.
    const DEFINITION_CAPTURES: &[&str] = &["name"];
    const REFERENCE_CAPTURES: &[&str] = &[
        "reference.call",
        "reference.new",
        "reference.type",
        "name",
        "path",
        "type",
    ];
    const INHERIT_CAPTURES: &[&str] = &["inherit.extends", "inherit.implements"];
    const IMPORT_CAPTURES: &[&str] = &[
        "import",
        "import.path",
        "import.name",
        "import.alias",
        "import.wildcard",
        "import.default",
        "import.namespace",
        "import.tree",
    ];

    #[test]
    fn every_definition_compiles() {
        for def in ALL.iter() {
            check(def);
        }
    }

    /// A misspelt definition kind or node kind is caught, not silently
    /// turned into a new id class or a scope that never matches.
    #[test]
    fn a_misspelt_kind_is_rejected() {
        let java = by_name("java").unwrap();
        let mut typo = java.clone();
        typo.definitions = typo
            .definitions
            .replace("@definition.constructor", "@definition.contructor");
        let err = std::panic::catch_unwind(|| check(&typo)).unwrap_err();
        let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(msg.contains("contructor"), "{msg}");
        for field in ["container", "scope"] {
            let mut typo = java.clone();
            let list = if field == "container" {
                &mut typo.container_kinds
            } else {
                &mut typo.scope_kinds
            };
            list.push("class_declaraton".into());
            let err = std::panic::catch_unwind(|| check(&typo)).unwrap_err();
            let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
            assert!(msg.contains("class_declaraton"), "{field}: {msg}");
        }
        check(java);
    }

    /// Every check of `every_definition_compiles` on one definition.
    fn check(def: &LangDef) {
        {
            let grammar = grammar_for(&def.grammar)
                .unwrap_or_else(|| panic!("`{}`: no grammar named `{}`", def.name, def.grammar));
            for (label, src, allowed) in [
                ("definitions", Some(&def.definitions), DEFINITION_CAPTURES),
                ("references", Some(&def.references), REFERENCE_CAPTURES),
                ("inherits", def.inherits.as_ref(), INHERIT_CAPTURES),
                ("imports", Some(&def.imports), IMPORT_CAPTURES),
                ("package", def.package.as_ref(), &["package"][..]),
                ("types", def.types.as_ref(), &["name", "type"][..]),
            ] {
                let Some(src) = src else { continue };
                let query = Query::new(&grammar, src).unwrap_or_else(|e| {
                    panic!("`{}`: the `{label}` query does not compile: {e}", def.name)
                });
                // A capture the parser does not read is a typo that would
                // silently match nothing useful.
                for name in query.capture_names() {
                    let known = allowed.contains(name)
                        || (label == "definitions"
                            && name
                                .strip_prefix("definition.")
                                .is_some_and(|k| DEFINITION_KINDS.contains(&k)));
                    assert!(
                        known,
                        "`{}`: `{label}` captures unknown `@{name}`",
                        def.name
                    );
                }
            }
            for kind in def
                .type_names
                .iter()
                .chain(&def.function_kinds)
                .chain(&def.container_kinds)
                .chain(&def.scope_kinds)
            {
                assert!(
                    grammar.id_for_node_kind(kind, true) != 0,
                    "`{}`: no node kind `{kind}`",
                    def.name
                );
            }
        }
    }

    /// Two languages claiming the same extension would make which one parses a
    /// file depend on registry order, which is not a thing anyone should have to
    /// know.
    #[test]
    fn no_extension_is_claimed_twice() {
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for def in ALL.iter() {
            for ext in &def.extensions {
                if let Some(other) = seen.insert(ext, &def.name) {
                    panic!("`{ext}` is claimed by both `{other}` and `{}`", def.name);
                }
            }
        }
    }

    /// The store tells a file a graph-less binary wrote by its language: the
    /// list it uses must be exactly the languages parsed here.
    #[test]
    fn the_graph_languages_are_the_parsed_ones() {
        let parsed: std::collections::BTreeSet<&str> = ALL.iter().map(LangDef::language).collect();
        let listed: std::collections::BTreeSet<&str> = devctx_core::symbol_id::GRAPH_LANGUAGES
            .iter()
            .copied()
            .collect();
        assert_eq!(parsed, listed);
    }

    #[test]
    fn tsx_is_recorded_as_typescript() {
        assert_eq!(by_name("tsx").unwrap().language(), "typescript");
        assert_eq!(by_name("java").unwrap().language(), "java");
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;

    #[test]
    fn the_fingerprint_is_stable_between_calls() {
        assert_eq!(extractor_fingerprint(), extractor_fingerprint());
        assert!(extractor_fingerprint().starts_with(&format!("v{EXTRACTOR_VERSION}-")));
    }

    #[test]
    fn the_fingerprint_changes_with_a_definition_or_the_version() {
        let base = fingerprint_of(1, &["{\"a\":1}", "{}"]);
        assert_eq!(base, fingerprint_of(1, &["{\"a\":1}", "{}"]));
        assert_ne!(base, fingerprint_of(1, &["{\"a\":2}", "{}"]));
        assert_ne!(base, fingerprint_of(2, &["{\"a\":1}", "{}"]));
        assert_ne!(
            fingerprint_of(1, &["ab", "c"]),
            fingerprint_of(1, &["a", "bc"])
        );
    }
}
