//! `devctx-parse` — source parsing for DevCtxEngine.
//!
//! tree-sitter symbol + import extraction for Python, JavaScript, TypeScript/TSX,
//! Go, Java and Rust, plus call-graph edges with FQN receiver resolution
//! (self/type/field-type maps) and framework HTTP route extraction. Extension-based
//! language detection covers parseable + raw-text files. See
//! `docs/architecture-spec.md` §3.

pub mod error;
pub mod lang;
pub mod parser;
pub mod registry;
pub mod routes;
pub mod types;

pub use error::{ParseError, Result};
pub use lang::{lang_for_extension, raw_text_language, Lang};
pub use parser::LanguageParser;
pub use registry::LangDef;
pub use routes::{extract_routes, Route};
pub use types::{GraphEdge, Import, ParsedFile, Symbol};

use std::path::Path;

/// Detect a parseable language from a file path's extension.
pub fn detect_lang(path: &Path) -> Option<Lang> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    lang_for_extension(&ext)
}

/// Parse `source` as `lang`, extracting symbols and imports.
pub fn parse(lang: Lang, source: &str) -> Result<ParsedFile> {
    LanguageParser::new(lang)?.parse(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(lang: Lang, src: &str) -> ParsedFile {
        parse(lang, src).unwrap()
    }

    fn find<'a>(pf: &'a ParsedFile, name: &str) -> &'a Symbol {
        pf.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("symbol {name} not found in {:?}", pf.symbols))
    }

    #[test]
    fn all_queries_compile() {
        for l in lang::all() {
            assert!(LanguageParser::new(l).is_ok(), "parser build failed: {l:?}");
        }
    }

    #[test]
    fn python_symbols_and_imports() {
        let src = "\
import os
from sys import path

def top_level():
    return 1

class Greeter:
    def greet(self, name):
        return name
";
        let pf = parse_ok(Lang::python(), src);
        assert_eq!(pf.language, "python");
        assert_eq!(find(&pf, "top_level").kind, "function");
        assert_eq!(find(&pf, "Greeter").kind, "class");
        let greet = find(&pf, "greet");
        assert_eq!(greet.kind, "method");
        assert_eq!(greet.parent.as_deref(), Some("Greeter"));
        assert_eq!(pf.imports.len(), 2);
        assert_eq!(pf.imports[0].line, 1);
    }

    #[test]
    fn rust_symbols_and_methods() {
        let src = "\
use std::fmt;

pub struct Point { x: i32 }

pub fn free() {}

impl Point {
    fn mag(&self) -> i32 { self.x }
}

trait Shape {}
enum Color { Red }
";
        let pf = parse_ok(Lang::rust(), src);
        assert_eq!(find(&pf, "Point").kind, "struct");
        assert_eq!(find(&pf, "free").kind, "function");
        let mag = find(&pf, "mag");
        assert_eq!(mag.kind, "method");
        assert_eq!(mag.parent.as_deref(), Some("Point"));
        assert_eq!(find(&pf, "Shape").kind, "trait");
        assert_eq!(find(&pf, "Color").kind, "enum");
        assert_eq!(pf.imports.len(), 1);
    }

    #[test]
    fn go_symbols() {
        let src = "\
package main

import \"fmt\"

type Server struct{}

func (s *Server) Handle() {}

func main() {}
";
        let pf = parse_ok(Lang::go(), src);
        assert_eq!(find(&pf, "Server").kind, "type");
        assert_eq!(find(&pf, "Handle").kind, "method");
        assert_eq!(find(&pf, "main").kind, "function");
        assert_eq!(pf.imports.len(), 1);
    }

    #[test]
    fn java_symbols() {
        let src = "\
package demo;
import java.util.List;

public class Service {
    public void run() {}
}

interface Runnable2 {}
";
        let pf = parse_ok(Lang::java(), src);
        assert_eq!(find(&pf, "Service").kind, "class");
        let run = find(&pf, "run");
        assert_eq!(run.kind, "method");
        assert_eq!(run.parent.as_deref(), Some("Service"));
        assert_eq!(find(&pf, "Runnable2").kind, "interface");
        assert_eq!(pf.imports.len(), 1);
    }

    #[test]
    fn typescript_symbols() {
        let src = "\
import { A } from './a';

export function build(): void {}

export class Widget {
    render() {}
}

interface Props {}
";
        let pf = parse_ok(Lang::typescript(), src);
        assert_eq!(find(&pf, "build").kind, "function");
        assert_eq!(find(&pf, "Widget").kind, "class");
        assert_eq!(find(&pf, "render").kind, "method");
        assert_eq!(find(&pf, "Props").kind, "interface");
        assert_eq!(pf.imports.len(), 1);
    }

    #[test]
    fn javascript_symbols() {
        let src = "\
import x from 'x';
function go() {}
class Box { open() {} }
";
        let pf = parse_ok(Lang::javascript(), src);
        assert_eq!(find(&pf, "go").kind, "function");
        assert_eq!(find(&pf, "Box").kind, "class");
        assert_eq!(find(&pf, "open").kind, "method");
    }

    #[test]
    fn extracts_rust_call_edges() {
        let src = "\
fn helper() -> i32 { 1 }
fn caller() -> i32 { helper() + helper() }
";
        let pf = parse_ok(Lang::rust(), src);
        let calls: Vec<_> = pf
            .edges
            .iter()
            .filter(|e| e.source == "caller" && e.target == "helper")
            .collect();
        assert_eq!(calls.len(), 2, "edges: {:?}", pf.edges);
        assert!(pf.edges.iter().all(|e| e.kind == "calls"));
    }

    #[test]
    fn resolves_python_self_call_to_class() {
        let src = "\
class A:
    def run(self):
        self.helper()

    def helper(self):
        pass
";
        let pf = parse_ok(Lang::python(), src);
        assert!(
            pf.edges
                .iter()
                .any(|e| e.source == "A.run" && e.target == "A.helper"),
            "edges: {:?}",
            pf.edges
        );
    }

    #[test]
    fn resolves_type_receiver_call() {
        let src = "\
def configure():
    Logger.getLogger()
";
        let pf = parse_ok(Lang::python(), src);
        assert!(
            pf.edges
                .iter()
                .any(|e| e.source == "configure" && e.target == "Logger.getLogger"),
            "edges: {:?}",
            pf.edges
        );
    }

    #[test]
    fn resolves_rust_self_call_to_impl_type() {
        let src = "\
struct Point;
impl Point {
    fn mag(&self) { self.compute(); }
    fn compute(&self) {}
}
";
        let pf = parse_ok(Lang::rust(), src);
        assert!(
            pf.edges
                .iter()
                .any(|e| e.source == "Point.mag" && e.target == "Point.compute"),
            "edges: {:?}",
            pf.edges
        );
    }

    fn has_edge(pf: &ParsedFile, source: &str, target: &str) -> bool {
        pf.edges
            .iter()
            .any(|e| e.source == source && e.target == target)
    }

    #[test]
    fn resolves_java_field_receiver_type() {
        let src = "\
public class Svc {
    private UserRepository repo;
    public void run() {
        repo.findById();
    }
}
";
        let pf = parse_ok(Lang::java(), src);
        assert!(
            has_edge(&pf, "Svc.run", "UserRepository.findById"),
            "edges: {:?}",
            pf.edges
        );
    }

    /// A fluent chain is the norm in reactive Java, and its receiver node is the
    /// whole expression before the dot — newlines, arguments and lambdas
    /// included. Measured on a Quarkus repository, that put call-graph nodes
    /// named `Oficina.findByCodigo(codigo).flatMap` and a three-line expression
    /// with a lambda inside it into the graph. A receiver nobody can name is
    /// not a qualifier: the honest answer is the bare callee.
    #[test]
    fn a_fluent_chain_receiver_does_not_qualify_the_target() {
        let src = "\
public class Svc {
    public void run() {
        Oficina.findByCodigo(codigo).flatMap(o -> o.persist());
    }
}
";
        let pf = parse_ok(Lang::java(), src);
        assert!(
            has_edge(&pf, "Svc.run", "flatMap"),
            "the chained call should fall back to the bare name; edges: {:?}",
            pf.edges
        );
        assert!(
            pf.edges
                .iter()
                .all(|e| !e.target.contains('(') && !e.target.contains('\n')),
            "a target carries an expression rather than a name: {:?}",
            pf.edges
        );
    }

    /// The multi-line case, which is what the graph actually filled up with.
    #[test]
    fn a_multiline_receiver_never_reaches_a_target_name() {
        let src = "\
public class Svc {
    public void run() {
        Oficina
            .persist(oficina).replaceWith(
                () -> OficinaDTO.from(oficina)).invoke(x);
    }
}
";
        let pf = parse_ok(Lang::java(), src);
        for e in &pf.edges {
            assert!(
                !e.target.contains('\n') && !e.target.contains(' '),
                "target is an expression, not a name: {:?}",
                e
            );
        }
    }

    /// The receivers that DO name something have to keep qualifying, or this
    /// change would trade one wrong answer for a less useful one.
    #[test]
    fn a_nameable_receiver_still_qualifies() {
        let src = "\
public class Svc {
    private UserRepository repo;
    public void run() {
        this.repo.findById();
        Helper.parse();
    }
}
";
        let pf = parse_ok(Lang::java(), src);
        assert!(
            has_edge(&pf, "Svc.run", "UserRepository.findById"),
            "a dotted field receiver still resolves; edges: {:?}",
            pf.edges
        );
        assert!(
            has_edge(&pf, "Svc.run", "Helper.parse"),
            "a type-looking receiver still qualifies; edges: {:?}",
            pf.edges
        );
    }

    /// A call inside a Java constructor used to produce no edge at all: the
    /// walk up from the call looked for a `function_kind`, a constructor is
    /// none of them, and an edge with no source is dropped. Constructor
    /// injection is the norm in Quarkus, so this was not an edge case.
    #[test]
    fn a_call_inside_a_java_constructor_produces_an_edge() {
        let src = "\
public class Svc {
    private final Repo repo;
    public Svc(Repo repo) {
        this.repo = repo;
        register();
    }
    void register() {}
}
";
        let pf = parse_ok(Lang::java(), src);
        assert!(
            has_edge(&pf, "Svc.Svc", "register"),
            "the constructor's call should be an edge; edges: {:?}",
            pf.edges
        );
    }

    #[test]
    fn resolves_rust_param_receiver_type() {
        let src = "fn handle(repo: Repo) { repo.load(); }\n";
        let pf = parse_ok(Lang::rust(), src);
        assert!(
            has_edge(&pf, "handle", "Repo.load"),
            "edges: {:?}",
            pf.edges
        );
    }

    #[test]
    fn resolves_go_param_receiver_type() {
        let src = "package m\nfunc handle(repo Repo) {\n\trepo.Save()\n}\n";
        let pf = parse_ok(Lang::go(), src);
        assert!(
            has_edge(&pf, "handle", "Repo.Save"),
            "edges: {:?}",
            pf.edges
        );
    }

    #[test]
    fn resolves_python_typed_param_receiver() {
        let src = "def handle(repo: Repo):\n    repo.load()\n";
        let pf = parse_ok(Lang::python(), src);
        assert!(
            has_edge(&pf, "handle", "Repo.load"),
            "edges: {:?}",
            pf.edges
        );
    }

    #[test]
    fn resolves_typescript_local_var_receiver() {
        let src = "function handle() {\n  const repo: Repo = make();\n  repo.load();\n}\n";
        let pf = parse_ok(Lang::typescript(), src);
        assert!(
            has_edge(&pf, "handle", "Repo.load"),
            "edges: {:?}",
            pf.edges
        );
    }

    /// The prose above a function is where its purpose is written; without it
    /// a chunk carries only identifiers, and a question phrased as behaviour
    /// has nothing to match.
    #[test]
    fn a_symbol_starts_at_its_doc_comment() {
        let src = "\
/// Waits for the lock holder to exit.
/// Returns false if it outlives the timeout.
#[inline]
fn wait_for_exit() {}
";
        let sym = &parse_ok(Lang::rust(), src).symbols[0];
        assert_eq!(sym.doc_start_line, 1, "starts at the first `///` line");
        assert_eq!(sym.start_line, 4, "the definition itself is unmoved");
        assert!(src[sym.doc_start_byte..sym.end_byte].contains("lock holder"));
    }

    /// A comment separated by a blank line is a section heading; one sharing a
    /// line with the code above it is a remark about *that* code; and `//!`
    /// documents the file. None of them belong to the symbol below.
    #[test]
    fn only_the_comment_directly_above_is_taken() {
        let blank = "/// Belongs to nothing.\n\nfn f() {}\n";
        assert_eq!(parse_ok(Lang::rust(), blank).symbols[0].doc_start_line, 3);

        let trailing = "fn a() {} // unrelated\nfn f() {}\n";
        assert_eq!(
            parse_ok(Lang::rust(), trailing).symbols[1].doc_start_line,
            2
        );

        let module = "//! The parser.\nfn f() {}\n";
        assert_eq!(parse_ok(Lang::rust(), module).symbols[0].doc_start_line, 2);
    }

    /// Every language we parse writes docs above the definition, whatever the
    /// comment syntax.
    #[test]
    fn doc_comments_are_found_in_every_language() {
        let cases = [
            (
                Lang::python(),
                "# Greets.\ndef greet():\n    pass\n",
                "greet",
            ),
            (
                Lang::go(),
                "package m\n\n// Greets.\nfunc Greet() {}\n",
                "Greet",
            ),
            (
                Lang::typescript(),
                "/** Greets. */\nfunction greet() {}\n",
                "greet",
            ),
            (Lang::java(), "// Greets.\nclass Greeter {}\n", "Greeter"),
        ];
        for (lang, src, name) in cases {
            let pf = parse_ok(lang, src);
            let sym = find(&pf, name);
            assert!(
                src[sym.doc_start_byte..sym.end_byte].contains("Greets"),
                "{lang:?} dropped the doc comment"
            );
        }
    }

    #[test]
    fn detects_language_from_path() {
        assert_eq!(detect_lang(Path::new("a/b/foo.py")), Some(Lang::python()));
        assert_eq!(detect_lang(Path::new("Main.java")), Some(Lang::java()));
        assert_eq!(detect_lang(Path::new("mod.rs")), Some(Lang::rust()));
        assert_eq!(detect_lang(Path::new("app.tsx")), Some(Lang::tsx()));
        assert_eq!(detect_lang(Path::new("README.md")), None);
    }

    #[test]
    fn raw_text_languages_map() {
        assert_eq!(raw_text_language("md"), Some("markdown"));
        assert_eq!(raw_text_language("yaml"), Some("yaml"));
        assert_eq!(raw_text_language("kt"), Some("kotlin"));
        assert_eq!(raw_text_language("py"), None);
    }

    #[test]
    fn tsx_reports_typescript() {
        assert_eq!(Lang::tsx().name(), "typescript");
        let pf = parse_ok(Lang::tsx(), "export function C() { return null; }");
        assert_eq!(pf.language, "typescript");
        assert_eq!(find(&pf, "C").kind, "function");
    }
}
