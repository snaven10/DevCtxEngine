//! The tree-sitter-backed language parser.

use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::error::{ParseError, Result};
use crate::facts::{
    FileFacts, ImportFact, InheritFact, RefFact, IMPLEMENTS, INHERITS, INSTANTIATES, REFERENCES,
};
use crate::lang::Lang;
use crate::resolve::java::is_constant_name;
use crate::resolve::scope::{Scopes, Via};
use crate::resolve::{resolver_for, LangResolver};
use crate::types::{GraphEdge, Import, ParsedFile, Symbol};

/// The name node of every symbol (by node id) → the symbol's qualified name
/// and start byte: which bindings of an anonymous callable are symbols.
type Bound = HashMap<usize, (String, usize)>;

/// A reusable parser for a single language. Owns the tree-sitter parser and the
/// compiled queries of its definition (`languages/<lang>.json`).
pub struct LanguageParser {
    lang: Lang,
    parser: Parser,
    resolver: &'static dyn LangResolver,
    definitions: Query,
    references: Query,
    inherits: Option<Query>,
    imports: Query,
    package: Option<Query>,
    type_query: Option<Query>,
}

/// Compile one query of `lang`, naming the language on failure.
fn compile(lang: Lang, src: &str) -> Result<Query> {
    Query::new(&lang.grammar(), src).map_err(|source| ParseError::Query {
        lang: lang.name(),
        source,
    })
}

