//! NO EDITAR. Espejo de `crates/devctx-parse/src/parser.rs` y de las claves
//! `calls`, `types`, `function_kinds` y `container_kinds` de
//! `crates/devctx-parse/languages/*.json` en v0.9.0
//! (tag `v0.9.0` = commit fdc7dd21cb617e257e3a79b1a9cfd5fb086b8524).
//!
//! What it is for: `graph_edges` is what a downgraded 0.9.0 reads, by name, with
//! the walk it always had. The only way for that walk to find every caller the
//! 0.9.0 original found is for the table to hold the rows 0.9.0 wrote, row for
//! row — its false positives included. So those rows are produced here by
//! 0.9.0's own code, copied as it was, over the tree the current parser has
//! already built (PLAN-009 TASK-019). The tree-sitter grammars are the ones
//! v0.9.0 locked (same versions in `Cargo.lock`).
//!
//! The copied functions are verbatim. What is not: `Lang` and `GraphEdge` are
//! local stand-ins with the fields and methods those functions use (the crate's
//! own types moved on), the queries and kinds are constants instead of JSON
//! (frozen in code, so the extractor fingerprint does not move), and
//! [`LegacyV090::edges`] is `parse`'s call of `extract_type_bindings` +
//! `extract_edges` without the rest of `parse`. Nothing else may change here; a
//! change of behaviour is a change of what a downgraded 0.9.0 sees.

use std::collections::HashMap;
use std::sync::LazyLock;

use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

/// Variable/field name → declared type, for receiver resolution.
type TypeMap = HashMap<String, String>;

/// A `graph_edges` row as 0.9.0's parser produced it (its `GraphEdge`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdge {
    /// Enclosing symbol making the call.
    pub source: String,
    /// Called symbol (bare callee name).
    pub target: String,
    /// Edge kind (`calls`).
    pub kind: String,
    /// 1-based line of the call.
    pub line: u32,
}

/// The per-language definition 0.9.0 read from `languages/<lang>.json`, as far
/// as its edge extraction used it.
struct Def {
    key: &'static str,
    calls: &'static str,
    types: Option<&'static str>,
    function_kinds: &'static [&'static str],
    container_kinds: &'static [&'static str],
}

const DEFS: &[Def] = &[
    Def {
        key: "go",
        calls: "(call_expression function: (identifier) @callee)\n(call_expression function: (selector_expression field: (field_identifier) @callee))",
        types: Some("(var_spec name: (identifier) @name type: (type_identifier) @type)\n(parameter_declaration name: (identifier) @name type: (type_identifier) @type)\n(field_declaration name: (field_identifier) @name type: (type_identifier) @type)"),
        function_kinds: &["function_declaration", "method_declaration"],
        container_kinds: &[],
    },
    Def {
        key: "java",
        calls: "(method_invocation name: (identifier) @callee)",
        types: Some("(field_declaration type: (type_identifier) @type\n    declarator: (variable_declarator name: (identifier) @name))\n(local_variable_declaration type: (type_identifier) @type\n    declarator: (variable_declarator name: (identifier) @name))\n(formal_parameter type: (type_identifier) @type name: (identifier) @name)"),
        function_kinds: &["method_declaration", "constructor_declaration"],
        container_kinds: &["class_declaration", "interface_declaration", "enum_declaration", "record_declaration"],
    },
    Def {
        key: "javascript",
        calls: "(call_expression function: (identifier) @callee)\n(call_expression function: (member_expression property: (property_identifier) @callee))",
        types: None,
        function_kinds: &["function_declaration", "method_definition"],
        container_kinds: &["class_declaration"],
    },
    Def {
        key: "python",
        calls: "(call function: (identifier) @callee)\n(call function: (attribute attribute: (identifier) @callee))",
        types: Some("(assignment left: (identifier) @name type: (type (identifier) @type))\n(typed_parameter (identifier) @name type: (type (identifier) @type))"),
        function_kinds: &["function_definition"],
        container_kinds: &["class_definition"],
    },
    Def {
        key: "rust",
        calls: "(call_expression function: (identifier) @callee)\n(call_expression function: (field_expression field: (field_identifier) @callee))\n(call_expression function: (scoped_identifier name: (identifier) @callee))",
        types: Some("(let_declaration pattern: (identifier) @name type: (type_identifier) @type)\n(parameter pattern: (identifier) @name type: (type_identifier) @type)\n(field_declaration name: (field_identifier) @name type: (type_identifier) @type)"),
        function_kinds: &["function_item"],
        container_kinds: &["impl_item", "trait_item"],
    },
    Def {
        key: "tsx",
        calls: "(call_expression function: (identifier) @callee)\n(call_expression function: (member_expression property: (property_identifier) @callee))",
        types: Some("(public_field_definition name: (property_identifier) @name\n    type: (type_annotation (type_identifier) @type))\n(variable_declarator name: (identifier) @name\n    type: (type_annotation (type_identifier) @type))\n(required_parameter pattern: (identifier) @name\n    type: (type_annotation (type_identifier) @type))"),
        function_kinds: &["function_declaration", "method_definition"],
        container_kinds: &["class_declaration", "interface_declaration", "enum_declaration"],
    },
    Def {
        key: "typescript",
        calls: "(call_expression function: (identifier) @callee)\n(call_expression function: (member_expression property: (property_identifier) @callee))",
        types: Some("(public_field_definition name: (property_identifier) @name\n    type: (type_annotation (type_identifier) @type))\n(variable_declarator name: (identifier) @name\n    type: (type_annotation (type_identifier) @type))\n(required_parameter pattern: (identifier) @name\n    type: (type_annotation (type_identifier) @type))"),
        function_kinds: &["function_declaration", "method_definition"],
        container_kinds: &["class_declaration", "interface_declaration", "enum_declaration"],
    },
];

