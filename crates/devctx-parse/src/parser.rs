//! The tree-sitter-backed language parser.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::error::{ParseError, Result};
use crate::lang::Lang;
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
        let (edges, module_edges) = self.extract_edges(root, bytes, &type_map);
        let file_symbol = Symbol {
            kind: devctx_core::symbol_id::FILE_KIND.to_string(),
            language: self.lang.name().to_string(),
            start_line: 1,
            end_line: root.end_position().row as u32 + 1,
            start_byte: 0,
            end_byte: bytes.len(),
            doc_start_line: 1,
            doc_start_byte: 0,
            ..Default::default()
        };
        Ok(ParsedFile {
            language: self.lang.name().to_string(),
            symbols,
            imports,
            edges,
            module_edges,
            file_symbol,
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

    /// Every call site, split by whether a named function encloses it.
    ///
    /// One edge per occurrence: two calls to the same target from the same
    /// function are two edges (the store's `graph_edges` writer folds them,
    /// the `edges` table keeps both). A call with no named enclosing function
    /// goes to the second list instead of being dropped.
    fn extract_edges(
        &self,
        root: Node<'_>,
        bytes: &[u8],
        type_map: &TypeMap,
    ) -> (Vec<GraphEdge>, Vec<GraphEdge>) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.calls_query, root, bytes);
        let mut out = Vec::new();
        let mut module = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let callee = cap.node;
                let Ok(name) = callee.utf8_text(bytes) else {
                    continue;
                };
                let func = enclosing_function_node(callee, self.lang.function_kinds());
                let target = qualified_target(callee, name, bytes, type_map, self.lang);
                let mut edge = GraphEdge {
                    source: String::new(),
                    target,
                    kind: "calls".to_string(),
                    line: callee.start_position().row as u32 + 1,
                    byte: callee.start_byte(),
                    source_byte: func.map(|f| f.start_byte()),
                    src_id: 0,
                };
                // Source: the enclosing function, qualified with its class if any.
                match qualified_source(callee, bytes, self.lang) {
                    Some(source) => {
                        edge.source = source;
                        out.push(edge);
                    }
                    None => module.push(edge),
                }
            }
        }
        out.sort_by_key(|e| e.line);
        module.sort_by_key(|e| e.line);
        (out, module)
    }

    fn extract_symbols(&self, root: Node<'_>, bytes: &[u8]) -> Vec<Symbol> {
        let names = self.symbol_query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.symbol_query, root, bytes);
        // Every definition first: the qualified name of each symbol needs to
        // know which of its ancestors are symbols too.
        let mut defs = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let kind = names[cap.index as usize].to_string();
                let name_node = cap.node;
                let Ok(name) = name_node.utf8_text(bytes) else {
                    continue;
                };
                let def = name_node.parent().unwrap_or(name_node);
                defs.push((kind, name.to_string(), def));
            }
        }
        let def_ids: HashSet<usize> = defs.iter().map(|(_, _, d)| d.id()).collect();

        let mut out = Vec::with_capacity(defs.len());
        for (mut kind, name, def) in defs {
            let container = enclosing_container(def, self.lang.container_kinds());
            let parent = container.and_then(|c| container_name(c, bytes));
            if kind == "function" && container.is_some() {
                kind = "method".to_string();
            }

            let head = doc_head(def, bytes);
            let (mut chain, scope_shape) = qualifier_chain(def, bytes, self.lang, &def_ids);
            chain.push(name.clone());
            let params = self
                .lang
                .overloads()
                .then(|| param_types(def, bytes))
                .flatten();
            out.push(Symbol {
                name,
                kind,
                language: self.lang.name().to_string(),
                start_line: def.start_position().row as u32 + 1,
                end_line: def.end_position().row as u32 + 1,
                start_byte: def.start_byte(),
                end_byte: def.end_byte(),
                doc_start_line: head.start_position().row as u32 + 1,
                doc_start_byte: head.start_byte(),
                parent,
                qualified: chain.join("."),
                signature: signature_of(def, bytes),
                params,
                trait_of: impl_trait(def, bytes, self.lang.container_kinds()),
                scope_shape,
                id: 0,
                parent_id: None,
            });
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

/// Where a symbol's text really begins: at the doc comment above it, not at
/// the keyword.
///
/// Tree-sitter's definition node starts at `fn`/`class`/`func`, which leaves
/// the `///` explanation directly above it inside no symbol at all — so the one
/// place we wrote *why* the code exists never reaches the index, and questions
/// phrased as behaviour ("what happens when two processes open the same file")
/// have nothing to match but identifiers.
///
/// Walks back over the run of comments and attributes/decorators sitting
/// immediately above `def`, and returns the first of them. The run ends at a
/// blank line, at anything that is not a comment or an attribute, at a trailing
/// comment sharing a line with the previous item, or at a module-level `//!`
/// doc, which belongs to the file rather than to this symbol.
fn doc_head<'t>(def: Node<'t>, bytes: &[u8]) -> Node<'t> {
    let mut head = def;
    let mut cur = def;
    while let Some(prev) = cur.prev_sibling() {
        let kind = prev.kind();
        let is_comment = kind.contains("comment");
        if !is_comment && !kind.contains("attribute") && !kind.contains("decorator") {
            break;
        }
        if !starts_its_own_line(prev.start_byte(), bytes) {
            break;
        }
        if breaks_the_run(
            trimmed_end(prev.end_byte(), bytes),
            head.start_byte(),
            bytes,
        ) {
            break;
        }
        if is_comment && is_inner_doc(prev, bytes) {
            break;
        }
        head = prev;
        cur = prev;
    }
    head
}