impl LanguageParser {
    /// Build a parser for `lang`, compiling its queries.
    pub fn new(lang: Lang) -> Result<Self> {
        let grammar = lang.grammar();
        let mut parser = Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|_| ParseError::Grammar(lang.name()))?;
        let optional = |src: Option<&str>| src.map(|s| compile(lang, s)).transpose();
        Ok(Self {
            lang,
            parser,
            resolver: resolver_for(lang),
            definitions: compile(lang, lang.definitions_query())?,
            references: compile(lang, lang.references_query())?,
            inherits: optional(lang.inherits_query())?,
            imports: compile(lang, lang.import_query())?,
            package: optional(lang.package_query())?,
            type_query: optional(lang.type_bindings_query())?,
        })
    }

    /// Parse `source`: symbols, imports, calls and the structured facts.
    pub fn parse(&mut self, source: &str) -> Result<ParsedFile> {
        let tree = self
            .parser
            .parse(source, None)
            .ok_or(ParseError::NoTree(self.lang.name()))?;
        let root = tree.root_node();
        let bytes = source.as_bytes();

        let (symbols, bound) = self.extract_symbols(root, bytes);
        let (imports, import_facts) = self.extract_imports(root, bytes);
        // Angular's `inject(T)` injects; a local helper of that name does
        // not (TASK-006 review, m5).
        let injects = import_facts.iter().any(|i| {
            i.path == "@angular/core" && i.name.as_deref() == Some("inject") && i.alias.is_none()
        });
        let scopes = Scopes::build(
            root,
            bytes,
            self.lang,
            self.type_query.as_ref(),
            self.resolver,
            injects,
        );
        let (edges, module_edges, refs) = self.extract_references(root, bytes, &scopes, &bound);
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
        let facts = FileFacts {
            package: self.extract_package(root, bytes),
            imports: import_facts,
            inherits: self.extract_inherits(root, bytes),
            refs,
        };
        Ok(ParsedFile {
            language: self.lang.name().to_string(),
            symbols,
            imports,
            edges,
            module_edges,
            file_symbol,
            facts,
        })
    }

    /// Every reference: calls (split by whether a named function encloses
    /// them), instantiations and type uses.
    ///
    /// One call edge per occurrence: two calls to the same target from the
    /// same function are two edges (the store's `graph_edges` writer folds
    /// them, the `edges` table keeps both). A call with no named enclosing
    /// function goes to the second list instead of being dropped.
    fn extract_references(
        &self,
        root: Node<'_>,
        bytes: &[u8],
        scopes: &Scopes,
        bound: &Bound,
    ) -> (Vec<GraphEdge>, Vec<GraphEdge>, Vec<RefFact>) {
        let names = self.references.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.references, root, bytes);
        let mut out = Vec::new();
        let mut module = Vec::new();
        let mut refs = Vec::new();
        while let Some(m) = matches.next() {
            let mut role = None;
            let (mut name, mut path, mut ty) = (None, None, None);
            for cap in m.captures {
                match names[cap.index as usize] {
                    "name" => name = Some(cap.node),
                    "path" => path = Some(cap.node),
                    "type" => ty = Some(cap.node),
                    r => role = r.strip_prefix("reference.").map(|k| (k, cap.node)),
                }
            }
            let Some((role, node)) = role else { continue };
            match role {
                "call" => {
                    let Some(callee) = name else { continue };
                    let Ok(callee_name) = callee.utf8_text(bytes) else {
                        continue;
                    };
                    let func = enclosing_source(callee, self.lang, bound);
                    let (target, hint) = match path {
                        Some(p) => (
                            path_target(callee, p, callee_name, bytes, self.lang),
                            "path".to_string(),
                        ),
                        None => {
                            let recv = classify(callee, bytes, scopes, 0);
                            (
                                recv.target(callee, callee_name, bytes, self.lang),
                                recv.hint(),
                            )
                        }
                    };
                    let hint = match call_arity(callee) {
                        Some(n) => format!("{hint} /{n}"),
                        None => hint,
                    };
                    let mut edge = GraphEdge {
                        source: String::new(),
                        target,
                        kind: "calls".to_string(),
                        line: callee.start_position().row as u32 + 1,
                        byte: callee.start_byte(),
                        source_byte: func.as_ref().map(|(b, _)| *b),
                        src_id: 0,
                        hint: Some(hint),
                    };
                    // Source: the enclosing function, qualified with its class if any.
                    match func {
                        Some((_, source)) => {
                            edge.source = source;
                            out.push(edge);
                        }
                        None => module.push(edge),
                    }
                }
                "new" => {
                    let target = ty.unwrap_or(node);
                    let Some(name) = type_ref_name(target, bytes, self.lang) else {
                        continue;
                    };
                    refs.push(self.ref_fact(INSTANTIATES, name, target, bound));
                }
                "type" => {
                    let mut found = Vec::new();
                    type_name_nodes(ty.unwrap_or(node), self.lang.type_names(), &mut found);
                    for t in found {
                        let Some(name) = type_ref_name(t, bytes, self.lang) else {
                            continue;
                        };
                        // A type parameter (`T` of `Cached<T>`) names nothing
                        // outside its declaration.
                        if is_type_parameter(t, &name, bytes) {
                            continue;
                        }
                        refs.push(self.ref_fact(REFERENCES, name, t, bound));
                    }
                }
                _ => {}
            }
        }
        out.sort_by_key(|e| (e.line, e.byte));
        module.sort_by_key(|e| (e.line, e.byte));
        refs.sort_by_key(|r| (r.byte, r.kind != INSTANTIATES));
        (out, module, refs)
    }

    fn ref_fact(&self, kind: &str, name: String, node: Node<'_>, bound: &Bound) -> RefFact {
        RefFact {
            kind: kind.to_string(),
            name,
            line: node.start_position().row as u32 + 1,
            byte: node.start_byte(),
            source_byte: enclosing_source(node, self.lang, bound).map(|(b, _)| b),
            src_id: 0,
        }
    }

    /// Supertypes named by the file's type definitions.
    fn extract_inherits(&self, root: Node<'_>, bytes: &[u8]) -> Vec<InheritFact> {
        let Some(query) = &self.inherits else {
            return Vec::new();
        };
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, root, bytes);
        let mut out = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let kind = match names[cap.index as usize] {
                    "inherit.extends" => INHERITS,
                    "inherit.implements" => IMPLEMENTS,
                    _ => continue,
                };
                let Some(name) = type_ref_name(cap.node, bytes, self.lang) else {
                    continue;
                };
                out.push(InheritFact {
                    kind: kind.to_string(),
                    name,
                    line: cap.node.start_position().row as u32 + 1,
                    byte: cap.node.start_byte(),
                    src_id: 0,
                });
            }
        }
        out.sort_by_key(|f| f.byte);
        out.dedup();
        out
    }

    /// The package the source declares (Java, Go).
    fn extract_package(&self, root: Node<'_>, bytes: &[u8]) -> Option<String> {
        let query = self.package.as_ref()?;
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, root, bytes);
        let m = matches.next()?;
        let node = m.captures.first()?.node;
        let text: String = node
            .utf8_text(bytes)
            .ok()?
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        Some(text).filter(|t| !t.is_empty())
    }

    /// The file's symbols in source order, and which name node each one
    /// has ([`Bound`]).
    fn extract_symbols(&self, root: Node<'_>, bytes: &[u8]) -> (Vec<Symbol>, Bound) {
        let names = self.definitions.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.definitions, root, bytes);
        // Every definition first: the qualified name of each symbol needs to
        // know which of its ancestors are symbols too. One (definition, name)
        // pair captured by several patterns keeps the earliest pattern's kind.
        let mut defs: Vec<(usize, String, String, Node<'_>, Node<'_>)> = Vec::new();
        let mut at: HashMap<(usize, usize), usize> = HashMap::new();
        while let Some(m) = matches.next() {
            let mut def = None;
            let mut name_node = None;
            for cap in m.captures {
                let n = names[cap.index as usize];
                if let Some(kind) = n.strip_prefix("definition.") {
                    def = Some((kind, cap.node));
                } else if n == "name" {
                    name_node = Some(cap.node);
                }
            }
            let (Some((kind, def)), Some(name_node)) = (def, name_node) else {
                continue;
            };
            let Ok(text) = name_node.utf8_text(bytes) else {
                continue;
            };
            // An `impl` is named after the type it implements, as written in
            // its definition (`impl<T> Foo<T>` → `Foo`).
            let name = if kind == "impl" {
                bare_type_name(text)
            } else {
                text.to_string()
            };
            let entry = (m.pattern_index, kind.to_string(), name, def, name_node);
            match at.get(&(def.id(), name_node.id())) {
                Some(&i) if defs[i].0 <= m.pattern_index => {}
                Some(&i) => defs[i] = entry,
                None => {
                    at.insert((def.id(), name_node.id()), defs.len());
                    defs.push(entry);
                }
            }
        }
        let def_ids: HashSet<usize> = defs.iter().map(|(_, _, _, d, _)| d.id()).collect();

        let mut out = Vec::with_capacity(defs.len());
        let mut bound = Bound::new();
        for (_, mut kind, name, def, name_node) in defs {
            let container = enclosing_container(def, self.lang.container_kinds());
            let parent = container.and_then(|c| container_name(c, bytes));
            if kind == "function" && container.is_some() {
                kind = "method".to_string();
            }
            // A JS/TS class's `constructor` is a method by syntax.
            if kind == "method" && name == "constructor" && def.kind() == "method_definition" {
                kind = "constructor".to_string();
            }

            // One of several declarators (`const a = …, b = …`, `int a, b;`)
            // spans its own declarator, the first one from the keyword (and
            // its doc), so no two share a span and none contains another.
            let (start, end, head_byte, head_line, signature) = match own_declarator(def, name_node)
            {
                Some((decl, first)) => {
                    let (head, start) = if first {
                        let h = doc_head(def, bytes);
                        (h, def)
                    } else {
                        (decl, decl)
                    };
                    let body = declarator_body(decl).unwrap_or(decl.end_byte());
                    (
                        start.start_position(),
                        decl.end_position(),
                        (head.start_byte(), start.start_byte(), decl.end_byte()),
                        head.start_position().row as u32 + 1,
                        signature_text(bytes, start.start_byte(), body),
                    )
                }
                None => {
                    let head = doc_head(def, bytes);
                    (
                        def.start_position(),
                        def.end_position(),
                        (head.start_byte(), def.start_byte(), def.end_byte()),
                        head.start_position().row as u32 + 1,
                        signature_of(def, bytes),
                    )
                }
            };
            let (doc_start_byte, start_byte, end_byte) = head_byte;
            let (mut chain, scope_shape) = qualifier_chain(def, bytes, self.lang, &def_ids);
            chain.push(name.clone());
            let params = (self.lang.overloads()
                && devctx_core::symbol_id::kind_class(&kind) == "callable")
                .then(|| param_types(def, bytes))
                .flatten();
            let (trait_of, impl_args) = if kind == "impl" {
                (trait_text(def, bytes), impl_self_args(def, bytes))
            } else {
                let imp = enclosing_container(def, self.lang.container_kinds());
                (
                    imp.and_then(|i| trait_text(i, bytes)),
                    imp.and_then(|i| impl_self_args(i, bytes)),
                )
            };
            let exported = self.resolver.exported(def, &name, bytes);
            let qualified = chain.join(".");
            bound.insert(name_node.id(), (qualified.clone(), start_byte));
            out.push(Symbol {
                name,
                kind,
                language: self.lang.name().to_string(),
                start_line: start.row as u32 + 1,
                end_line: end.row as u32 + 1,
                start_byte,
                end_byte,
                doc_start_line: head_line,
                doc_start_byte,
                parent,
                qualified,
                signature,
                exported,
                params,
                trait_of,
                impl_args,
                scope_shape,
                id: 0,
                parent_id: None,
            });
        }
        out.sort_by_key(|s| (s.start_byte, std::cmp::Reverse(s.end_byte)));
        (out, bound)
    }

    /// The import statements as text (what the file chunk lists), and one
    /// structured fact per name they bring in.
    fn extract_imports(&self, root: Node<'_>, bytes: &[u8]) -> (Vec<Import>, Vec<ImportFact>) {
        let names = self.imports.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.imports, root, bytes);
        let mut statements: Vec<Node<'_>> = Vec::new();
        // (statement id, fact): a statement can bring in several names.
        let mut facts: Vec<(usize, ImportFact)> = Vec::new();
        while let Some(m) = matches.next() {
            let mut stmt = None;
            let mut fact = ImportFact::default();
            let mut tree = None;
            // The line of what is imported: one `import (…)` block in Go
            // spans several.
            let mut at = None;
            for cap in m.captures {
                let text = || -> String {
                    cap.node
                        .utf8_text(bytes)
                        .unwrap_or_default()
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect()
                };
                match names[cap.index as usize] {
                    "import" => stmt = Some(cap.node),
                    "import.path" => {
                        fact.path = unquote(&text()).to_string();
                        at = Some(cap.node.start_position().row as u32 + 1);
                    }
                    "import.name" => fact.name = Some(text()),
                    "import.alias" => fact.alias = Some(text()),
                    "import.wildcard" => fact.wildcard = true,
                    "import.default" => {
                        fact.name = Some("default".into());
                        fact.alias = Some(text());
                    }
                    "import.namespace" => {
                        fact.wildcard = true;
                        fact.alias = Some(text());
                    }
                    "import.tree" => tree = Some(cap.node),
                    _ => {}
                }
            }
            let Some(stmt) = stmt else { continue };
            // `export { x } from './y'`, `export * from './y'`: re-exported,
            // not bound in the file.
            fact.reexport = stmt.kind() == "export_statement";
            if !statements.iter().any(|s| s.id() == stmt.id()) {
                statements.push(stmt);
            }
            let line = at.unwrap_or(stmt.start_position().row as u32 + 1);
            let expanded = match tree {
                Some(t) => self.resolver.expand_import_tree(t, bytes),
                None if fact.path.is_empty() => Vec::new(),
                None => vec![fact],
            };
            for mut f in expanded {
                f.line = line;
                f.target = self.resolver.import_target(&f);
                facts.push((stmt.id(), f));
            }
        }
        // A pattern that only names the path (`import './x'`, `import a.B;`)
        // also matches statements another pattern details: keep the detail.
        let detailed: HashSet<(usize, String)> = facts
            .iter()
            .filter(|(_, f)| f.name.is_some() || f.alias.is_some() || f.wildcard)
            .map(|(s, f)| (*s, f.path.clone()))
            .collect();
        let mut seen = HashSet::new();
        let facts: Vec<ImportFact> = facts
            .into_iter()
            .filter(|(s, f)| {
                let bare = f.name.is_none() && f.alias.is_none() && !f.wildcard;
                !(bare && detailed.contains(&(*s, f.path.clone())))
            })
            .filter(|(s, f)| seen.insert((*s, f.clone())))
            .map(|(_, f)| f)
            .collect();
        statements.sort_by_key(|n| n.start_byte());
        let imports = statements
            .iter()
            .filter_map(|n| {
                n.utf8_text(bytes).ok().map(|text| Import {
                    statement: text.to_string(),
                    line: n.start_position().row as u32 + 1,
                })
            })
            .collect();
        let mut facts = facts;
        facts.sort_by_key(|f| f.line);
        (imports, facts)
    }
}

