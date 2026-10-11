//! `skeleton` (PLAN-009 TASK-014, DD-17): the structure of one file — its
//! symbols with signature and line range, nested by container, without
//! bodies — read from the symbol graph, never from the file. For locating
//! where something is; the code to edit comes from `read_symbol` /
//! `read_file` (master §2: a skeleton is no substitute for the code).
//!
//! Pure: [`render`] draws the text from the rows the serve read.

use std::collections::HashMap;

use devctx_store::{FileImports, StoredSymbol};
use serde::{Deserialize, Serialize};

use crate::repo_map::clean_signature;

/// Import targets named in the summary line, at most.
pub const IMPORTS_SHOWN: usize = 8;

/// The least budget a skeleton is given (tokens): its header and closing
/// lines alone take up to ~100.
pub const MIN_SKELETON_BUDGET: usize = 256;

/// `skeleton`'s parameters, as the tool, the API and the CLI take them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkeletonQuery {
    /// The file: repo-relative (or absolute inside the repository), as
    /// `read_file` takes it.
    pub file: String,
    /// A first line naming the project that answered (MCP `project`): the
    /// MCP's own, never read from nor sent over the wire (a query string
    /// could otherwise put text in the answer). Local: inside the budget;
    /// routed, added by the client in front of the server's answer.
    #[serde(skip)]
    pub label: Option<String>,
}

impl SkeletonQuery {
    /// A query for `file`.
    pub fn of(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            label: None,
        }
    }

    /// Refuses a missing file, before anything is opened.
    pub fn validate(&self) -> Result<(), String> {
        if self.file.trim().is_empty() {
            return Err(
                "file is required: a repo-relative path (as read_file takes it)".to_string(),
            );
        }
        Ok(())
    }

    /// The parameters of `GET /skeleton` (`label` is not one).
    pub fn query_pairs(&self) -> Vec<(&'static str, String)> {
        vec![("file", self.file.clone())]
    }
}

/// What [`render`] draws one skeleton from.
pub struct SkeletonInput<'a> {
    /// Repo-relative path, as the index holds it.
    pub file: &'a str,
    /// The file's rows of `symbols`, in any order (the file symbol among them).
    pub symbols: &'a [StoredSymbol],
    /// The file's imports, summarized.
    pub imports: &'a FileImports,
    /// Lines that go first (which project answered).
    pub head: &'a [String],
    /// Lines that go last (branch fallback).
    pub tail: &'a [String],
}

