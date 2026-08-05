//! The tree-sitter-backed language parser.

use std::collections::HashMap;

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::error::{ParseError, Result};
use crate::lang::{Lang, CONTAINER_KINDS, FUNCTION_KINDS};
use crate::types::{GraphEdge, Import, ParsedFile, Symbol};

/// Variable/field name → declared type, for receiver resolution.
type TypeMap = HashMap<String, String>;

/// A reusable parser for a single language. Owns the tree-sitter parser and the
/// compiled symbol/import/calls/type queries.
pub struct LanguageParser {
    lang: Lang,
    parser: Parser,
    symbol_query: Query,
    import_query: Query,
    calls_query: Query,
    type_query: Option<Query>,
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
        let type_query = match lang.type_bindings_query() {
            Some(src) => Some(
                Query::new(&grammar, src).map_err(|source| ParseError::Query {
                    lang: lang.name(),
                    source,
                })?,
            ),
            None => None,
        };
        Ok(Self {
            lang,
            parser,
            symbol_query,
            import_query,
            calls_query,
            type_query,
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
        let type_map = self.extract_type_bindings(root, bytes);
        let edges = self.extract_edges(root, bytes, &type_map);
        Ok(ParsedFile {
            language: self.lang.name().to_string(),
            symbols,
            imports,
            edges,
        })
    }

    /// Build the file-level variable/field → type map.
    fn extract_type_bindings(&self, root: Node<'_>, bytes: &[u8]) -> TypeMap {
        let mut map = TypeMap::new();
        let Some(query) = &self.type_query else {
            return map;
        };
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, root, bytes);
        while let Some(m) = matches.next() {
            let mut name = None;
            let mut ty = None;
            for cap in m.captures {
                match names[cap.index as usize] {
                    "name" => name = cap.node.utf8_text(bytes).ok(),
                    "type" => ty = cap.node.utf8_text(bytes).ok(),
                    _ => {}
                }
            }
            if let (Some(n), Some(t)) = (name, ty) {
                map.entry(n.to_string()).or_insert_with(|| t.to_string());
            }
        }
        map
    }

    fn extract_edges(&self, root: Node<'_>, bytes: &[u8], type_map: &TypeMap) -> Vec<GraphEdge> {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.calls_query, root, bytes);
        let mut out = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let callee = cap.node;
                let Ok(name) = callee.utf8_text(bytes) else {
                    continue;
                };
                // Source: the enclosing function, qualified with its class if any.
                let Some(source) = qualified_source(callee, bytes) else {
                    continue; // module-level call: no source symbol.
                };
                let target = qualified_target(callee, name, bytes, type_map);
                out.push(GraphEdge {
                    source,
                    target,
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

/// The nearest enclosing function/method definition node, if any.
fn enclosing_function_node(node: Node<'_>) -> Option<Node<'_>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if FUNCTION_KINDS.contains(&n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// The edge source: the enclosing function, qualified as `Class.method` when the
/// function is defined inside a container (class/impl/…).
fn qualified_source(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let func = enclosing_function_node(node)?;
    let name = func
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(bytes).ok())?
        .to_string();
    match enclosing_container(func).and_then(|c| container_name(c, bytes)) {
        Some(class) => Some(format!("{class}.{name}")),
        None => Some(name),
    }
}

/// The edge target, resolved from the call receiver where possible:
/// `self`/`this` → `EnclosingClass.callee`; a `Type`-looking receiver →
/// `Type.callee`; a local/field whose type is known → `Type.callee`; otherwise
/// the bare callee name.
fn qualified_target(callee: Node<'_>, name: &str, bytes: &[u8], type_map: &TypeMap) -> String {
    let Some(receiver) = receiver_of(callee, bytes) else {
        return name.to_string();
    };
    match receiver.as_str() {
        "self" | "this" | "cls" | "super" => enclosing_container(callee)
            .and_then(|c| container_name(c, bytes))
            .map(|class| format!("{class}.{name}"))
            .unwrap_or_else(|| name.to_string()),
        r if r.chars().next().is_some_and(|c| c.is_uppercase()) => format!("{r}.{name}"),
        r => {
            // Local/field receiver: resolve its declared type (also handles a
            // `self.field` / `this.field` receiver by stripping the prefix).
            let key = r
                .strip_prefix("self.")
                .or_else(|| r.strip_prefix("this."))
                .unwrap_or(r);
            match type_map.get(key) {
                Some(ty) => format!("{ty}.{name}"),
                None => name.to_string(),
            }
        }
    }
}

/// Text of the call's receiver (the object before the `.`), if this is a
/// member/method call rather than a plain function call.
fn receiver_of(callee: Node<'_>, bytes: &[u8]) -> Option<String> {
    let parent = callee.parent()?;
    let field = match parent.kind() {
        "attribute" | "member_expression" | "method_invocation" => "object",
        "selector_expression" => "operand",
        "field_expression" => "value",
        _ => return None,
    };
    parent
        .child_by_field_name(field)
        .and_then(|n| n.utf8_text(bytes).ok())
        .map(str::to_string)
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
