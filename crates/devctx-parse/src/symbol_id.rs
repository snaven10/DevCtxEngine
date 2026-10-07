//! Assigning stable ids to a parsed file's symbols and edges (PLAN-009 DD-3).
//!
//! The hash itself lives in [`devctx_core::symbol_id`], shared with the store,
//! which re-keys a renamed file without re-parsing it. What lives here is the
//! part that needs the parse: which disambiguator each symbol gets, which
//! symbol contains which, and which symbol each call comes from.

use std::collections::{HashMap, HashSet};

pub use devctx_core::symbol_id::{
    file_symbol_id, kind_class, rename_delta, sym_hex, symbol_id, FILE_KIND,
};

use crate::types::{GraphEdge, ParsedFile, Symbol};

impl ParsedFile {
    /// Give every symbol its id and parent id, name the file symbol after
    /// `file`, and point every edge at its source symbol.
    ///
    /// - **Disambiguator:** the normalised parameter types where the language
    ///   overloads (so `run(Long)` and `run(String)` differ), else empty. Two
    ///   symbols of the file still sharing kind class, qualified name and
    ///   disambiguator take an ordinal in source order (`#1`, `#2`…), and so
    ///   does a hash collision.
    /// - **Parent:** the innermost symbol whose span contains this one; else,
    ///   for a symbol whose container is not itself a symbol (a Rust `impl`),
    ///   the type of that name in this file; else the file symbol.
    /// - **Edge source:** the symbol defined by the enclosing function node;
    ///   else the innermost symbol around the call; else the file symbol.
    pub fn assign_ids(&mut self, repo: &str, file: &str) {
        let file_id = file_symbol_id(repo, file);
        self.file_symbol.kind = FILE_KIND.to_string();
        self.file_symbol.qualified = file.to_string();
        self.file_symbol.name = file.rsplit('/').next().unwrap_or(file).to_string();
        self.file_symbol.id = file_id;
        self.file_symbol.parent_id = None;

        let mut used: HashSet<u64> = HashSet::from([file_id]);
        let mut seen: HashMap<(String, String, String), usize> = HashMap::new();
        for sym in &mut self.symbols {
            let class = kind_class(&sym.kind).to_string();
            let base = sym.params.clone().unwrap_or_default();
            let n = seen
                .entry((class.clone(), sym.qualified.clone(), base.clone()))
                .or_insert(0);
            let mut ordinal = *n;
            *n += 1;
            loop {
                let dis = if ordinal == 0 {
                    base.clone()
                } else {
                    format!("{base}#{ordinal}")
                };
                let id = symbol_id(repo, file, &class, &sym.qualified, &dis);
                if used.insert(id) {
                    sym.id = id;
                    break;
                }
                ordinal += 1;
            }
        }

        let parents: Vec<Option<u64>> = (0..self.symbols.len())
            .map(|i| Some(parent_of(&self.symbols, i).unwrap_or(file_id)))
            .collect();
        for (sym, parent) in self.symbols.iter_mut().zip(parents) {
            sym.parent_id = parent;
        }

        let by_start: HashMap<usize, u64> = self
            .symbols
            .iter()
            .rev() // the first symbol at a byte wins
            .map(|s| (s.start_byte, s.id))
            .collect();
        let symbols = &self.symbols;
        let source_of = |e: &GraphEdge| {
            e.source_byte
                .and_then(|b| by_start.get(&b).copied())
                .or_else(|| innermost_around(symbols, e.byte, e.byte, None).map(|j| symbols[j].id))
                .unwrap_or(file_id)
        };
        for e in self.edges.iter_mut().chain(self.module_edges.iter_mut()) {
            e.src_id = source_of(e);
        }
    }
}

/// The parent of `symbols[i]`, if any symbol of the file can be it.
fn parent_of(symbols: &[Symbol], i: usize) -> Option<u64> {
    let s = &symbols[i];
    if let Some(j) = innermost_around(symbols, s.start_byte, s.end_byte, Some(i)) {
        return Some(symbols[j].id);
    }
    // A container that is not a symbol of its own (a Rust `impl Point`):
    // the type it names, when this file defines it.
    let qualifier = s.qualified.rsplit_once('.').map(|(q, _)| q)?;
    symbols
        .iter()
        .find(|t| kind_class(&t.kind) == "type" && t.qualified == qualifier)
        .map(|t| t.id)
}

