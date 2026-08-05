//! `devai-parse` — source parsing for DevAI.
//!
//! F3: tree-sitter symbol + import extraction for a starter language set
//! (Python, JavaScript, TypeScript/TSX, Go, Java, Rust), plus extension-based
//! language detection (parseable + raw-text). Call-graph edges, exports and the
//! framework route extractors land in a follow-up. See `docs/rust-rewrite-plan.md` §4.

pub mod error;
pub mod lang;
pub mod parser;
pub mod routes;
pub mod types;

pub use error::{ParseError, Result};
pub use lang::{lang_for_extension, raw_text_language, Lang};
pub use parser::LanguageParser;
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
        for &l in lang::ALL {
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
        let pf = parse_ok(Lang::Python, src);
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
        let pf = parse_ok(Lang::Rust, src);
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
        let pf = parse_ok(Lang::Go, src);
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
        let pf = parse_ok(Lang::Java, src);
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
        let pf = parse_ok(Lang::TypeScript, src);
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
        let pf = parse_ok(Lang::JavaScript, src);
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
        let pf = parse_ok(Lang::Rust, src);
        let calls: Vec<_> = pf
            .edges
            .iter()
            .filter(|e| e.source == "caller" && e.target == "helper")
            .collect();
        assert_eq!(calls.len(), 2, "edges: {:?}", pf.edges);
        assert!(pf.edges.iter().all(|e| e.kind == "calls"));
    }

    #[test]
    fn extracts_python_method_call_edges() {
        let src = "\
class A:
    def run(self):
        self.helper()

    def helper(self):
        pass
";
        let pf = parse_ok(Lang::Python, src);
        assert!(
            pf.edges
                .iter()
                .any(|e| e.source == "run" && e.target == "helper"),
            "edges: {:?}",
            pf.edges
        );
    }

    #[test]
    fn detects_language_from_path() {
        assert_eq!(detect_lang(Path::new("a/b/foo.py")), Some(Lang::Python));
        assert_eq!(detect_lang(Path::new("Main.java")), Some(Lang::Java));
        assert_eq!(detect_lang(Path::new("mod.rs")), Some(Lang::Rust));
        assert_eq!(detect_lang(Path::new("app.tsx")), Some(Lang::Tsx));
        assert_eq!(detect_lang(Path::new("README.md")), None);
    }

    #[test]
    fn raw_text_languages_map() {
        assert_eq!(raw_text_language("md"), Some("markdown"));
        assert_eq!(raw_text_language("yaml"), Some("yaml"));
        assert_eq!(raw_text_language("py"), None);
    }

    #[test]
    fn tsx_reports_typescript() {
        assert_eq!(Lang::Tsx.name(), "typescript");
        let pf = parse_ok(Lang::Tsx, "export function C() { return null; }");
        assert_eq!(pf.language, "typescript");
        assert_eq!(find(&pf, "C").kind, "function");
    }
}