/// Whether `name` is a type parameter declared by an item around `node`
/// (its `type_parameters`: `<T: Clone>`, `<T extends X>`, `[T any]`).
fn is_type_parameter(node: Node<'_>, name: &str, bytes: &[u8]) -> bool {
    if name.contains(['.', ':']) {
        return false;
    }
    let mut cur = node.parent();
    while let Some(n) = cur {
        if let Some(params) = n.child_by_field_name("type_parameters") {
            let mut cursor = params.walk();
            let declared = params
                .named_children(&mut cursor)
                .any(|p| first_name(p, bytes).as_deref() == Some(name));
            if declared {
                return true;
            }
        }
        cur = n.parent();
    }
    false
}

/// The first identifier under `node` (itself included): the name a type
/// parameter declares.
fn first_name(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    if matches!(node.kind(), "type_identifier" | "identifier") {
        return node.utf8_text(bytes).ok().map(str::to_string);
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    children.into_iter().find_map(|c| first_name(c, bytes))
}

/// A string literal's content (`"net/http"` → `net/http`).
fn unquote(t: &str) -> &str {
    t.trim_matches(|c| c == '"' || c == '\'' || c == '`')
}

/// The outermost type-name nodes under `node` (itself included): each is one
/// reference. A type name is not descended into (`a.b.Foo` is one name),
/// anything else is (`List<Foo>` is `List` and `Foo`).
fn type_name_nodes<'t>(node: Node<'t>, kinds: &[String], out: &mut Vec<Node<'t>>) {
    if kinds.iter().any(|k| k == node.kind()) {
        out.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        type_name_nodes(child, kinds, out);
    }
}