/// Is `at` preceded only by indentation on its line? A comment that shares a
/// line with the code before it is a trailing remark about *that* code.
fn starts_its_own_line(at: usize, bytes: &[u8]) -> bool {
    bytes[..at]
        .iter()
        .rev()
        .take_while(|b| **b != b'\n')
        .all(|b| b.is_ascii_whitespace())
}

/// Back up over trailing whitespace. Grammars disagree on whether a line
/// comment's node swallows its newline — tree-sitter-rust does, tree-sitter-go
/// does not — so trim it off before measuring the gap and both behave alike.
fn trimmed_end(end: usize, bytes: &[u8]) -> usize {
    let mut e = end;
    while e > 0 && bytes[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    e
}

/// Does what sits between two nodes end the run? Anything but a single line
/// break does: a blank line means the comment above is a section heading rather
/// than this symbol's doc, and non-whitespace means something else is in there.
fn breaks_the_run(from: usize, to: usize, bytes: &[u8]) -> bool {
    match bytes.get(from..to) {
        Some(gap) => {
            !gap.iter().all(u8::is_ascii_whitespace)
                || gap.iter().filter(|b| **b == b'\n').count() > 1
        }
        None => true,
    }
}

/// A module-level doc (`//!`, `/*!`) documents the file, not the symbol below.
fn is_inner_doc(node: Node<'_>, bytes: &[u8]) -> bool {
    node.utf8_text(bytes)
        .is_ok_and(|t| t.starts_with("//!") || t.starts_with("/*!"))
}

/// The nearest enclosing function/method definition node, if any.
fn enclosing_function_node<'t>(node: Node<'t>, kinds: &[String]) -> Option<Node<'t>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if kinds.iter().any(|k| k == n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// The edge source: the enclosing function, qualified as `Class.method` when the
/// function is defined inside a container (class/impl/…).
fn qualified_source(node: Node<'_>, bytes: &[u8], lang: Lang) -> Option<String> {
    let func = enclosing_function_node(node, lang.function_kinds())?;
    let name = func
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(bytes).ok())?
        .to_string();
    match enclosing_container(func, lang.container_kinds()).and_then(|c| container_name(c, bytes)) {
        Some(class) => Some(format!("{class}.{name}")),
        None => Some(name),
    }
}

/// The edge target, resolved from the call receiver where possible:
/// `self`/`this` → `EnclosingClass.callee`; a `Type`-looking receiver →
/// `Type.callee`; a local/field whose type is known → `Type.callee`; otherwise
/// the bare callee name.
fn qualified_target(
    callee: Node<'_>,
    name: &str,
    bytes: &[u8],
    type_map: &TypeMap,
    lang: Lang,
) -> String {
    let Some(receiver) = receiver_of(callee, bytes) else {
        return name.to_string();
    };
    match receiver.as_str() {
        "self" | "this" | "cls" | "super" => enclosing_container(callee, lang.container_kinds())
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
        .filter(|t| nameable(t))
        .map(str::to_string)
}

/// Can this receiver text stand for something a person could look up?
///
/// The receiver node of a chained call is the entire expression before the dot.
/// In reactive Java that is routine, and it put graph nodes like
/// `Office.findByCodigo(codigo).flatMap` — and three-line ones with a lambda
/// inside — into the call graph, where they match nothing and are searchable by
/// nobody.
///
/// Only an identifier or a dotted run of identifiers qualifies a target.
/// Anything else falls back to the bare callee name: less specific, but true,
/// and a bare name now finds its own edges anyway.
fn nameable(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
                && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        })
}

