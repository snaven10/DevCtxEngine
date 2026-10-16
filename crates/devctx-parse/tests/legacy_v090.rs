//! `graph_edges` row for row as the v0.9.0 binary wrote it (PLAN-009 TASK-019).
//!
//! A downgraded 0.9.0 walks `graph_edges` by name. The only way for that walk
//! to find every caller the 0.9.0 original found is for the table to hold the
//! rows 0.9.0 wrote, its false positives included, so `ParsedFile::legacy_edges`
//! comes from 0.9.0's own code (`src/legacy_v090.rs`). This test pins it to
//! what the real binary produced: `tests/golden/legacy_v090.tsv` is the
//! `graph_edges` table the v0.9.0 binary wrote indexing `tests/fixtures/` as
//! the root of a repository — every fixture of every language, plus
//! `fixtures/legacy_v090/`, the shapes 0.10 changed (an arrow or a function
//! expression in a method, nested classes and functions, closures and lambdas,
//! a Rust `mod tests` and generic `impl`s, Rust paths, Go receivers and method
//! expressions, Java fields declared by their full name, the file-wide type
//! map of 0.9.0).
//!
//! To regenerate it: index a copy of `tests/fixtures/` (a git repository) with
//! the v0.9.0 binary and dump `graph_edges` and the code files of `file_state`
//! in the format of the header; never edit the rows by hand.

use std::collections::HashSet;

use devctx_parse::{lang_for_extension, parse};

const GOLDEN: &str = include_str!("golden/legacy_v090.tsv");

/// What the store writes for one file: the extraction's rows, deduplicated by
/// (source, target, kind) keeping the first, as `replace_file_edges` does.
fn rows_of(path: &str) -> Vec<String> {
    let src = std::fs::read_to_string(format!(
        "{}/tests/fixtures/{path}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("{path}: {e}"));
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    let lang = lang_for_extension(ext).unwrap_or_else(|| panic!("{path}: no language"));
    let pf = parse(lang, &src).unwrap();
    let mut seen = HashSet::new();
    pf.legacy_edges
        .iter()
        .filter(|e| seen.insert((e.source.clone(), e.target.clone(), e.kind.clone())))
        .map(|e| format!("{path}\t{}\t{}\t{}\t{}", e.line, e.source, e.target, e.kind))
        .collect()
}

#[test]
fn graph_edges_is_what_the_v0_9_0_binary_wrote() {
    let mut files = Vec::new();
    let mut want = Vec::new();
    for line in GOLDEN.lines().filter(|l| !l.starts_with('#')) {
        match line.strip_prefix("file\t") {
            Some(f) => files.push(f),
            None => want.push(line.to_string()),
        }
    }
    assert_eq!(files.len(), 90, "the golden covers every code fixture");
    let mut got: Vec<String> = files.iter().flat_map(|f| rows_of(f)).collect();
    // The golden's order: `ORDER BY source_file, line, source, target, kind`.
    let key = |r: &String| {
        let c: Vec<&str> = r.split('\t').collect();
        (
            c[0].to_string(),
            c[1].parse::<u32>().unwrap(),
            c[2].to_string(),
            c[3].to_string(),
            c[4].to_string(),
        )
    };
    got.sort_by_key(key);
    want.sort_by_key(key);
    let missing: Vec<&String> = want.iter().filter(|r| !got.contains(r)).collect();
    let extra: Vec<&String> = got.iter().filter(|r| !want.contains(r)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "rows v0.9.0 wrote and we do not: {missing:#?}\nrows we write and v0.9.0 did not: {extra:#?}"
    );
    assert_eq!(got, want);
}