/// A type as an edge destination: the text without generic arguments,
/// references, pointers or whitespace, path kept (`com.x.Base`,
/// `fmt::Display`, `pkg.T`); `Self` is the enclosing type. `None` for
/// something that is not a name (a lifetime, a literal, an expression).
fn type_ref_name(node: Node<'_>, bytes: &[u8], lang: Lang) -> Option<String> {
    let text = node.utf8_text(bytes).ok()?;
    let mut t: &str = text.trim();
    loop {
        let stripped = t
            .strip_prefix('&')
            .or_else(|| t.strip_prefix('*'))
            .or_else(|| t.strip_prefix("mut "))
            .or_else(|| t.strip_prefix("dyn "))
            .or_else(|| t.strip_prefix("[]"))
            .map(str::trim_start);
        match stripped {
            Some(rest) => t = rest,
            None => break,
        }
    }
    let end = t.find(['<', '[', '(']).unwrap_or(t.len());
    let name: String = t[..end].chars().filter(|c| !c.is_whitespace()).collect();
    if name == "Self" {
        return enclosing_container(node, lang.container_kinds())
            .and_then(|c| container_name(c, bytes));
    }
    let ok = !name.is_empty()
        && name
            .split("::")
            .flat_map(|seg| seg.split('.'))
            .all(nameable);
    ok.then_some(name)
}

/// The edge target of a path call (Rust `Foo::bar()`, `Self::new()`,
/// `std::fs::read()`): `Type.callee` when the path ends in a type (or
/// `Self`, the enclosing type), so it matches the method's qualified name;
/// otherwise the module path, `path::callee`. A path that is not a plain
/// run of names (`<T as Trait>::f`, `Vec::<u8>::new`) falls back to the
/// bare callee.
fn path_target(callee: Node<'_>, path: Node<'_>, name: &str, bytes: &[u8], lang: Lang) -> String {
    let text: String = path
        .utf8_text(bytes)
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let segments: Vec<&str> = text.split("::").collect();
    if segments.is_empty() || !segments.iter().all(|s| nameable(s)) {
        return name.to_string();
    }
    let last = segments[segments.len() - 1];
    if last == "Self" {
        return match enclosing_container(callee, lang.container_kinds())
            .and_then(|c| container_name(c, bytes))
        {
            Some(ty) => format!("{ty}.{name}"),
            None => name.to_string(),
        };
    }
    if last.chars().next().is_some_and(char::is_uppercase) {
        return format!("{last}.{name}");
    }
    format!("{text}::{name}")
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

/// The source of what `node` does: the byte where it starts and the name
/// `graph_edges` records for it.
///
/// The nearest enclosing callable (a node of the language's
/// `function_kinds`) that is a **symbol**, named by its qualified name (every
/// enclosing scope: `traced.wrapper`, `OrderService.init.run`, `C.m.f`, Go
/// `Server.Handle`), from the symbol's start:
///
/// - one whose `name` is a symbol's name node (a function, a method, a
///   constructor);
/// - else one bound to a symbol — the `const x = () => …`, the `const x =
///   function named() {…}` (the symbol is `x`, not `named`) or the class
///   field `x = () => …` the definitions query made one (`C.m.cb` for a
///   `const` inside a method).
///
/// Any other callable is walked past, so the call inside belongs to the
/// function that wrote it: a callback (`.map(x => f(x))`, `function
/// named() {}` passed as an argument), an arrow bound to an object key
/// (`subscribe({ next: r => … })`), to a member (`this.onTick = () => …`,
/// `exports.handler = function …`). Their names are no symbols, and a source
/// that names no symbol loses the caller to whoever reads `graph_edges` by
/// name (`impact_analysis`, `get_references`).
fn enclosing_source(node: Node<'_>, lang: Lang, bound: &Bound) -> Option<(usize, String)> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if lang.function_kinds().iter().any(|k| k == n.kind()) {
            let own = n
                .child_by_field_name("name")
                .and_then(|m| bound.get(&m.id()));
            let held = || binding_of(n).and_then(|b| bound.get(&b.id()));
            if let Some((q, start)) = own.or_else(held) {
                return Some((*start, q.clone()));
            }
        }
        cur = n.parent();
    }
    None
}