/// Walk up from `node` to the nearest container (class/impl/…) definition.
fn enclosing_container<'t>(node: Node<'t>, kinds: &[String]) -> Option<Node<'t>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if kinds.iter().any(|k| k == n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Display name of a container node: its `name` field, or `type` (Rust
/// `impl`) without generic arguments, so `impl<T> Foo<T>` is `Foo` — the name
/// the type's own definition has.
fn container_name(container: Node<'_>, bytes: &[u8]) -> Option<String> {
    if let Some(name) = container.child_by_field_name("name") {
        return name.utf8_text(bytes).ok().map(str::to_string);
    }
    let ty = container.child_by_field_name("type")?;
    ty.utf8_text(bytes).ok().map(bare_type_name)
}

/// A type as written in a container or receiver position, reduced to the
/// name its definition carries: no reference or pointer (`&Foo`, `&'a mut
/// Foo`, `*const Foo`), no generic arguments (`Foo<T>`, Go `Foo[T]`), no path
/// (`crate::x::Foo`, `super::Foo` → `Foo`), no whitespace. A type with no
/// name of its own (`[T]`, `(A, B)`) keeps its text without whitespace, so
/// it still qualifies something and matches no definition.
fn bare_type_name(text: &str) -> String {
    let mut t = text.trim();
    loop {
        let stripped = t
            .strip_prefix('&')
            .or_else(|| t.strip_prefix('*'))
            .or_else(|| t.strip_prefix("mut "))
            .or_else(|| t.strip_prefix("const "))
            .or_else(|| t.strip_prefix("dyn "))
            .or_else(|| strip_lifetime(t))
            .map(str::trim_start);
        match stripped {
            Some(rest) => t = rest,
            None => break,
        }
    }
    let end = t.find(['<', '[']).unwrap_or(t.len());
    let name = last_path_segment(&t[..end]);
    let name: String = name.chars().filter(|c| !c.is_whitespace()).collect();
    if name.is_empty() {
        t.chars().filter(|c| !c.is_whitespace()).collect()
    } else {
        name
    }
}

/// `t` without a leading lifetime (`'a Foo` → `Foo`).
fn strip_lifetime(t: &str) -> Option<&str> {
    let rest = t.strip_prefix('\'')?;
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    Some(&rest[end..])
}

/// The last segment of a `::` path (`std::fmt::Display` → `Display`).
fn last_path_segment(t: &str) -> &str {
    t.rsplit("::").next().unwrap_or(t).trim()
}

/// Names of every scope around `def`, outermost first: its qualifier, and
/// the shape of that qualifier — one character per scope, `f` for a
/// callable (a node of the language's `function_kinds`), `s` for anything
/// else — `None` when no scope is callable.
///
/// A scope is a container (class, `impl`, trait…), any other symbol of the
/// file (a Rust `mod`, an enclosing function or method — a Python decorator's
/// `wrapper`, a Java anonymous class's method) or a language's `scope_kinds`
/// (a TypeScript `namespace`, the variable or property holding an object
/// literal, a Java enum constant, constructor or field). A Go method is
/// qualified by its receiver type. So two homonyms differ by where they are,
/// not by their order in the file (PLAN-009 DD-3), and the ordinal is left
/// for true redefinitions. The shape goes into the id's disambiguator: a
/// function `a` and a module `a` both qualify their contents as `a.P`.
fn qualifier_chain(
    def: Node<'_>,
    bytes: &[u8],
    lang: Lang,
    def_ids: &HashSet<usize>,
) -> (Vec<String>, Option<String>) {
    let mut chain = Vec::new();
    let mut shape = Vec::new();
    if let Some(recv) = go_receiver_type(def, bytes) {
        chain.push(recv);
        shape.push('s');
    }
    let mut cur = def.parent();
    while let Some(n) = cur {
        let name = if lang.container_kinds().iter().any(|k| k == n.kind()) {
            container_name(n, bytes)
        } else if def_ids.contains(&n.id()) || lang.scope_kinds().iter().any(|k| k == n.kind()) {
            n.child_by_field_name("name")
                .or_else(|| n.child_by_field_name("key"))
                .and_then(|c| c.utf8_text(bytes).ok())
                .filter(|t| nameable(t))
                .map(str::to_string)
        } else {
            None
        };
        if let Some(name) = name {
            chain.push(name);
            let callable = lang.function_kinds().iter().any(|k| k == n.kind());
            shape.push(if callable { 'f' } else { 's' });
        }
        cur = n.parent();
    }
    chain.reverse();
    shape.reverse();
    let shape = shape
        .contains(&'f')
        .then(|| shape.into_iter().collect::<String>());
    (chain, shape)
}

