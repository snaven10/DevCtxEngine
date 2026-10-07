//! Assigning stable ids to a parsed file's symbols and edges (PLAN-009 DD-3).
//!
//! The hash itself lives in [`devctx_core::symbol_id`], shared with the store.
//! What lives here is the part that needs the parse: which disambiguator each
//! symbol gets, which symbol contains which, and which symbol each call comes
//! from.

use std::collections::{HashMap, HashSet};

pub use devctx_core::symbol_id::{file_symbol_id, kind_class, sym_hex, symbol_id, FILE_KIND};

use crate::types::{GraphEdge, ParsedFile, Symbol};

impl ParsedFile {
    /// Give every symbol its id and parent id, name the file symbol after
    /// `file`, and point every edge at its source symbol.
    ///
    /// - **Qualified name** (set by the parser): every enclosing scope, so
    ///   homonyms differ by where they are — `tests.helper`, `deco.wrapper`,
    ///   `Outer.start.run`, Go `T.String`.
    /// - **Disambiguator:** the trait of a Rust `impl Trait for` (so
    ///   `Display`'s and `Debug`'s `X.fmt` differ), and the normalised
    ///   parameter types where the language overloads with separate bodies
    ///   (Java: `run(Long)` and `run(String)`), always, not only when an
    ///   overload exists — adding one must not move the other's id; and,
    ///   when an enclosing scope is a callable, the scope shape (`@sf` for
    ///   `Outer.start.run`), so `fn a() { struct P }` and `mod a { struct P }`
    ///   differ; and for a Rust `impl` and its members, the self type's
    ///   generic arguments and `where` clause (`~<u8>`), so `impl Foo<u8>`
    ///   and `impl Foo<u16>` differ. Else empty. Only two symbols still sharing kind class, qualified name and
    ///   disambiguator (a Python redefinition, two anonymous classes in one
    ///   method) take an ordinal in source order (`#1`, `#2`…), as does a
    ///   hash collision: the last resort, and the one case where inserting a
    ///   homonym above moves an id.
    /// - **Parent:** the innermost symbol whose span contains this one (a
    ///   method's `impl`); for a Rust `impl`, the type of that name beside
    ///   it; else, for a symbol whose container is not itself a symbol (a Go
    ///   method's receiver type), the type of that name in this file; else
    ///   the file symbol.
    /// - **Edge source:** the symbol defined by the enclosing function node;
    ///   else the innermost symbol around the call; else the file symbol.
    ///   The same for instantiations and type uses; a supertype's is the
    ///   innermost symbol around it (the class, `impl` or trait naming it).
    /// - **Package:** for a language that derives it from the path (Python,
    ///   TS/JS, Rust), the module path of `file`.
    pub fn assign_ids(&mut self, repo: &str, file: &str) {
        if self.facts.package.is_none() {
            if let Some(lang) = crate::Lang::named(&self.language) {
                self.facts.package = crate::resolve::resolver_for(lang).package_from_path(file);
            }
        }
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
            let mut base = match (&sym.trait_of, &sym.params) {
                (Some(t), Some(p)) => format!("{t}|{p}"),
                (Some(t), None) => t.clone(),
                (None, Some(p)) => p.clone(),
                (None, None) => String::new(),
            };
            if let Some(args) = &sym.impl_args {
                base.push('~');
                base.push_str(args);
            }
            if let Some(shape) = &sym.scope_shape {
                base.push('@');
                base.push_str(shape);
            }
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
        for r in &mut self.facts.refs {
            r.src_id = r
                .source_byte
                .and_then(|b| by_start.get(&b).copied())
                .or_else(|| innermost_around(symbols, r.byte, r.byte, None).map(|j| symbols[j].id))
                .unwrap_or(file_id);
        }
        for f in &mut self.facts.inherits {
            f.src_id = innermost_around(symbols, f.byte, f.byte, None)
                .map(|j| symbols[j].id)
                .unwrap_or(file_id);
        }
    }
}