/// What an anonymous callable is bound to: the variable of `const x = () =>
/// …`, the field of `x = () => …` in a class, the key of `{ x: () => … }`,
/// the left side of `a.b = () => …`. Whether that binding is a symbol is
/// [`Bound`]'s to say.
fn binding_of(func: Node<'_>) -> Option<Node<'_>> {
    let parent = func.parent()?;
    match parent.kind() {
        "variable_declarator" | "public_field_definition" => parent.child_by_field_name("name"),
        "field_definition" => parent.child_by_field_name("property"),
        "pair" => parent.child_by_field_name("key"),
        "assignment_expression" => parent.child_by_field_name("left"),
        _ => None,
    }
}

/// What a call's receiver is, as far as the file can tell (PLAN-009
/// TASK-005, DD-7 rules 1-4): what the `edges` row keeps as its `hint` for
/// the link pass, and what makes the `graph_edges` target.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Recv {
    /// No receiver: `foo()`.
    Bare,
    /// TypeScript/JavaScript: no receiver, and no scope binds the name (an
    /// import, a global, an undeclared name).
    Free,
    /// `this`/`self`/`Self`/`cls`.
    This,
    /// `super`.
    Super,
    /// A local, parameter or field whose type the scope knows (or `new
    /// T()`, `(T) e`): the type as written and how it was found.
    Typed(String, Via),
    /// A name no scope binds: a type (`Office.findByCode`), a package, an
    /// inherited field, a lambda parameter.
    Name(String),
    /// A declaration whose type nothing local says (`var q =
    /// em.createQuery(…)`): never taken for a type.
    Untyped,
    /// Another call (`a.b().c()`): the previous callee and its receiver,
    /// up to [`CHAIN_DEPTH`] calls back.
    Chain(Option<(String, Box<Recv>)>),
    /// Any other expression.
    Expr,
    /// A dotted receiver whose first segment a scope binds (`target.parent`,
    /// `this.cfg.server`): the fields after it, and what types the first.
    Member(Vec<String>, Box<Recv>),
    /// A bare or `this` call inside an anonymous class (`new TimerTask() {
    /// … cancel() }`): the class it extends first, then the enclosing scopes.
    Anon(String, Box<Recv>),
}

impl Recv {
    /// The `graph_edges` target (the format 0.9.0 reads, DD-2): `Type.callee`
    /// for a typed receiver, `Container.callee` for `this`/`super`, a
    /// capitalised name that is no constant (`Office.find`, never
    /// `LOG.info`), else the bare callee.
    fn target(&self, callee: Node<'_>, name: &str, bytes: &[u8], lang: Lang) -> String {
        match self {
            Recv::This | Recv::Super => enclosing_container(callee, lang.container_kinds())
                .and_then(|c| container_name(c, bytes))
                .map(|class| format!("{class}.{name}"))
                .unwrap_or_else(|| name.to_string()),
            // What a call returns (`x = make()`): its type is the link
            // pass's to find.
            Recv::Typed(ty, _) if ty.ends_with(')') || ty.ends_with('?') => name.to_string(),
            Recv::Typed(ty, _) => format!("{ty}.{name}"),
            Recv::Name(r)
                if r.chars().next().is_some_and(char::is_uppercase) && !is_constant_name(r) =>
            {
                format!("{r}.{name}")
            }
            _ => name.to_string(),
        }
    }

    /// The `edges.hint` (PLAN-009 DD-6): space-separated, what the link pass
    /// re-resolves the occurrence from without the source — `bare`, `this`,
    /// `super`, `typed <via> <Type>`, `name <receiver>`, `untyped`, `expr`,
    /// `chain [<previous callee> <its hint>]`, `member <a.b> <hint of the
    /// first segment>`, `anon <Type> <bare|this>` (`path` for a Rust path call);
    /// the parser appends ` /<arguments>`.
    fn hint(&self) -> String {
        match self {
            Recv::Bare => "bare".into(),
            Recv::Free => "free".into(),
            Recv::This => "this".into(),
            Recv::Super => "super".into(),
            Recv::Typed(ty, via) => format!("typed {} {ty}", via.as_str()),
            Recv::Name(r) => format!("name {r}"),
            Recv::Untyped => "untyped".into(),
            Recv::Expr => "expr".into(),
            Recv::Chain(None) => "chain".into(),
            Recv::Chain(Some((prev, recv))) => format!("chain {prev} {}", recv.hint()),
            Recv::Member(path, base) => format!("member {} {}", path.join("."), base.hint()),
            Recv::Anon(ty, inner) => format!("anon {ty} {}", inner.hint()),
        }
    }
}

/// How many calls back a chain's receiver is followed (`a.b().c().d()`):
/// the link pass types each from the one before (DD-7).
const CHAIN_DEPTH: u8 = 4;

/// Node kinds of a call whose value can be a receiver.
const CALL_KINDS: &[&str] = &["method_invocation", "call_expression", "call"];

/// The receiver of the call whose callee is `callee`.
fn classify(callee: Node<'_>, bytes: &[u8], scopes: &Scopes, depth: u8) -> Recv {
    let recv = match receiver_node(callee) {
        Some(recv) => classify_receiver(recv, bytes, scopes, depth),
        // TypeScript/JavaScript: `next(req)` with `next` a parameter or a
        // local holds a value nothing types, never a function of that name;
        // a function in scope is `bare`; a name no scope binds (an import, a
        // global) is `free`, so a function bound in a sibling block is not
        // taken for it.
        None if !scopes.implicit_this() => {
            match callee
                .utf8_text(bytes)
                .ok()
                .and_then(|n| scopes.lookup(callee, n))
            {
                // The module's own function: looked up as such, never as a
                // homonym bound in an enclosing function's block.
                Some(b) if b.func && scopes.at_top_level(b) => Recv::Free,
                Some(b) if b.func => Recv::Bare,
                Some(_) => Recv::Untyped,
                None => Recv::Free,
            }
        }
        None => Recv::Bare,
    };
    match recv {
        Recv::Bare | Recv::This => match anonymous_supertype(callee, bytes) {
            Some(ty) => Recv::Anon(ty, Box::new(recv)),
            None => recv,
        },
        other => other,
    }
}

/// The type an anonymous class around `node` extends, if one is nearer
/// than any named class (`new TimerTask() { … }` → `TimerTask`).
fn anonymous_supertype(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "class_body" => {
                let parent = n.parent()?;
                if parent.kind() != "object_creation_expression" {
                    return None; // a named class's body
                }
                let ty = parent.child_by_field_name("type")?.utf8_text(bytes).ok()?;
                return crate::resolve::scope::TypeText::parse(ty)
                    .map(|t| t.base)
                    .filter(|t| nameable(t));
            }
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration" => return None,
            _ => {}
        }
        cur = n.parent();
    }
    None
}

