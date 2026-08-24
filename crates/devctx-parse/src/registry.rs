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
    /// Query capturing definitions; the capture name is the symbol kind.
    pub symbols: String,
    /// Query capturing call callees as `@callee`.
    pub calls: String,
    /// Query capturing `@name`/`@type` pairs to resolve a receiver's type.
    /// Absent for untyped languages.
    #[serde(default)]
    pub types: Option<String>,
    /// Query capturing whole import statements as `@import`.
    pub imports: String,
    /// Node kinds that define a callable, for resolving an edge's source.
    pub function_kinds: Vec<String>,
    /// Node kinds that act as symbol containers, for the parent of a symbol and
    /// for telling a method from a function.
    pub container_kinds: Vec<String>,
}

impl LangDef {
    /// The `language` string recorded on this language's symbols.
    pub fn language(&self) -> &str {
        self.store_language.as_deref().unwrap_or(&self.name)
    }
}

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
    /// grammar and compiles all four of its queries, so a bad node kind fails
    /// here — naming the language and the query — rather than in somebody's
    /// index run.
    #[test]
    fn every_definition_compiles() {
        for def in ALL.iter() {
            let grammar = grammar_for(&def.grammar)
                .unwrap_or_else(|| panic!("`{}`: no grammar named `{}`", def.name, def.grammar));
            for (label, src) in [
                ("symbols", Some(&def.symbols)),
                ("calls", Some(&def.calls)),
                ("imports", Some(&def.imports)),
                ("types", def.types.as_ref()),
            ] {
                let Some(src) = src else { continue };
                if let Err(e) = Query::new(&grammar, src) {
                    panic!("`{}`: the `{label}` query does not compile: {e}", def.name);
                }
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

    #[test]
    fn tsx_is_recorded_as_typescript() {
        assert_eq!(by_name("tsx").unwrap().language(), "typescript");
        assert_eq!(by_name("java").unwrap().language(), "java");
    }
}
