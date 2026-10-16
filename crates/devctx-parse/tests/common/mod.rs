//! Shared helpers for the per-language fixture tests (PLAN-009 TASK-004).

#![allow(dead_code)] // each test file uses its own subset

use std::collections::HashMap;

pub mod linked;

use devctx_parse::{parse, Lang, ParsedFile, Symbol};

/// Parse `tests/fixtures/<fixture>` as if it lived at `path` in repo `repo`.
pub fn facts(lang: Lang, fixture: &str, path: &str) -> ParsedFile {
    let src = std::fs::read_to_string(format!(
        "{}/tests/fixtures/{fixture}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("{fixture}: {e}"));
    let mut pf = parse(lang, &src).unwrap();
    pf.assign_ids("repo", path);
    pf
}

/// The symbol with this qualified name (and kind, when several share it).
pub fn sym<'a>(pf: &'a ParsedFile, kind: &str, qualified: &str) -> &'a Symbol {
    pf.symbols
        .iter()
        .find(|s| s.kind == kind && s.qualified == qualified)
        .unwrap_or_else(|| panic!("no {kind} {qualified} in {:#?}", outline(pf)))
}

/// `kind qualified` of every symbol, in source order.
pub fn outline(pf: &ParsedFile) -> Vec<String> {
    pf.symbols
        .iter()
        .map(|s| format!("{} {}", s.kind, s.qualified))
        .collect()
}

/// id → qualified name, file symbol included.
pub fn names(pf: &ParsedFile) -> HashMap<u64, String> {
    std::iter::once(&pf.file_symbol)
        .chain(&pf.symbols)
        .map(|s| (s.id, s.qualified.clone()))
        .collect()
}

/// The parent's qualified name of the symbol.
pub fn parent_of(pf: &ParsedFile, s: &Symbol) -> String {
    names(pf)[&s.parent_id.expect("every symbol has a parent")].clone()
}

/// `(kind, name, source symbol)` of every instantiation and type use.
pub fn refs(pf: &ParsedFile) -> Vec<(String, String, String)> {
    let n = names(pf);
    pf.facts
        .refs
        .iter()
        .map(|r| (r.kind.clone(), r.name.clone(), n[&r.src_id].clone()))
        .collect()
}

/// `(kind, supertype, defining symbol)` of every supertype.
pub fn inherits(pf: &ParsedFile) -> Vec<(String, String, String)> {
    let n = names(pf);
    pf.facts
        .inherits
        .iter()
        .map(|f| (f.kind.clone(), f.name.clone(), n[&f.src_id].clone()))
        .collect()
}

/// The `imports` edge destinations, in order.
pub fn import_targets(pf: &ParsedFile) -> Vec<String> {
    pf.facts.imports.iter().map(|i| i.target.clone()).collect()
}

/// `(source symbol, target)` of every call, module level included.
pub fn calls(pf: &ParsedFile) -> Vec<(String, String)> {
    let n = names(pf);
    pf.edges
        .iter()
        .chain(&pf.module_edges)
        .map(|e| (n[&e.src_id].clone(), e.target.clone()))
        .collect()
}

/// Owned triple, for comparing against literals.
pub fn t3(a: &str, b: &str, c: &str) -> (String, String, String) {
    (a.into(), b.into(), c.into())
}

/// Owned pair, for comparing against literals.
pub fn t2(a: &str, b: &str) -> (String, String) {
    (a.into(), b.into())
}