fn classify_receiver(recv: Node<'_>, bytes: &[u8], scopes: &Scopes, depth: u8) -> Recv {
    let text = |n: Node<'_>| n.utf8_text(bytes).unwrap_or_default().to_string();
    match recv.kind() {
        // TypeScript/JavaScript: `this` is the class only in a class's
        // method or field; in an object literal's method, a class
        // expression, a `function` or the module it is something else.
        "this" if !scopes.implicit_this() && !this_is_a_class(recv) => return Recv::Untyped,
        "this" | "self" => return Recv::This,
        "super" => return Recv::Super,
        "parenthesized_expression" => {
            return match recv.named_child(0) {
                Some(inner) => classify_receiver(inner, bytes, scopes, depth),
                None => Recv::Expr,
            }
        }
        "cast_expression" | "object_creation_expression" | "new_expression" => {
            let ty = recv
                .child_by_field_name("type")
                .or_else(|| recv.child_by_field_name("constructor"))
                .and_then(|t| crate::resolve::scope::TypeText::parse(&text(t)))
                .map(|t| t.base)
                .filter(|t| nameable(t));
            return ty.map_or(Recv::Expr, |t| Recv::Typed(t, Via::Local));
        }
        k if CALL_KINDS.contains(&k) => {
            // Python `super().m()`: the supertype's.
            if recv
                .child_by_field_name("function")
                .is_some_and(|f| f.kind() == "identifier" && f.utf8_text(bytes) == Ok("super"))
            {
                return Recv::Super;
            }
            if depth >= CHAIN_DEPTH {
                return Recv::Chain(None);
            }
            let prev = inner_callee(recv)
                .and_then(|c| c.utf8_text(bytes).ok().map(|n| (c, n.to_string())))
                .filter(|(_, n)| nameable(n));
            return Recv::Chain(
                prev.map(|(c, n)| (n, Box::new(classify(c, bytes, scopes, depth + 1)))),
            );
        }
        // A literal is of its type (`"done".equals(x)`, `Foo.class`, Python
        // `", ".join(…)`).
        k => {
            if let Some(t) = scopes.resolver().literal_type(k) {
                return Recv::Typed(t.into(), Via::Local);
            }
        }
    }
    let t = text(recv);
    if !nameable(&t) {
        return Recv::Expr;
    }
    match t.as_str() {
        "self" | "Self" | "this" | "cls" => return Recv::This,
        "super" => return Recv::Super,
        _ => {}
    }
    let member = t.strip_prefix("self.").or_else(|| t.strip_prefix("this."));
    let found = match member {
        Some(m) => scopes.field(recv, m),
        None => scopes.lookup(recv, &t),
    };
    let key = member.unwrap_or(&t);
    let typed = |b: &crate::resolve::scope::Binding| match &b.ty {
        Some(ty) if type_token(&ty.base) => Recv::Typed(ty.base.clone(), b.via),
        _ => Recv::Untyped,
    };
    if let Some(b) = found {
        // A function or class bound to the name (TypeScript/JavaScript):
        // the name itself, as the file wrote it (`Klass.make()`,
        // `fn.call()`), never an untyped value.
        if b.func {
            return Recv::Name(key.to_string());
        }
        return typed(b);
    }
    // `target.parent`, `this.cfg.server`: a binding, then its fields — never
    // a package path (PLAN-009 TASK-005 review).
    if let Some((first, rest)) = key.split_once('.') {
        let base = match member {
            Some(_) => scopes.field(recv, first),
            None => scopes.lookup(recv, first),
        };
        let path: Vec<String> = rest.split('.').map(str::to_string).collect();
        match base {
            Some(b) => return Recv::Member(path, Box::new(typed(b))),
            // Rust, Go, Python: the fields after `self`, typed by the link
            // pass from their declarations (TASK-007).
            None if member.is_some() && scopes.resolver().fields_by_link() => {
                return Recv::Member(
                    key.split('.').map(str::to_string).collect(),
                    Box::new(Recv::This),
                )
            }
            // `this.x.y` with `x` declared elsewhere (a supertype).
            None if member.is_some() => {
                return Recv::Member(path, Box::new(Recv::Name(first.to_string())))
            }
            None => {}
        }
    }
    if member.is_some() && scopes.resolver().fields_by_link() {
        return Recv::Member(vec![key.to_string()], Box::new(Recv::This));
    }
    Recv::Name(key.to_string())
}

/// Whether a TypeScript/JavaScript `this` at `node` is the instance of a
/// declared class: the nearest scope that binds `this` (arrows do not) is a
/// method or a field of a `class` declaration's body.
fn this_is_a_class(node: Node<'_>) -> bool {
    let declared = |body: Node<'_>| {
        body.kind() == "class_body"
            && body.parent().is_some_and(|c| {
                matches!(c.kind(), "class_declaration" | "abstract_class_declaration")
            })
    };
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "arrow_function" => {}
            "method_definition" => return n.parent().is_some_and(declared),
            "class_body" => return declared(n),
            "function_declaration"
            | "function_expression"
            | "generator_function_declaration"
            | "generator_function"
            | "program" => return false,
            _ => {}
        }
        cur = n.parent();
    }
    false
}