/// The stand-in for 0.9.0's `Lang`: what its edge functions asked of it.
#[derive(Clone, Copy)]
struct Lang(&'static LangKinds);

struct LangKinds {
    function_kinds: Vec<String>,
    container_kinds: Vec<String>,
}

/// [`DEFS`]' kinds as the `&[String]` 0.9.0's functions take, built once.
static KINDS: LazyLock<Vec<LangKinds>> = LazyLock::new(|| {
    DEFS.iter()
        .map(|d| LangKinds {
            function_kinds: d.function_kinds.iter().map(|s| s.to_string()).collect(),
            container_kinds: d.container_kinds.iter().map(|s| s.to_string()).collect(),
        })
        .collect()
});

impl Lang {
    fn function_kinds(self) -> &'static [String] {
        &self.0.function_kinds
    }

    fn container_kinds(self) -> &'static [String] {
        &self.0.container_kinds
    }
}

/// 0.9.0's edge extraction for one language, its queries compiled once.
pub struct LegacyV090 {
    lang: Lang,
    calls_query: Query,
    type_query: Option<Query>,
}

impl LegacyV090 {
    /// The 0.9.0 extraction for the language `key` (a registry key), or `None`
    /// for a language 0.9.0 did not extract edges from. A query that no
    /// longer compiles is an error: the grammar moved under the copy.
    pub fn new(
        key: &str,
        grammar: &tree_sitter::Language,
    ) -> Result<Option<Self>, tree_sitter::QueryError> {
        let Some(i) = DEFS.iter().position(|d| d.key == key) else {
            return Ok(None);
        };
        let def = &DEFS[i];
        Ok(Some(Self {
            lang: Lang(&KINDS[i]),
            calls_query: Query::new(grammar, def.calls)?,
            type_query: def.types.map(|t| Query::new(grammar, t)).transpose()?,
        }))
    }

    /// The `graph_edges` rows 0.9.0 wrote for this tree, in its order.
    pub fn edges(&self, root: Node<'_>, bytes: &[u8]) -> Vec<GraphEdge> {
        let type_map = self.extract_type_bindings(root, bytes);
        self.extract_edges(root, bytes, &type_map)
    }

    // --- verbatim from v0.9.0 parser.rs (`impl LanguageParser`) -----------

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
                let Some(source) = qualified_source(callee, bytes, self.lang) else {
                    continue; // module-level call: no source symbol.
                };
                let target = qualified_target(callee, name, bytes, type_map, self.lang);
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
}

// --- verbatim from v0.9.0 parser.rs (free functions) -----------------------

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
/// `Oficina.findByCodigo(codigo).flatMap` — and three-line ones with a lambda
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

/// Display name of a container node: its `name` field, or `type` (Rust `impl`).
fn container_name(container: Node<'_>, bytes: &[u8]) -> Option<String> {
    let name_node = container
        .child_by_field_name("name")
        .or_else(|| container.child_by_field_name("type"))?;
    name_node.utf8_text(bytes).ok().map(str::to_string)
}
