//! The tree-sitter-backed language parser.

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::error::{ParseError, Result};
use crate::lang::{Lang, CONTAINER_KINDS, FUNCTION_KINDS};
use crate::types::{GraphEdge, Import, ParsedFile, Symbol};

/// A reusable parser for a single language. Owns the tree-sitter parser and the
/// compiled symbol/import/calls queries.
pub struct LanguageParser {
    lang: Lang,
    parser: Parser,
    symbol_query: Query,
    import_query: Query,
    calls_query: Query,
}

impl LanguageParser {
    /// Build a parser for `lang`, compiling its queries.
    pub fn new(lang: Lang) -> Result<Self> {
        let grammar = lang.grammar();
        let mut parser = Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|_| ParseError::Grammar(lang.name()))?;
        let symbol_query =
            Query::new(&grammar, lang.symbol_query()).map_err(|source| ParseError::Query {
                lang: lang.name(),
                source,
            })?;
        let import_query =
            Query::new(&grammar, lang.import_query()).map_err(|source| ParseError::Query {
                lang: lang.name(),
                source,
            })?;
        let calls_query =
            Query::new(&grammar, lang.calls_query()).map_err(|source| ParseError::Query {
                lang: lang.name(),
                source,
            })?;
        Ok(Self {
            lang,
            parser,
            symbol_query,
            import_query,
            calls_query,
        })
    }

    /// Parse `source`, extracting symbols and imports.
    pub fn parse(&mut self, source: &str) -> Result<ParsedFile> {
        let tree = self
            .parser
            .parse(source, None)
            .ok_or(ParseError::NoTree(self.lang.name()))?;
        let root = tree.root_node();
        let bytes = source.as_bytes();

        let symbols = self.extract_symbols(root, bytes);
        let imports = self.extract_imports(root, bytes);
        let edges = self.extract_edges(root, bytes);
        Ok(ParsedFile {
            language: self.lang.name().to_string(),
            symbols,
            imports,
            edges,
        })
    }

    fn extract_edges(&self, root: Node<'_>, bytes: &[u8]) -> Vec<GraphEdge> {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.calls_query, root, bytes);
        let mut out = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let callee = cap.node;
                let Ok(target) = callee.utf8_text(bytes) else {
                    continue;
                };
                // Resolve the enclosing function/method as the edge source.
                let Some(source) = enclosing_function_name(callee, bytes) else {
                    continue; // module-level call: no source symbol.
                };
                out.push(GraphEdge {
                    source,
                    target: target.to_string(),
                    kind: "calls".to_string(),
                    line: callee.start_position().row as u32 + 1,
                });
            }
        }
        out.sort_by_key(|e| e.line);
        out
    }

    fn extract_symbols(&self, root: Node<'_>, bytes: &[u8]) -> Vec<Symbol> {
        let names = self.symbol_query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.symbol_query, root, bytes);
        let mut out = Vec::new();

        while let Some(m) = matches.next() {
            for cap in m.captures {
                let mut kind = names[cap.index as usize].to_string();
                let name_node = cap.node;
                let Ok(name) = name_node.utf8_text(bytes) else {
                    continue;
                };
                let def = name_node.parent().unwrap_or(name_node);

                let container = enclosing_container(def);
                let parent = container.and_then(|c| container_name(c, bytes));
                if kind == "function" && container.is_some() {
                    kind = "method".to_string();
                }

                out.push(Symbol {
                    name: name.to_string(),
                    kind,
                    language: self.lang.name().to_string(),
                    start_line: def.start_position().row as u32 + 1,
                    end_line: def.end_position().row as u32 + 1,
                    start_byte: def.start_byte(),
                    end_byte: def.end_byte(),
                    parent,
                });
            }
        }
        out.sort_by_key(|s| s.start_byte);
        out
    }

    fn extract_imports(&self, root: Node<'_>, bytes: &[u8]) -> Vec<Import> {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.import_query, root, bytes);
        let mut out = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                if let Ok(text) = cap.node.utf8_text(bytes) {
                    out.push(Import {
                        statement: text.to_string(),
                        line: cap.node.start_position().row as u32 + 1,
                    });
                }
            }
        }
        out.sort_by_key(|i| i.line);
        out
    }
}

/// Name of the nearest enclosing function/method definition, if any.
fn enclosing_function_name(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if FUNCTION_KINDS.contains(&n.kind()) {
            return n
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(bytes).ok())
                .map(str::to_string);
        }
        cur = n.parent();
    }
    None
}

/// Walk up from `node` to the nearest container (class/impl/…) definition.
fn enclosing_container(node: Node<'_>) -> Option<Node<'_>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if CONTAINER_KINDS.contains(&n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Display name of a container node: its `name` field, or `type` (Rust `impl`).
fn container_name(container: Node<'_>, bytes: &[u8]) -> Option<String> {
    let name_node = container
        .child_by_field_name("name")
        .or_else(|| container.child_by_field_name("type"))?;
    name_node.utf8_text(bytes).ok().map(str::to_string)
}