/// The callee name node of a call node (`a.b(…)` → `b`, `f(…)` → `f`).
fn inner_callee(call: Node<'_>) -> Option<Node<'_>> {
    if let Some(name) = call.child_by_field_name("name") {
        return Some(name); // Java `method_invocation`
    }
    let func = call.child_by_field_name("function")?;
    for field in ["property", "field", "attribute"] {
        if let Some(n) = func.child_by_field_name(field) {
            return Some(n);
        }
    }
    matches!(func.kind(), "identifier").then_some(func)
}

/// How many arguments the call of `callee` passes, when the grammar says.
fn call_arity(callee: Node<'_>) -> Option<usize> {
    let mut cur = callee.parent();
    for _ in 0..2 {
        let n = cur?;
        if let Some(args) = n.child_by_field_name("arguments") {
            let mut c = args.walk();
            let count = args
                .named_children(&mut c)
                .filter(|a| !a.kind().contains("comment"))
                .count();
            return Some(count);
        }
        cur = n.parent();
    }
    None
}

/// The receiver node of a member/method call, if this is one.
fn receiver_node(callee: Node<'_>) -> Option<Node<'_>> {
    let parent = callee.parent()?;
    let field = match parent.kind() {
        "attribute" | "member_expression" | "method_invocation" => "object",
        "selector_expression" => "operand",
        "field_expression" => "value",
        _ => return None,
    };
    parent.child_by_field_name(field)
}

/// Can this receiver text stand for something a person could look up?
///
/// The receiver node of a chained call is the entire expression before the dot.
/// In reactive Java that is routine, and it put graph nodes like
/// `Office.findByCode(code).flatMap` — and three-line ones with a lambda
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

/// A receiver's type as a hint carries it: a name (`Foo`, `a.b.Foo`), or
/// what calling one returns (`make()`, Rust `Store.open()?`).
fn type_token(ty: &str) -> bool {
    let base = ty.strip_suffix('?').unwrap_or(ty);
    let base = base.strip_suffix("()").unwrap_or(base);
    nameable(base)
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
/// the shape of that qualifier — one entry per scope, `f` for a callable (a
/// node of the language's `function_kinds`), `s` for anything else — `None`
/// when no scope is callable. In a language with overloads (Java) a
/// callable's entry carries its parameter types, `f(int)`: two constructors
/// `O()` and `O(int)`, each with an anonymous class's `run`, are two scopes.
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
    // One entry per scope, so the order can be reversed with the chain.
    let mut shape: Vec<String> = Vec::new();
    if let Some(recv) = go_receiver_type(def, bytes) {
        chain.push(recv);
        shape.push("s".into());
    }
    let mut cur = def.parent();
    while let Some(n) = cur {
        let name = if lang.container_kinds().iter().any(|k| k == n.kind()) {
            container_name(n, bytes)
        } else if def_ids.contains(&n.id()) || lang.scope_kinds().iter().any(|k| k == n.kind()) {
            n.child_by_field_name("name")
                .or_else(|| n.child_by_field_name("key"))
                // A JS class field (`field_definition`) names itself so.
                .or_else(|| n.child_by_field_name("property"))
                .and_then(|c| c.utf8_text(bytes).ok())
                .filter(|t| nameable(t))
                .map(str::to_string)
        } else {
            None
        };
        if let Some(name) = name {
            chain.push(name);
            let callable = lang.function_kinds().iter().any(|k| k == n.kind());
            let entry = if !callable {
                "s".to_string()
            } else if let Some(params) = lang.overloads().then(|| param_types(n, bytes)).flatten() {
                // Overloads of one name are separate scopes: `O()` and
                // `O(int)` each with an anonymous `run` are not one
                // `O.O.run` told apart by source order.
                format!("f({params})")
            } else {
                "f".to_string()
            };
            shape.push(entry);
        }
        cur = n.parent();
    }
    chain.reverse();
    shape.reverse();
    let shape = shape
        .iter()
        .any(|e| e.starts_with('f'))
        .then(|| shape.concat());
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

/// The trait of an `impl` node, whitespace and path removed, generic
/// arguments kept (`From<A>` and `From<B>` are two impls; `fmt::Display` is
/// `Display`): the disambiguator of the `impl` symbol itself and, for what
/// sits in it, of its members — `impl Display for X { fn fmt }` and `impl
/// Debug for X { fn fmt }` are both `X.fmt`.
fn trait_text(imp: Node<'_>, bytes: &[u8]) -> Option<String> {
    let tr = tidy_generics(&uncommented(imp.child_by_field_name("trait")?, bytes));
    let args = tr.find('<').unwrap_or(tr.len());
    let name = last_path_segment(&tr[..args]);
    if name.is_empty() {
        return Some(tr);
    }
    Some(format!("{name}{}", &tr[args..]))
}

/// The self type's generic arguments and the `where` clause of an `impl`
/// node, without comments, whitespace or trailing commas ([`tidy_generics`]: `impl
/// Foo<u8>` → `<u8>`, `impl<T> W<T> where T: Copy` → `<T>whereT:Copy`,
/// also as `rustfmt` lays it out vertically); `None` for a non-generic one, or a node
/// that is not an `impl`. Part of the id's disambiguator (DD-3), beside the
/// trait: `impl Foo<u8>` and `impl Foo<u16>` are both `Foo`, and so are
/// their methods.
fn impl_self_args(imp: Node<'_>, bytes: &[u8]) -> Option<String> {
    if imp.kind() != "impl_item" {
        return None;
    }
    let compact = |n: Node<'_>| -> String { tidy_generics(&uncommented(n, bytes)) };
    let ty = imp
        .child_by_field_name("type")
        .map(compact)
        .unwrap_or_default();
    let args = ty
        .find('<')
        .map(|i| ty[i..].to_string())
        .unwrap_or_default();
    let mut cursor = imp.walk();
    let clause = imp
        .named_children(&mut cursor)
        .find(|c| c.kind() == "where_clause")
        .map(compact)
        .unwrap_or_default();
    let out = format!("{args}{clause}");
    (!out.is_empty()).then_some(out)
}

/// Generic arguments and `where` bounds as one spelling, however `rustfmt`
/// laid them out: no whitespace, no trailing comma before a `>` or at the end
/// (`Foo<\n    T,\n>` → `Foo<T>`, `where\n    T: Copy,` → `whereT:Copy`), nor
/// before a `)` closing a group of several elements (`Fn(A, B,)` → `Fn(A,B)`).
/// Part of an id's disambiguator (DD-3): a reformat is not another impl. The
/// comma of a one-element group stays: `(u8,)` is a tuple, `(u8)` is `u8`.
/// Comments are left out by the caller ([`uncommented`]).
fn tidy_generics(text: &str) -> String {
    let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    // Per open `(`/`<`: whether a comma at its depth came before.
    let mut commas: Vec<bool> = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '(' | '<' => commas.push(false),
            ',' => {
                let next = chars.get(i + 1).copied();
                let several = commas.last().copied().unwrap_or(false);
                match next {
                    Some('>') => continue,
                    Some(')') if several => continue,
                    None => continue,
                    _ => {
                        if let Some(top) = commas.last_mut() {
                            *top = true;
                        }
                    }
                }
            }
            // The `>` of `->` closes no group.
            '>' if i > 0 && chars[i - 1] == '-' => {}
            ')' | '>' => {
                commas.pop();
            }
            _ => {}
        }
        out.push(c);
    }
    while out.last() == Some(&',') {
        out.pop();
    }
    out.into_iter().collect()
}

/// The text of `node` without the comments inside it (`where // sorted\n
/// T: Copy` is `where T: Copy`): editing a comment is not another impl.
fn uncommented(node: Node<'_>, bytes: &[u8]) -> String {
    fn comments(n: Node<'_>, out: &mut Vec<(usize, usize)>) {
        if n.kind().contains("comment") {
            out.push((n.start_byte(), n.end_byte()));
            return;
        }
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            comments(c, out);
        }
    }
    let mut skip = Vec::new();
    comments(node, &mut skip);
    let mut text = Vec::with_capacity(node.end_byte() - node.start_byte());
    let mut at = node.start_byte();
    for (s, e) in skip {
        text.extend_from_slice(&bytes[at..s]);
        text.push(b' ');
        at = e;
    }
    text.extend_from_slice(&bytes[at..node.end_byte()]);
    String::from_utf8_lossy(&text).into_owned()
}