/// The receiver type of a Go method (`func (s *Svc[T]) Run()` → `Svc`).
fn go_receiver_type(def: Node<'_>, bytes: &[u8]) -> Option<String> {
    let list = def.child_by_field_name("receiver")?;
    let mut cursor = list.walk();
    let param = list
        .named_children(&mut cursor)
        .find(|p| p.kind() == "parameter_declaration")?;
    let ty = param.child_by_field_name("type")?.utf8_text(bytes).ok()?;
    Some(bare_type_name(ty)).filter(|t| !t.is_empty())
}

/// The trait of the nearest `impl Trait for Type` around `def`, whitespace
/// and path removed, generic arguments kept (`From<A>` and `From<B>` are two
/// impls; `fmt::Display` is `Display`). Part of the id's disambiguator:
/// `impl Display for X { fn fmt }` and `impl Debug for X { fn fmt }` are both
/// `X.fmt`.
fn impl_trait(def: Node<'_>, bytes: &[u8], kinds: &[String]) -> Option<String> {
    let imp = enclosing_container(def, kinds)?;
    let tr = imp.child_by_field_name("trait")?.utf8_text(bytes).ok()?;
    let tr: String = tr.chars().filter(|c| !c.is_whitespace()).collect();
    let args = tr.find('<').unwrap_or(tr.len());
    let name = last_path_segment(&tr[..args]);
    if name.is_empty() {
        return Some(tr);
    }
    Some(format!("{name}{}", &tr[args..]))
}

/// Longest provisional signature kept, in characters.
const SIGNATURE_MAX: usize = 200;

/// The definition up to its body (the whole of it when it has none), first
/// line only, whitespace collapsed, capped at [`SIGNATURE_MAX`] characters:
/// `fn g() -> i32 { 1 }` is `fn g() -> i32`. Provisional until the structured
/// extraction (PLAN-009 TASK-004).
fn signature_of(def: Node<'_>, bytes: &[u8]) -> String {
    let end = def
        .child_by_field_name("body")
        .map_or(def.end_byte(), |b| b.start_byte());
    let text = std::str::from_utf8(&bytes[def.start_byte()..end]).unwrap_or_default();
    let line = text.lines().next().unwrap_or_default();
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(SIGNATURE_MAX)
        .collect()
}

/// The declared parameter types of a callable, normalised (no names, no
/// whitespace, no generic arguments, simple names, comma-separated):
/// `actualizar(Long id, java.util.List<Dto> d)` → `Long,List`.
/// `None` when the node has no parameter list (a class, a field).
fn param_types(def: Node<'_>, bytes: &[u8]) -> Option<String> {
    let list = def.child_by_field_name("parameters")?;
    let mut cursor = list.walk();
    let types: Vec<String> = list
        .named_children(&mut cursor)
        .filter(|p| !p.kind().contains("comment"))
        .map(|p| {
            let ty = p.child_by_field_name("type").or_else(|| {
                (p.kind() == "spread_parameter")
                    .then(|| p.named_child(0))
                    .flatten()
            });
            let text = ty.and_then(|t| t.utf8_text(bytes).ok()).unwrap_or("?");
            let mut norm: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            if let Some(rest) = norm.strip_prefix(':') {
                norm = rest.to_string(); // a TypeScript `type_annotation`
            }
            norm = simple_type_name(&norm);
            if p.kind() == "spread_parameter" {
                norm.push_str("...");
            }
            norm
        })
        .collect();
    Some(types.join(","))
}

/// A Java type reduced to its simple name: generic arguments dropped (no two
/// overloads differ only by them: erasure) and the package path too
/// (`java.util.List<String>[]` → `List[]`), so the spelling does not move
/// the id.
fn simple_type_name(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut depth = 0usize;
    for c in t.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.rsplit('.').next().unwrap_or_default().to_string()
}