/// The index of the smallest symbol whose span covers `[from, to]`, other
/// than `skip`. Of two with the same span the earlier is the container of
/// the later, never the reverse, so containment has no cycle.
fn innermost_around(
    symbols: &[Symbol],
    from: usize,
    to: usize,
    skip: Option<usize>,
) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (j, t) in symbols.iter().enumerate() {
        if Some(j) == skip || t.start_byte > from || t.end_byte < to {
            continue;
        }
        if let Some(i) = skip {
            let s = &symbols[i];
            let same_span = t.start_byte == s.start_byte && t.end_byte == s.end_byte;
            if same_span && j > i {
                continue;
            }
        }
        let size = t.end_byte - t.start_byte;
        if best.is_none_or(|b| size < symbols[b].end_byte - symbols[b].start_byte) {
            best = Some(j);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use crate::{parse, Lang, ParsedFile};

    fn ids(lang: Lang, src: &str, file: &str) -> ParsedFile {
        let mut pf = parse(lang, src).unwrap();
        pf.assign_ids("repo", file);
        pf
    }

    fn id_of(pf: &ParsedFile, qualified: &str) -> u64 {
        pf.symbols
            .iter()
            .find(|s| s.qualified == qualified)
            .unwrap_or_else(|| panic!("{qualified} not in {:?}", pf.symbols))
            .id
    }

    /// The id is of the symbol, not of where its body sits: moving a function
    /// within its file, or pushing it down with new code above, keeps it.
    #[test]
    fn moving_a_body_within_the_file_keeps_the_id() {
        let before = "\
struct Point;
impl Point {
    fn mag(&self) -> i32 { 1 }
    fn norm(&self) -> i32 { 2 }
}
fn free() {}
";
        let after = "\
// a new header comment

fn free() {}

struct Point;
impl Point {
    fn norm(&self) -> i32 {
        2
    }

    fn mag(&self) -> i32 { 1 }
}
";
        let a = ids(Lang::rust(), before, "src/geo.rs");
        let b = ids(Lang::rust(), after, "src/geo.rs");
        for q in ["Point", "Point.mag", "Point.norm", "free"] {
            assert_eq!(id_of(&a, q), id_of(&b, q), "{q} moved and changed id");
        }
        assert_eq!(a.file_symbol.id, b.file_symbol.id);
        // The parse is branch-blind: the same file on any branch has these ids.
        assert_eq!(
            id_of(&a, "Point.mag"),
            crate::symbol_id::symbol_id("repo", "src/geo.rs", "callable", "Point.mag", "")
        );
    }

    /// Overloads are different symbols to whoever asks "who calls this".
    #[test]
    fn java_overloads_get_distinct_ids() {
        let src = "\
public class OfficeService {
    public void actualizar(Long id, String user) {}
    public void actualizar(Long id, OfficeRequest request, String user) {}
    public void actualizar(String code) {}
}
";
        let pf = ids(Lang::java(), src, "OfficeService.java");
        let overloads: Vec<_> = pf
            .symbols
            .iter()
            .filter(|s| s.name == "actualizar")
            .collect();
        assert_eq!(overloads.len(), 3);
        assert_eq!(overloads[0].params.as_deref(), Some("Long,String"));
        assert_eq!(
            overloads[1].params.as_deref(),
            Some("Long,OfficeRequest,String")
        );
        let distinct: std::collections::HashSet<u64> = overloads.iter().map(|s| s.id).collect();
        assert_eq!(distinct.len(), 3, "{overloads:?}");
        // Reordering the overloads keeps each one's id: the types decide, not
        // the position.
        let swapped = "\
public class OfficeService {
    public void actualizar(String code) {}
    public void actualizar(Long id, OfficeRequest request, String user) {}
    public void actualizar(Long id, String user) {}
}
";
        let again = ids(Lang::java(), swapped, "OfficeService.java");
        let by_params = |pf: &ParsedFile, p: &str| {
            pf.symbols
                .iter()
                .find(|s| s.params.as_deref() == Some(p))
                .unwrap()
                .id
        };
        for p in ["Long,String", "Long,OfficeRequest,String", "String"] {
            assert_eq!(by_params(&pf, p), by_params(&again, p), "{p}");
        }
    }

    /// Same name, same container, nothing to tell them apart (Python
    /// redefinition): the ordinal does, so no two rows share an id.
    #[test]
    fn homonyms_without_types_take_an_ordinal() {
        let src = "def f():\n    pass\n\ndef f():\n    pass\n";
        let pf = ids(Lang::python(), src, "m.py");
        assert_eq!(pf.symbols.len(), 2);
        assert_ne!(pf.symbols[0].id, pf.symbols[1].id);
        assert_ne!(pf.symbols[0].id, pf.file_symbol.id);
    }

    #[test]
    fn parents_and_edge_sources_point_at_symbols_of_the_file() {
        let src = "\
class A:
    def run(self):
        self.helper()

    def helper(self):
        pass

setup()
";
        let pf = ids(Lang::python(), src, "pkg/a.py");
        assert_eq!(pf.file_symbol.qualified, "pkg/a.py");
        assert_eq!(pf.file_symbol.name, "a.py");
        let class = id_of(&pf, "A");
        let run = id_of(&pf, "A.run");
        assert_eq!(pf.symbols[0].parent_id, Some(pf.file_symbol.id));
        assert!(pf.symbols[1..].iter().all(|s| s.parent_id == Some(class)));
        let call = pf.edges.iter().find(|e| e.target == "A.helper").unwrap();
        assert_eq!(call.src_id, run);
        let module = pf
            .module_edges
            .iter()
            .find(|e| e.target == "setup")
            .unwrap();
        assert_eq!(module.src_id, pf.file_symbol.id);
    }

    /// A Rust `impl` is a container but not a symbol: its methods hang from
    /// the type it names when the file defines it.
    #[test]
    fn rust_impl_methods_hang_from_their_type() {
        let src = "struct Point;\nimpl Point {\n    fn mag(&self) {}\n}\n";
        let pf = ids(Lang::rust(), src, "p.rs");
        assert_eq!(
            pf.symbols
                .iter()
                .find(|s| s.name == "mag")
                .unwrap()
                .parent_id,
            Some(id_of(&pf, "Point"))
        );
    }

    /// A nested class qualifies its members with the whole chain, so two
    /// inner classes with a member of the same name do not collide.
    #[test]
    fn nested_containers_qualify_with_the_whole_chain() {
        let src = "\
public class Outer {
    static class A { void find() {} }
    static class B { void find() {} }
}
";
        let pf = ids(Lang::java(), src, "Outer.java");
        let a = id_of(&pf, "Outer.A.find");
        let b = id_of(&pf, "Outer.B.find");
        assert_ne!(a, b);
        let find_a = pf.symbols.iter().find(|s| s.id == a).unwrap();
        assert_eq!(find_a.parent.as_deref(), Some("A"));
        assert_eq!(find_a.parent_id, Some(id_of(&pf, "Outer.A")));
        assert_eq!(find_a.signature, "void find() {}");
    }
}