/// When `def` declares several names, each in its own declarator (TS/JS
/// `const a = …, b = …`, Java `int a, b;`): the declarator of `name` and
/// whether it is the first one. `None` for a single declarator.
fn own_declarator<'t>(def: Node<'t>, name: Node<'t>) -> Option<(Node<'t>, bool)> {
    let mut cursor = def.walk();
    let declarators: Vec<Node<'t>> = def
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "variable_declarator")
        .collect();
    if declarators.len() < 2 {
        return None;
    }
    let at = declarators
        .iter()
        .position(|d| d.start_byte() <= name.start_byte() && name.end_byte() <= d.end_byte())?;
    Some((declarators[at], at == 0))
}

/// Where the body of a declarator's function value starts (`b = () => {…}`).
fn declarator_body(decl: Node<'_>) -> Option<usize> {
    let v = decl.child_by_field_name("value")?;
    matches!(v.kind(), "arrow_function" | "function_expression")
        .then(|| v.child_by_field_name("body").map(|b| b.start_byte()))
        .flatten()
}

/// Longest signature kept, in characters.
const SIGNATURE_MAX: usize = 200;

/// The definition up to its body, whitespace collapsed, capped at
/// [`SIGNATURE_MAX`] characters (DD-17): `fn g() -> i32 { 1 }` is
/// `fn g() -> i32`, a multi-line parameter list is one line. The body is
/// the `body` field; for a function bound to a name (`const x = (a) => …`)
/// the function's body; for a Go type, its `{`. A definition with no body
/// (a field, a constant) is whole.
fn signature_of(def: Node<'_>, bytes: &[u8]) -> String {
    let end = body_start(def, bytes).unwrap_or(def.end_byte());
    signature_text(bytes, def.start_byte(), end)
}

/// `bytes[start..end]` as a signature: whitespace collapsed, capped.
fn signature_text(bytes: &[u8], start: usize, end: usize) -> String {
    let text = std::str::from_utf8(&bytes[start..end]).unwrap_or_default();
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("( ", "(")
        .replace(", )", ")")
        .replace(" )", ")")
        .chars()
        .take(SIGNATURE_MAX)
        .collect()
}

/// Where the body of `def` starts, if it has one (see [`signature_of`]).
fn body_start(def: Node<'_>, bytes: &[u8]) -> Option<usize> {
    if let Some(body) = def.child_by_field_name("body") {
        return Some(body.start_byte());
    }
    // `const x = () => …`, `x = function () {…}` in a class.
    let mut cursor = def.walk();
    let declarators: Vec<Node<'_>> = def
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "variable_declarator")
        .collect();
    let value = declarators
        .first()
        .and_then(|d| d.child_by_field_name("value"))
        .or_else(|| def.child_by_field_name("value"));
    if let Some(v) = value {
        if matches!(v.kind(), "arrow_function" | "function_expression") {
            return v.child_by_field_name("body").map(|b| b.start_byte());
        }
    }
    if def.kind() == "type_spec" {
        let text = &bytes[def.start_byte()..def.end_byte()];
        return text
            .iter()
            .position(|b| *b == b'{')
            .map(|i| def.start_byte() + i);
    }
    None
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

#[cfg(test)]
mod tests {
    use super::tidy_generics;

    #[test]
    fn tidy_generics_drops_layout_not_meaning() {
        assert_eq!(tidy_generics("Foo<\n    T,\n>"), "Foo<T>");
        assert_eq!(
            tidy_generics("where\n    T: Copy,\n    U: Clone,"),
            "whereT:Copy,U:Clone"
        );
        assert_eq!(tidy_generics("<A, B<C,>,>"), "<A,B<C>>");
        // A one-element tuple keeps its comma: it is not the bare type.
        assert_eq!(tidy_generics("Foo<(u8,)>"), "Foo<(u8,)>");
        assert_ne!(tidy_generics("Foo<(u8,)>"), tidy_generics("Foo<(u8)>"));
        // A group of several elements folds its trailing comma.
        assert_eq!(
            tidy_generics("whereF: Fn(\n    A,\n    B,\n),"),
            "whereF:Fn(A,B)"
        );
        assert_eq!(
            tidy_generics("Foo<(\n    u8,\n    u16,\n)>"),
            "Foo<(u8,u16)>"
        );
        // The `>` of `->` closes nothing.
        assert_eq!(
            tidy_generics("whereF:Fn(A, Box<dyn Fn() -> u8>,)"),
            "whereF:Fn(A,Box<dynFn()->u8>)"
        );
    }
}