/// The skeleton of `input` within `budget` tokens (0 = no cap; else at
/// least [`MIN_SKELETON_BUDGET`]).
pub fn render(input: &SkeletonInput<'_>, budget: usize) -> String {
    let budget = match budget {
        0 => None,
        b => Some(b.max(MIN_SKELETON_BUDGET)),
    };
    let is_root = |s: &StoredSymbol| s.kind == FILE_KIND && s.parent_id.is_none();
    let root = input.symbols.iter().find(|s| is_root(s));
    let nodes: Vec<&StoredSymbol> = input.symbols.iter().filter(|s| !is_root(s)).collect();
    let test_file = root.is_some_and(|f| f.is_test);
    // The last line any symbol reaches: the file symbol's (its end is the
    // parser's, one past a final newline), for the closing line's bound.
    let lines = root
        .map(|f| f.end_line)
        .into_iter()
        .chain(nodes.iter().map(|s| s.end_line))
        .max()
        .unwrap_or(0);

    let mut fixed: Vec<String> = input.head.to_vec();
    fixed.push(format!(
        "{}: {} symbols{}",
        input.file,
        nodes.len(),
        if test_file { ", test file" } else { "" }
    ));
    let mut imports_at = None;
    if let Some(line) = imports_line(input.imports) {
        imports_at = Some(fixed.len());
        fixed.push(line);
    }
    let order = tree_order(&nodes);
    let body: Vec<String> = order
        .iter()
        .map(|&(i, depth)| {
            let s = nodes[i];
            format!(
                "{}{} [{}-{}]{}",
                INDENT.repeat(depth),
                shown_signature(s),
                s.start_line,
                s.end_line,
                if s.is_test && !test_file {
                    " [test]"
                } else {
                    ""
                }
            )
        })
        .collect();

    // Characters, not tokens: a text of `c` characters is `ceil(c / 4)`
    // tokens, so it is within the budget exactly when `c <= 4 * budget`.
    let chars = |l: &String| l.chars().count() + 1;
    let body_chars: usize = body.iter().map(chars).sum();
    let room = budget.map(|b| b.saturating_mul(CHARS_PER_TOKEN));
    // The closing line at its longest (every symbol cut, the last line of
    // the file), reserved whenever not everything fits.
    let worst = closing(nodes.len(), lines, lines, budget.unwrap_or(0));
    let worst_chars = worst.chars().count() + 1;
    let mut fixed_chars: usize = fixed.iter().chain(input.tail).map(chars).sum();
    // The imports line is the one fixed line that can give way: to its
    // count when the fixed lines and the closing one leave no room (review
    // of TASK-014, MINOR 4), and out if even that does not fit.
    if let (Some(room), Some(at)) = (room, imports_at) {
        if fixed_chars + worst_chars > room {
            let short = format!("imports ({})", imports_total(input.imports));
            fixed_chars = fixed_chars - chars(&fixed[at]) + chars(&short);
            fixed[at] = short;
            if fixed_chars + worst_chars > room {
                fixed_chars -= chars(&fixed[at]);
                fixed.remove(at);
            }
        }
    }
    let shown = match room {
        Some(room) if fixed_chars + body_chars > room => {
            let mut left = room.saturating_sub(fixed_chars + worst_chars);
            body.iter()
                .take_while(|l| match left.checked_sub(chars(l)) {
                    Some(rest) => {
                        left = rest;
                        true
                    }
                    None => false,
                })
                .count()
        }
        _ => body.len(),
    };

    let mut out = String::new();
    for l in fixed.iter().chain(&body[..shown]) {
        out.push_str(l);
        out.push('\n');
    }
    if shown < body.len() {
        // The lowest line of what was cut, not the first cut in tree order:
        // a member drawn under its container (a Rust `impl` under its type)
        // can be far below a sibling that was cut after it (MAJOR 1).
        let cut = &order[shown..];
        let first = cut
            .iter()
            .map(|&(i, _)| nodes[i].start_line)
            .min()
            .unwrap_or(0);
        let last = cut
            .iter()
            .map(|&(i, _)| nodes[i].end_line)
            .max()
            .unwrap_or(first);
        out.push_str(&closing(cut.len(), first, last, budget.unwrap_or(0)));
        out.push('\n');
    }
    for l in input.tail {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// The kind of the file symbol, the root of a file's tree.
const FILE_KIND: &str = "file";

/// One level of the tree.
const INDENT: &str = "  ";

/// The token estimate of the whole product (`repo_map::tokens`).
const CHARS_PER_TOKEN: usize = 4;

/// One import target, at most, in characters.
const IMPORT_CHARS: usize = 80;

/// The line that names what did not fit.
fn closing(more: usize, first: i32, last: i32, budget: usize) -> String {
    format!(
        "[devctx] {more} more symbols (lines {first}-{last}) did not fit the {budget}-token \
         budget (DEVCTX_MAX_OUTPUT_TOKENS); read_file with start_line={first} shows them"
    )
}

/// `imports (N): a, b, …, +M more`, or nothing without imports.
fn imports_line(imports: &FileImports) -> Option<String> {
    if imports.total == 0 && imports.first.is_empty() {
        return None;
    }
    let total = imports_total(imports);
    let mut names: Vec<String> = imports
        .first
        .iter()
        .take(IMPORTS_SHOWN)
        .map(|n| {
            if n.chars().count() > IMPORT_CHARS {
                let mut c: String = n.chars().take(IMPORT_CHARS - 1).collect();
                c.push('…');
                c
            } else {
                n.clone()
            }
        })
        .collect();
    let more = total - names.len();
    if more > 0 {
        names.push(format!("+{more} more"));
    }
    Some(format!("imports ({total}): {}", names.join(", ")))
}

/// Distinct import targets of the file.
fn imports_total(imports: &FileImports) -> usize {
    imports.total.max(imports.first.len())
}

/// A symbol's signature as the skeleton shows it: as the repo map does
/// (annotations, a field's value and a final `;` out); a signature the index
/// cut ends in its own `…`. When what is left still starts with an
/// annotation or attribute that never closes, the cut fell inside it (an
/// index from before the extractor skipped them, review of TASK-014): no
/// name is left to show, so `kind name …`.
fn shown_signature(s: &StoredSymbol) -> String {
    // `int qty;`, `void run();`: the terminator is no part of the head.
    let shown = clean_signature(s.signature.as_deref(), &s.kind, &s.name)
        .trim_end_matches(';')
        .trim_end()
        .to_string();
    let unclosed =
        (shown.starts_with('@') && !shown.starts_with("@interface")) || shown.starts_with("#[");
    if unclosed {
        format!("{} {} …", s.kind, s.name)
    } else {
        shown
    }
}

/// How siblings are ordered: source position (line, byte, the longest
/// first), then kind, name and id.
fn sibling_key(s: &StoredSymbol) -> (i32, i64, std::cmp::Reverse<i64>, &str, &str, u64) {
    (
        s.start_line,
        s.start_byte,
        std::cmp::Reverse(s.end_byte),
        &s.kind,
        &s.name,
        s.id,
    )
}

/// The symbols in tree order (pre-order: a container, then its members),
/// each with its depth. Siblings by line, byte, longest first, kind, name
/// and id: whatever the order of the rows, the same text. A symbol whose
/// parent is not among them is at the top; a cycle cannot loop.
fn tree_order(nodes: &[&StoredSymbol]) -> Vec<(usize, usize)> {
    let mut sorted: Vec<usize> = (0..nodes.len()).collect();
    sorted.sort_by(|&a, &b| sibling_key(nodes[a]).cmp(&sibling_key(nodes[b])));
    let mut index: HashMap<u64, usize> = HashMap::new();
    for &i in &sorted {
        index.entry(nodes[i].id).or_insert(i);
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut roots = Vec::new();
    for &i in &sorted {
        match nodes[i].parent_id.and_then(|p| index.get(&p).copied()) {
            Some(p) if p != i => children[p].push(i),
            _ => roots.push(i),
        }
    }
    let mut seen = vec![false; nodes.len()];
    let mut out = Vec::with_capacity(nodes.len());
    // Roots first; then whatever only a cycle reaches, from its first row.
    for start in roots.into_iter().chain(sorted.iter().copied()) {
        if seen[start] {
            continue;
        }
        let mut stack = vec![(start, 0usize)];
        while let Some((i, depth)) = stack.pop() {
            if std::mem::replace(&mut seen[i], true) {
                continue;
            }
            out.push((i, depth));
            for &c in children[i].iter().rev() {
                if !seen[c] {
                    stack.push((c, depth + 1));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_map::tokens;

    /// A symbol of `a/Foo.java` for the pure tests.
    fn sym(
        id: u64,
        parent: Option<u64>,
        kind: &str,
        name: &str,
        sig: &str,
        lines: (i32, i32),
        bytes: i64,
    ) -> StoredSymbol {
        StoredSymbol {
            id,
            parent_id: parent,
            file: "a/Foo.java".into(),
            kind: kind.into(),
            name: name.into(),
            qualified: name.into(),
            signature: (!sig.is_empty()).then(|| sig.to_string()),
            start_line: lines.0,
            end_line: lines.1,
            start_byte: bytes,
            end_byte: bytes + 10,
            ..Default::default()
        }
    }

    fn file(end: i32) -> StoredSymbol {
        StoredSymbol {
            end_byte: 1_000_000,
            ..sym(1, None, "file", "Foo.java", "", (1, end), 0)
        }
    }

    fn draw(symbols: &[StoredSymbol], imports: &FileImports, budget: usize) -> String {
        render(
            &SkeletonInput {
                file: "a/Foo.java",
                symbols,
                imports,
                head: &[],
                tail: &[],
            },
            budget,
        )
    }

    fn java() -> Vec<StoredSymbol> {
        vec![
            file(40),
            sym(
                10,
                Some(1),
                "class",
                "Foo",
                "@Path(\"/foo\") public class Foo",
                (3, 40),
                30,
            ),
            sym(
                11,
                Some(10),
                "field",
                "repo",
                "private final Repo repo = new Repo()",
                (5, 5),
                60,
            ),
            sym(
                12,
                Some(10),
                "constructor",
                "Foo",
                "public Foo(Repo repo)",
                (7, 9),
                90,
            ),
            sym(
                13,
                Some(10),
                "method",
                "run",
                "@Override public void run(String id)",
                (11, 20),
                120,
            ),
            sym(
                14,
                Some(10),
                "class",
                "Inner",
                "static class Inner",
                (22, 38),
                300,
            ),
            sym(15, Some(14), "method", "go", "int go()", (24, 26), 330),
        ]
    }

    /// The tree by `parent_id`, two spaces per level; each line the cleaned
    /// signature and `[l1-l2]`; a header with the counts and the imports
    /// summarized in one line.
    #[test]
    fn a_tree_by_parent_with_signatures_and_ranges() {
        let imports = FileImports {
            total: 2,
            first: vec!["app.core.Repo".into(), "jakarta.ws.rs.Path".into()],
        };
        let out = draw(&java(), &imports, 8000);
        assert_eq!(
            out,
            "a/Foo.java: 6 symbols\n\
             imports (2): app.core.Repo, jakarta.ws.rs.Path\n\
             public class Foo [3-40]\n\
             \x20 private final Repo repo [5-5]\n\
             \x20 public Foo(Repo repo) [7-9]\n\
             \x20 public void run(String id) [11-20]\n\
             \x20 static class Inner [22-38]\n\
             \x20   int go() [24-26]\n"
        );
    }

    /// The same rows in any order draw the same text; siblings that start
    /// on the same line keep source order (byte), then name — never the id.
    #[test]
    fn the_order_is_the_source_order_whatever_the_input() {
        let mut rows = java();
        // Two names of one declaration (Go `var b, a int`): one line, one
        // range. By id `b` (98) would go first; by name, `a`. `c` starts
        // later on the same line and has the smallest id: last, by byte.
        rows.push(sym(98, Some(1), "const", "b", "var b int", (42, 42), 900));
        rows.push(sym(99, Some(1), "const", "a", "var a int", (42, 42), 900));
        rows.push(sym(2, Some(1), "const", "c", "var c int", (42, 42), 950));
        let base = draw(&rows, &FileImports::default(), 8000);
        let tail: Vec<&str> = base.lines().rev().take(3).collect();
        assert_eq!(
            tail,
            [
                "var c int [42-42]",
                "var b int [42-42]",
                "var a int [42-42]"
            ],
            "{base}"
        );
        for seed in 0..20u64 {
            let mut shuffled = rows.clone();
            // A deterministic shuffle per seed.
            shuffled.sort_by_key(|s| s.id.wrapping_mul(0x9E37_79B9_7F4A_7C15 ^ seed) >> 7);
            assert_eq!(draw(&shuffled, &FileImports::default(), 8000), base);
        }
        let mut reversed = rows.clone();
        reversed.reverse();
        assert_eq!(draw(&reversed, &FileImports::default(), 8000), base);
    }

    /// A file with more symbols than the budget holds: never over the
    /// budget, the symbols shown are the first in source order, and the
    /// closing line counts the rest and names their lines. Budget 0: all.
    #[test]
    fn the_budget_is_kept_and_what_is_cut_is_named() {
        let mut rows = vec![
            file(3000),
            sym(
                10,
                Some(1),
                "class",
                "Big",
                "public class Big",
                (1, 3000),
                5,
            ),
        ];
        for i in 0..300 {
            let line = 10 + i * 9;
            rows.push(sym(
                100 + i as u64,
                Some(10),
                "method",
                &format!("m{i}"),
                &format!("public void m{i}(String first, String second)"),
                (line, line + 7),
                100 + i as i64 * 50,
            ));
        }
        for budget in [MIN_SKELETON_BUDGET, 600, 2000] {
            let out = draw(&rows, &FileImports::default(), budget);
            assert!(tokens(&out) <= budget, "{} > {budget}\n{out}", tokens(&out));
            let shown = out.lines().filter(|l| l.contains("public void m")).count();
            let last = out.lines().last().unwrap();
            assert!(last.starts_with("[devctx] "), "{last}");
            let more = 300 - shown;
            let first_cut = 10 + shown * 9;
            assert!(
                last.contains(&format!("{more} more symbols (lines {first_cut}-"))
                    && last.contains(&format!("start_line={first_cut}")),
                "{last}"
            );
            // In source order: m0, m1, …
            assert!(out.contains("public void m0("), "{out}");
            assert!(shown > 0);
        }
        // Below the floor, the floor.
        let out = draw(&rows, &FileImports::default(), 10);
        assert!(tokens(&out) <= MIN_SKELETON_BUDGET, "{out}");
        assert!(out.contains("more symbols"), "{out}");
        let all = draw(&rows, &FileImports::default(), 0);
        assert_eq!(
            all.lines().filter(|l| l.contains("public void m")).count(),
            300
        );
        assert!(!all.contains("[devctx]"), "{all}");
    }

    /// Test code is marked, not hidden: the file asked for is what the user
    /// wants to see. In a test file the header says so once.
    #[test]
    fn test_code_is_marked_not_hidden() {
        let mut rows = java();
        rows.push(StoredSymbol {
            is_test: true,
            ..sym(20, Some(1), "module", "tests", "mod tests", (50, 60), 2000)
        });
        rows.push(StoredSymbol {
            is_test: true,
            ..sym(
                21,
                Some(20),
                "function",
                "checks",
                "fn checks()",
                (52, 55),
                2100,
            )
        });
        let out = draw(&rows, &FileImports::default(), 8000);
        assert!(out.contains("mod tests [50-60] [test]\n"), "{out}");
        assert!(out.contains("  fn checks() [52-55] [test]\n"), "{out}");
        assert!(out.contains("public class Foo [3-40]\n"), "{out}");
        let all_test: Vec<StoredSymbol> = java()
            .into_iter()
            .map(|s| StoredSymbol { is_test: true, ..s })
            .collect();
        let out = draw(&all_test, &FileImports::default(), 8000);
        assert!(
            out.lines().next().unwrap().ends_with(", test file"),
            "{out}"
        );
        assert!(!out.contains("[test]"), "{out}");
        assert!(out.contains("int go() [24-26]"), "{out}");
    }

    /// Imports: their count and the first ones; the rest counted.
    #[test]
    fn imports_are_one_line() {
        let imports = FileImports {
            total: 12,
            first: (0..IMPORTS_SHOWN).map(|i| format!("pkg.T{i}")).collect(),
        };
        let out = draw(&java(), &imports, 8000);
        assert!(
            out.contains("imports (12): pkg.T0, pkg.T1, pkg.T2, pkg.T3, pkg.T4, pkg.T5, pkg.T6, pkg.T7, +4 more\n"),
            "{out}"
        );
        let none = draw(&java(), &FileImports::default(), 8000);
        assert!(!none.contains("imports"), "{none}");
    }

    /// The index marks a signature it cut with `…` (199 characters and
    /// `…`); shown as is. One whose cut fell inside its leading annotations
    /// (an index from before the extractor skipped them) leaves an
    /// annotation that never closes: `kind name …`, decided by that and not
    /// by whether the name appears somewhere (review of TASK-014, MINOR 3:
    /// `get` inside `summary = "get the order…"`). Exactly 200 characters
    /// without `…`: whole, no false mark.
    #[test]
    fn cut_signatures_are_named() {
        let cut = |s: String| -> String {
            let mut c: String = s.chars().take(199).collect();
            c.push('…');
            c
        };
        let in_annotation = cut(format!(
            "@Operation(summary = \"{}\") public Response list()",
            "x".repeat(300)
        ));
        let get_inside = cut(format!(
            "@Operation(summary = \"get the order {}\") public Order get()",
            "x".repeat(300)
        ));
        let id_inside = cut(format!(
            "@Path(\"/{{id}}/{}\") public Order id()",
            "y".repeat(300)
        ));
        let wide = cut(format!(
            "public void wide({})",
            (0..40)
                .map(|i| format!("String p{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        let name = "n".repeat(200 - "void ()".len());
        let exact = format!("void {name}()");
        let rows = vec![
            file(40),
            sym(10, Some(1), "class", "Foo", "public class Foo", (3, 40), 30),
            sym(11, Some(10), "method", "list", &in_annotation, (5, 9), 60),
            sym(12, Some(10), "method", "wide", &wide, (11, 12), 90),
            sym(13, Some(10), "method", "get", &get_inside, (14, 15), 120),
            sym(14, Some(10), "method", "id", &id_inside, (17, 18), 150),
            sym(15, Some(10), "method", &name, &exact, (20, 21), 180),
        ];
        let out = draw(&rows, &FileImports::default(), 8000);
        assert!(out.contains("  method list … [5-9]\n"), "{out}");
        assert!(out.contains("  method get … [14-15]\n"), "{out}");
        assert!(out.contains("  method id … [17-18]\n"), "{out}");
        assert!(!out.contains("xxxx") && !out.contains("yyyy"), "{out}");
        let wide = out.lines().find(|l| l.contains("wide(")).unwrap();
        assert!(wide.ends_with("… [11-12]"), "{wide}");
        assert!(out.contains(&format!("  {exact} [20-21]\n")), "{out}");
    }

    /// NIT of the review: `label` is the MCP's own line, never a parameter
    /// of the wire: `GET /skeleton?label=…` cannot put text in the answer.
    #[test]
    fn the_label_is_not_a_wire_parameter() {
        let q: SkeletonQuery =
            serde_json::from_value(serde_json::json!({ "file": "a.rs", "label": "evil" })).unwrap();
        assert_eq!(q.label, None);
        let q = SkeletonQuery {
            label: Some("[devctx] skeleton from x".into()),
            ..SkeletonQuery::of("a.rs")
        };
        assert_eq!(q.query_pairs(), vec![("file", "a.rs".to_string())]);
    }

    /// Review of TASK-014, MAJOR 1: the closing line names the lowest line
    /// of what was cut, not the line of the first symbol cut in tree order.
    /// A Rust `impl` is drawn under its type: with `struct A` (l. 10), `struct
    /// B` (l. 20), `impl A` (l. 300) under A and `impl B` (l. 600) under B, a
    /// cut inside `impl A` also cuts `struct B`, which starts at line 20.
    #[test]
    fn the_closing_line_names_the_lowest_cut_line() {
        let mut rows = vec![
            file(900),
            sym(10, Some(1), "struct", "A", "pub struct A", (10, 12), 100),
            sym(20, Some(1), "struct", "B", "pub struct B", (20, 22), 200),
            sym(30, Some(10), "impl", "A", "impl A", (300, 590), 3000),
            sym(40, Some(20), "impl", "B", "impl B", (600, 890), 6000),
        ];
        for i in 0..100 {
            let line = 301 + i * 2;
            rows.push(sym(
                1000 + i as u64,
                Some(30),
                "method",
                &format!("a{i}"),
                &format!("pub fn a{i}(&self, first: String, second: String) -> String"),
                (line, line + 1),
                3001 + i as i64 * 10,
            ));
        }
        let out = draw(&rows, &FileImports::default(), MIN_SKELETON_BUDGET);
        assert!(out.contains("pub fn a0("), "{out}");
        assert!(!out.contains("pub struct B"), "{out}");
        let last = out.lines().last().unwrap();
        assert!(
            last.contains("(lines 20-890)") && last.contains("start_line=20 "),
            "{last}"
        );
    }

    /// Review of TASK-014, MINOR 4: when the fixed lines (here a long tail
    /// and eight long imports) leave no room, the imports line shrinks to its
    /// count, and the budget still holds.
    #[test]
    fn the_imports_line_gives_way_to_the_budget() {
        let imports = FileImports {
            total: 30,
            first: (0..IMPORTS_SHOWN)
                .map(|i| format!("pkg{i}.{}", "q".repeat(76)))
                .collect(),
        };
        let tail = vec![format!("[devctx] branch_fallback: {}", "z".repeat(600))];
        let out = render(
            &SkeletonInput {
                file: "a/Foo.java",
                symbols: &java(),
                imports: &imports,
                head: &[],
                tail: &tail,
            },
            MIN_SKELETON_BUDGET,
        );
        assert!(
            tokens(&out) <= MIN_SKELETON_BUDGET,
            "{}\n{out}",
            tokens(&out)
        );
        assert!(out.contains("\nimports (30)\n"), "{out}");
        assert!(out.contains("public class Foo"), "{out}");
    }
}