/// The parent of `symbols[i]`, if any symbol of the file can be it.
fn parent_of(symbols: &[Symbol], i: usize) -> Option<u64> {
    let s = &symbols[i];
    // An `impl` belongs to the type it implements, when the file defines
    // it beside the `impl` (same scope, same qualified name).
    if s.kind == "impl" {
        if let Some(t) = symbols
            .iter()
            .find(|t| kind_class(&t.kind) == "type" && t.qualified == s.qualified)
        {
            return Some(t.id);
        }
    }
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
/// than `skip` and than any symbol with `skip`'s span (the names of one
/// declaration that has no node per name, Go `var a, b int`; TS/JS and
/// Java declarators each span their own, see `own_declarator`), so
/// containment has no cycle. Of two candidates with one span the earlier
/// wins: an occurrence inside a Go `var a, b T` is `a`'s.
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
        // Two symbols with one span are names of one declaration (Go `var
        // a, b int`): siblings, never one the container of the other.
        if let Some(i) = skip {
            let s = &symbols[i];
            if t.start_byte == s.start_byte && t.end_byte == s.end_byte {
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
        let imp = pf.symbols.iter().find(|s| s.kind == "impl").unwrap();
        let mag = pf.symbols.iter().find(|s| s.name == "mag").unwrap();
        // The method hangs from its `impl` (the container that holds it),
        // the `impl` from the type it implements.
        assert_eq!(mag.parent_id, Some(imp.id));
        assert_eq!(imp.parent_id, Some(id_of(&pf, "Point")));
        assert_eq!(
            (imp.name.as_str(), imp.qualified.as_str()),
            ("Point", "Point")
        );
        assert_ne!(imp.id, id_of(&pf, "Point"), "an impl is not its type");
        assert_eq!(mag.parent.as_deref(), Some("Point"));
    }

    /// The id of the symbol whose source text contains `marker`.
    fn id_at(pf: &ParsedFile, src: &str, marker: &str) -> u64 {
        pf.symbols
            .iter()
            .filter(|s| src[s.start_byte..s.end_byte].contains(marker))
            .min_by_key(|s| s.end_byte - s.start_byte)
            .unwrap_or_else(|| panic!("no symbol around {marker}: {:?}", pf.symbols))
            .id
    }

    /// Inserting a homonym *above* an existing symbol must not move that
    /// symbol's id: an ordinal in source order would hand the old id to the
    /// newcomer, and a stored id would silently point at another function.
    /// The qualified name carries every enclosing symbol (module, function,
    /// class) and a Rust trait impl carries its trait, so the ordinal is
    /// left for true redefinitions only.
    #[test]
    fn inserting_a_homonym_above_keeps_existing_ids() {
        let cases: &[(Lang, &str, &str, &str, &[&str])] = &[
            (
                Lang::rust(),
                "src/x.rs",
                "\
fn helper() { m_top(); }
struct X;
impl Display for X { fn fmt(&self) { m_display(); } }
mod tests { fn helper() { m_tests(); } }
",
                "\
mod other { fn helper() { n1(); } }
impl Debug for X { fn fmt(&self) { n2(); } }
fn helper() { m_top(); }
struct X;
impl Display for X { fn fmt(&self) { m_display(); } }
mod tests { fn helper() { m_tests(); } }
",
                &["m_top", "m_display", "m_tests"],
            ),
            (
                Lang::python(),
                "deco.py",
                "\
def deco_a(f):
    def wrapper():
        m_a()
    return wrapper

def deco_b(f):
    def wrapper():
        m_b()
    return wrapper
",
                "\
def deco_z(f):
    def wrapper():
        n1()
    return wrapper

def deco_a(f):
    def wrapper():
        m_a()
    return wrapper

def deco_b(f):
    def wrapper():
        m_b()
    return wrapper
",
                &["m_a", "m_b"],
            ),
            (
                Lang::go(),
                "t.go",
                "package p\nfunc (a A) String() string { return m_a() }\n",
                "package p\nfunc (b *B) String() string { return n1() }\n\
                 func (a A) String() string { return m_a() }\n",
                &["m_a"],
            ),
            (
                Lang::java(),
                "Outer.java",
                "\
class Outer {
    void start() { new Runnable() { public void run() { m_start(); } }; }
}
",
                "\
class Outer {
    void stop() { new Runnable() { public void run() { n1(); } }; }
    void start() { new Runnable() { public void run() { m_start(); } }; }
}
",
                &["m_start"],
            ),
            // A Java constructor is a scope: its anonymous class's method is
            // not the class's own method of the same name.
            (
                Lang::java(),
                "O.java",
                "class O {\n    O() { new Runnable() { public void run() { m_ctor(); } }; }\n}\n",
                "class O {\n    void run() { n1(); }\n    \
                 O() { new Runnable() { public void run() { m_ctor(); } }; }\n}\n",
                &["m_ctor"],
            ),
            // Object literals are scopes through the variable (or property)
            // that holds them.
            (
                Lang::typescript(),
                "o.ts",
                "const b = { run() { m_b(); } };\nconst c = { k: { run() { m_c(); } } };\n",
                "const a = { run() { n1(); } };\nconst x = { k: { run() { n2(); } } };\n\
                 const b = { run() { m_b(); } };\nconst c = { k: { run() { m_c(); } } };\n",
                &["m_b", "m_c"],
            ),
            (
                Lang::javascript(),
                "o.js",
                "const b = { run() { m_b(); } };\n",
                "const a = { run() { n1(); } };\nconst b = { run() { m_b(); } };\n",
                &["m_b"],
            ),
            // Java enum constants with bodies, and anonymous classes held by
            // fields.
            (
                Lang::java(),
                "Op.java",
                "\
enum Op {
    MINUS { int apply() { return m_minus(); } };
    abstract int apply();
}
class C {
    Runnable b = new Runnable() { public void run() { m_b(); } };
}
",
                "\
enum Op {
    PLUS { int apply() { return n1(); } },
    MINUS { int apply() { return m_minus(); } };
    abstract int apply();
}
class C {
    Runnable a = new Runnable() { public void run() { n2(); } };
    Runnable b = new Runnable() { public void run() { m_b(); } };
}
",
                &["m_minus", "m_b"],
            ),
            // A function and a module of the same name (Rust keeps them in
            // different namespaces) both qualify their contents as `a.P`; the
            // disambiguator tells a callable scope apart.
            (
                Lang::rust(),
                "j.rs",
                "mod a { struct P(u8, M_mod); }\n",
                "fn a() { struct P(u8, N1); }\nmod a { struct P(u8, M_mod); }\n",
                &["M_mod"],
            ),
            // Two inherent impls of one generic type differ by its arguments:
            // inserting `impl Foo<u8>` above `impl Foo<u16>` moves neither the
            // impl nor its methods (nor their parent ids).
            (
                Lang::rust(),
                "g.rs",
                "struct Foo<T>(T);\nimpl Foo<u16> { fn get(&self) { m_u16(); } }\n",
                "struct Foo<T>(T);\nimpl Foo<u8> { fn get(&self) { n1(); } }\n\
                 impl Foo<u16> { fn get(&self) { m_u16(); } }\n",
                &["m_u16"],
            ),
            (
                Lang::rust(),
                "w.rs",
                "struct W<T>(T);\nimpl<T> W<T> where T: Copy { fn get(&self) { m_copy(); } }\n",
                "struct W<T>(T);\nimpl<T> W<T> where T: Clone { fn get(&self) { n1(); } }\n\
                 impl<T> W<T> where T: Copy { fn get(&self) { m_copy(); } }\n",
                &["m_copy"],
            ),
            // `rustfmt` writes a long `where` clause vertically, one bound a
            // line with a trailing comma, and a long argument list with one
            // after the last argument: neither is another impl.
            (
                Lang::rust(),
                "v.rs",
                "struct W<T>(T);\nimpl<T> W<T> where T: Copy { fn get(&self) { m_vert(); } }\n",
                "struct W<T>(T);\nimpl<T> W<T> where T: Clone { fn get(&self) { n1(); } }\n\
                 impl<T> W<T>\nwhere\n    T: Copy,\n{\n    fn get(&self) {\n        m_vert();\n    }\n}\n",
                &["m_vert"],
            ),
            (
                Lang::rust(),
                "t.rs",
                "struct Foo<T>(T);\nimpl<T> Foo<T> { fn get(&self) { m_trail(); } }\n",
                "struct Foo<T>(T);\nimpl Foo<u8> { fn get(&self) { n1(); } }\n\
                 impl<T> Foo<\n    T,\n> {\n    fn get(&self) {\n        m_trail();\n    }\n}\n",
                &["m_trail"],
            ),
            // Java overloads around an anonymous class: `O()` and `O(int)`,
            // `m(int)` and `m(String)` are different scopes, so adding an
            // overload above does not hand the old `run` an ordinal.
            (
                Lang::java(),
                "L.java",
                "\
class O {
    O() { new Runnable() { public void run() { m_ctor(); } }; }
    void m(String s) { new Runnable() { public void run() { m_str(); } }; }
}
",
                "\
class O {
    O(int x) { new Runnable() { public void run() { n1(); } }; }
    O() { new Runnable() { public void run() { m_ctor(); } }; }
    void m(int i) { new Runnable() { public void run() { n2(); } }; }
    void m(String s) { new Runnable() { public void run() { m_str(); } }; }
}
",
                &["m_ctor", "m_str"],
            ),
        ];
        for (lang, file, before, after, markers) in cases {
            let a = ids(*lang, before, file);
            let b = ids(*lang, after, file);
            for m in *markers {
                let parent = |pf: &ParsedFile, src: &str| {
                    let id = id_at(pf, src, m);
                    pf.symbols.iter().find(|s| s.id == id).unwrap().parent_id
                };
                assert_eq!(
                    parent(&a, before),
                    parent(&b, after),
                    "{}: the parent of the symbol around {m} changed id",
                    lang.name()
                );
                assert_eq!(
                    id_at(&a, before, m),
                    id_at(&b, after, m),
                    "{}: the symbol around {m} changed id\nbefore {:#?}\nafter {:#?}",
                    lang.name(),
                    a.symbols,
                    b.symbols
                );
            }
        }
    }

    /// What the qualified name and the disambiguator say, per language.
    #[test]
    fn qualified_names_carry_every_enclosing_symbol() {
        // `impl` symbols aside: they carry their type's qualified name.
        let q = |lang: Lang, src: &str| -> Vec<(String, Option<String>)> {
            ids(lang, src, "f")
                .symbols
                .iter()
                .filter(|s| s.kind != "impl")
                .map(|s| (s.qualified.clone(), s.trait_of.clone()))
                .collect()
        };
        let rust = q(
            Lang::rust(),
            "mod a { struct P<T>(T); impl<T> P<T> { fn get(&self) { fn inner() {} } } \
             impl<T> From<T> for P<T> { fn from(t: T) -> Self { P(t) } } }",
        );
        let names: Vec<&str> = rust.iter().map(|(q, _)| q.as_str()).collect();
        assert_eq!(
            names,
            ["a", "a.P", "a.P.get", "a.P.get.inner", "a.P.from"],
            "{rust:?}"
        );
        assert_eq!(rust[4].1.as_deref(), Some("From<T>"));
        assert_eq!(rust[2].1, None);
        let py = q(
            Lang::python(),
            "def deco(f):\n    def wrapper():\n        pass\n",
        );
        assert_eq!(py[1].0, "deco.wrapper");
        let ts = q(
            Lang::typescript(),
            "namespace N { export function f() {} }\nfunction g() { function h() {} }\n",
        );
        let names: Vec<&str> = ts.iter().map(|(q, _)| q.as_str()).collect();
        assert_eq!(names, ["N.f", "g", "g.h"], "{ts:?}");

        // The type of an `impl` is reduced to the name its definition has:
        // no lifetime, reference, `mut` or path; a type with no name of its
        // own (a slice) keeps its text. The trait loses its path, not its
        // generic arguments.
        let src = "\
struct Foo;
impl<'a> Display for &'a Foo { fn fmt(&self) {} }
impl<'a> Debug for &'a mut Foo { fn fmt(&self) {} }
impl<T> fmt::Display for [T] { fn fmt(&self) {} }
impl crate::x::Foo { fn a(&self) {} }
impl super::Foo { fn b(&self) {} }
impl<T> std::convert::From<T> for Foo { fn from(t: T) -> Self { Foo } }
";
        let pf = ids(Lang::rust(), src, "f");
        let got: Vec<(&str, Option<&str>)> = pf
            .symbols
            .iter()
            .filter(|s| s.kind != "impl")
            .map(|s| (s.qualified.as_str(), s.trait_of.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("Foo", None),
                ("Foo.fmt", Some("Display")),
                ("Foo.fmt", Some("Debug")),
                ("[T].fmt", Some("Display")),
                ("Foo.a", None),
                ("Foo.b", None),
                ("Foo.from", Some("From<T>")),
            ],
        );
        let foo = id_of(&pf, "Foo");
        for s in &pf.symbols[1..] {
            if s.kind == "impl" {
                // The impl hangs from its type when the file defines it.
                let want = if s.qualified == "Foo" {
                    foo
                } else {
                    pf.file_symbol.id
                };
                assert_eq!(s.parent_id, Some(want), "{s:?}");
                continue;
            }
            // A method hangs from its impl.
            let imp = pf
                .symbols
                .iter()
                .rfind(|t| {
                    t.kind == "impl" && t.start_byte < s.start_byte && t.end_byte >= s.end_byte
                })
                .unwrap();
            assert_eq!(s.parent_id, Some(imp.id), "{s:?}");
            assert_eq!(
                s.parent.as_deref(),
                Some(&s.qualified[..s.qualified.len() - s.name.len() - 1])
            );
        }
    }

    /// Java parameter types are reduced to their simple name without
    /// generic arguments: overloads cannot differ by them (erasure), and a
    /// fully qualified spelling is the same type.
    #[test]
    fn java_parameter_types_are_simple_names() {
        let src = "\
class S {
    void f(List<String> a, java.util.Map<String, List<Long>> b, String[] c, int... d) {}
}
";
        let pf = ids(Lang::java(), src, "S.java");
        let f = pf.symbols.iter().find(|s| s.name == "f").unwrap();
        assert_eq!(f.params.as_deref(), Some("List,Map,String[],int..."));
    }

    /// A generic impl names its type without the parameters, so its methods
    /// hang from the struct the file defines (`impl<T> Foo<T>` → `Foo`).
    #[test]
    fn a_generic_impl_hangs_from_its_type() {
        let src = "struct Foo<T>(T);\nimpl<T> Foo<T> {\n    fn get(&self) {}\n}\n";
        let pf = ids(Lang::rust(), src, "g.rs");
        let get = pf.symbols.iter().find(|s| s.name == "get").unwrap();
        assert_eq!(get.qualified, "Foo.get");
        assert_eq!(get.parent.as_deref(), Some("Foo"));
        let imp = pf.symbols.iter().find(|s| s.kind == "impl").unwrap();
        assert_eq!((imp.name.as_str(), imp.qualified.as_str()), ("Foo", "Foo"));
        assert_eq!(get.parent_id, Some(imp.id));
        assert_eq!(imp.parent_id, Some(id_of(&pf, "Foo")));
    }

    /// TypeScript overload signatures are not symbols, so parameter types
    /// would only make the id fragile to a type refactor: TS ids ignore them.
    /// Java keeps them (its overloads are separate bodies).
    #[test]
    fn typescript_ids_ignore_parameter_types() {
        let a = ids(Lang::typescript(), "function f(x: number) {}\n", "f.ts");
        let b = ids(Lang::typescript(), "function f(x: string) {}\n", "f.ts");
        assert_eq!(a.symbols[0].id, b.symbols[0].id);
        assert_eq!(a.symbols[0].params, None);
    }

    /// The signature stops at the body, also when the body is on its line.
    #[test]
    fn the_signature_excludes_a_one_line_body() {
        let pf = ids(Lang::python(), "def f(x): return x\n", "s.py");
        assert_eq!(pf.symbols[0].signature, "def f(x):");
        let pf = ids(Lang::rust(), "fn g() -> i32 { 1 }\nstruct S;\n", "s.rs");
        assert_eq!(pf.symbols[0].signature, "fn g() -> i32");
        assert_eq!(pf.symbols[1].signature, "struct S;");
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
        assert_eq!(find_a.signature, "void find()");
    }

    /// Several declarators in one declaration (`const a = …, b = …`, Java
    /// `int a, b;`, Go `var a, b int`) are siblings: none is the parent of
    /// another, and each one's calls and type uses are its own.
    #[test]
    fn declarators_of_one_declaration_are_siblings() {
        let src = "const a = () => { x(); }, b = () => { y(); };\n";
        let pf = ids(Lang::typescript(), src, "d.ts");
        let file = pf.file_symbol.id;
        for q in ["a", "b"] {
            let s = pf.symbols.iter().find(|s| s.qualified == q).unwrap();
            assert_eq!(s.parent_id, Some(file), "{q}: {:?}", pf.symbols);
        }
        let names: std::collections::HashMap<u64, &str> = pf
            .symbols
            .iter()
            .map(|s| (s.id, s.qualified.as_str()))
            .collect();
        let calls: Vec<(&str, &str)> = pf
            .edges
            .iter()
            .map(|e| (names[&e.src_id], e.target.as_str()))
            .collect();
        assert_eq!(calls, [("a", "x"), ("b", "y")]);
        let graph: Vec<(&str, &str)> = pf
            .edges
            .iter()
            .map(|e| (e.source.as_str(), e.target.as_str()))
            .collect();
        assert_eq!(graph, [("a", "x"), ("b", "y")]);

        let pf = ids(Lang::java(), "class C { int a, b; Foo f, g; }\n", "C.java");
        let c = id_of(&pf, "C");
        for q in ["C.a", "C.b", "C.f", "C.g"] {
            let s = pf.symbols.iter().find(|s| s.qualified == q).unwrap();
            assert_eq!(s.parent_id, Some(c), "{q}: {:?}", pf.symbols);
        }

        let pf = ids(Lang::go(), "package p\nvar a, b int\n", "v.go");
        for q in ["a", "b"] {
            let s = pf.symbols.iter().find(|s| s.qualified == q).unwrap();
            assert_eq!(
                s.parent_id,
                Some(pf.file_symbol.id),
                "{q}: {:?}",
                pf.symbols
            );
        }
    }

    /// `exported` means "visible outside the module": a Python function
    /// nested in another is not, whatever its name.
    #[test]
    fn a_nested_python_function_is_not_exported() {
        let pf = ids(
            Lang::python(),
            "def deco(f):\n    def wrapper():\n        pass\n    return wrapper\n\nclass K:\n    def m(self):\n        pass\n",
            "d.py",
        );
        let exported = |q: &str| {
            pf.symbols
                .iter()
                .find(|s| s.qualified == q)
                .unwrap()
                .exported
        };
        assert_eq!(exported("deco"), Some(true));
        assert_eq!(exported("deco.wrapper"), Some(false));
        assert_eq!(exported("K.m"), Some(true));
    }
}
