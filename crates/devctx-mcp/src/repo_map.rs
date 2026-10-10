//! `repo_map` (PLAN-009 TASK-011, DD-14): a map of the repository in about a
//! thousand tokens — the signatures of the symbols most relevant to the
//! agent's focus, grouped by file, ranked by PageRank personalized to that
//! focus (DD-12), without bodies. Orientation, not code: the agent reads code
//! with `read_symbol` / `read_file`.
//!
//! Pure and testable: [`MapGraph`] is built once per branch and index
//! generation (the adjacency the serve caches) and [`render`] draws one map
//! from it per request.
//!
//! - **Ranking.** Without a focus, the global rank (`symbols.rank`). With one,
//!   the same matrix personalized (DD-14): every symbol of a `focus_files`
//!   file weighs 50 in the personalization vector — the files the agent has
//!   open are the referencers whose uses rank —, a symbol named in
//!   `focus_symbols` or as an identifier of `query` weighs 10, and the edges
//!   into it count ×10; an edge into a name starting with `_` or defined in
//!   more than 5 files counts ×0.1. (A ×50 on every edge *out of* a focus
//!   file would cancel in each source's split, as in Aider: the 50 is the
//!   personalization.)
//! - **What is listed.** Never test code (a test file, or `#[cfg(test)]`
//!   code, DD-9) nor libraries (no row in `symbols`), nor file symbols, nor
//!   the focus files themselves (the agent has them).
//! - **Selection.** A binary search on how many of the ranked symbols fit the
//!   budget (tokens = characters / 4, rounded up), stopping once within 15 %
//!   under it; files in order of their symbols' summed rank, symbols by line
//!   under their containers (`parent_id`), indented.

use std::collections::{HashMap, HashSet};

use devctx_index::RankMatrix;
use devctx_store::{MapSymbol, RankEdge};
use serde::{Deserialize, Serialize};

/// Tokens a map takes when the request says nothing (DD-14).
pub const DEFAULT_REPO_MAP_BUDGET: usize = 1024;

/// The least a map is given: its closing lines alone take up to ~200.
pub const MIN_REPO_MAP_BUDGET: usize = 256;

/// The search stops once the map is this close under the budget.
const TOLERANCE: f64 = 0.15;

/// Personalization of a symbol of a focus file (DD-14: the referencers).
const FOCUS_FILE_WEIGHT: f64 = 50.0;

/// Personalization of a symbol the request names, and the multiplier of the
/// edges into it.
const MENTIONED_WEIGHT: f64 = 10.0;

/// Multiplier of the edges into a `_name` or a name defined in many files.
const DAMPENED: f64 = 0.1;

/// More files than this defining a name make it generic (`get`, `run`).
const COMMON_NAME_FILES: usize = 5;

/// The longest signature shown (DD-17).
const SIGNATURE_MAX: usize = 200;

/// Focus items named in the closing line, at most.
const FOCUS_ITEMS_SHOWN: usize = 6;

/// One focus item, at most, in characters.
const FOCUS_ITEM_CHARS: usize = 60;

/// `repo_map`'s parameters, as the tool, the API and the CLI take them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoMapQuery {
    /// Tokens the map may take (default [`DEFAULT_REPO_MAP_BUDGET`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<usize>,
    /// Files the agent has open or is editing: what they use ranks first;
    /// they themselves are not listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus_files: Vec<String>,
    /// Symbols the task is about (bare, qualified or `file::name`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus_symbols: Vec<String>,
    /// The task in words: its identifiers (`camelCase`, `snake_case`,
    /// `Type.member`) count as focus symbols, and so does a one-word query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

impl RepoMapQuery {
    /// The caller's error, checked before anything is opened: a budget of
    /// 0 tokens means nothing (any other is clamped, and the map says so).
    pub fn validate(&self) -> Result<(), String> {
        if self.budget == Some(0) {
            return Err(format!(
                "budget is the tokens the map may take: a positive number (default \
                 {DEFAULT_REPO_MAP_BUDGET}, at least {MIN_REPO_MAP_BUDGET} is used)"
            ));
        }
        Ok(())
    }

    /// The parameters of `GET /repo-map`: lists comma-separated.
    pub fn query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if let Some(b) = self.budget {
            out.push(("budget", b.to_string()));
        }
        if !self.focus_files.is_empty() {
            out.push(("focus_files", self.focus_files.join(",")));
        }
        if !self.focus_symbols.is_empty() {
            out.push(("focus_symbols", self.focus_symbols.join(",")));
        }
        if let Some(q) = self.query.as_deref().filter(|q| !q.trim().is_empty()) {
            out.push(("query", q.to_string()));
        }
        out
    }

    /// The query `GET /repo-map` describes (lists comma-separated).
    pub fn from_params(
        budget: Option<usize>,
        focus_files: Option<&str>,
        focus_symbols: Option<&str>,
        query: Option<String>,
    ) -> Self {
        let list = |s: Option<&str>| -> Vec<String> {
            s.map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
        };
        Self {
            budget,
            focus_files: list(focus_files),
            focus_symbols: list(focus_symbols),
            query: query.filter(|q| !q.trim().is_empty()),
        }
    }

    /// The focus terms: `focus_symbols`, then the identifiers of `query`
    /// (or the query itself when it is one word), each once.
    fn terms(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |t: &str| {
            let t = t.trim();
            if !t.is_empty() && !out.iter().any(|x| x == t) {
                out.push(t.to_string());
            }
        };
        for s in &self.focus_symbols {
            push(s);
        }
        for t in query_identifiers(self.query.as_deref().unwrap_or("")) {
            push(&t);
        }
        out
    }
}

/// The identifiers of a query (`identifier_tokens`), or the query itself when
/// it is a single word made of identifier characters (`Cached`, `dispatch`):
/// an agent naming one symbol.
pub fn query_identifiers(query: &str) -> Vec<String> {
    let found = devctx_search::identifier_tokens(query);
    if !found.is_empty() {
        return found;
    }
    let q = query.trim();
    let word = !q.is_empty()
        && q.chars().all(|c| c.is_alphanumeric() || "_.:".contains(c))
        && q.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_');
    if word {
        vec![q.to_string()]
    } else {
        Vec::new()
    }
}

/// A node of the map: a symbol, interned.
#[derive(Debug, Clone)]
struct Node {
    id: u64,
    parent: Option<u32>,
    file: u32,
    kind: u16,
    name: Box<str>,
    qualified: Box<str>,
    signature: Option<Box<str>>,
    line: i32,
    test: bool,
    rank: Option<f64>,
}

/// A branch's symbols and PageRank matrix, as the repo map ranks and draws
/// them: built once per branch and index generation (DD-12) and shared by
/// every request until the next `index` (or until it goes unused).
#[derive(Debug, Clone)]
pub struct MapGraph {
    /// In the matrix's order (by id).
    nodes: Vec<Node>,
    files: Vec<Box<str>>,
    kinds: Vec<Box<str>>,
    /// The name is defined in more than [`COMMON_NAME_FILES`] files.
    common: Vec<bool>,
    matrix: RankMatrix,
}

impl MapGraph {
    /// The graph of `symbols` (a branch's, [`Store::map_symbols`]) over
    /// `edges` ([`Store::branch_rank_edges`]: `live_edges` only).
    ///
    /// [`Store::map_symbols`]: devctx_store::Store::map_symbols
    /// [`Store::branch_rank_edges`]: devctx_store::Store::branch_rank_edges
    pub fn build(mut symbols: Vec<MapSymbol>, edges: &[RankEdge]) -> Self {
        symbols.sort_by_key(|s| s.id);
        symbols.dedup_by_key(|s| s.id);
        let ids: Vec<u64> = symbols.iter().map(|s| s.id).collect();
        let matrix = RankMatrix::new(&ids, edges);
        let mut files: Vec<Box<str>> = Vec::new();
        let mut file_at: HashMap<String, u32> = HashMap::new();
        let mut kinds: Vec<Box<str>> = Vec::new();
        let mut kind_at: HashMap<String, u16> = HashMap::new();
        let index: HashMap<u64, u32> = ids
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i as u32))
            .collect();
        let mut nodes: Vec<Node> = Vec::with_capacity(symbols.len());
        for s in symbols {
            let file = *file_at.entry(s.file.clone()).or_insert_with(|| {
                files.push(s.file.as_str().into());
                (files.len() - 1) as u32
            });
            let kind = *kind_at.entry(s.kind.clone()).or_insert_with(|| {
                kinds.push(s.kind.as_str().into());
                (kinds.len() - 1) as u16
            });
            nodes.push(Node {
                id: s.id,
                parent: s.parent_id.and_then(|p| index.get(&p).copied()),
                file,
                kind,
                name: s.name.into(),
                qualified: s.qualified.into(),
                signature: s.signature.map(Into::into),
                line: s.start_line,
                test: s.is_test,
                rank: s.rank,
            });
        }
        let file_kind = kinds.iter().position(|k| &**k == "file").map(|k| k as u16);
        let mut defined_in: HashMap<&str, HashSet<u32>> = HashMap::new();
        for n in &nodes {
            if Some(n.kind) != file_kind {
                defined_in.entry(&n.name).or_default().insert(n.file);
            }
        }
        let common = nodes
            .iter()
            .map(|n| defined_in.get(&*n.name).map_or(0, |f| f.len()) > COMMON_NAME_FILES)
            .collect();
        nodes.shrink_to_fit();
        Self {
            nodes,
            files,
            kinds,
            common,
            matrix,
        }
    }

    /// Symbols in the graph.
    pub fn symbol_count(&self) -> usize {
        self.nodes.len()
    }

    /// Aggregated edges of its matrix.
    pub fn edge_count(&self) -> usize {
        self.matrix.edge_count()
    }

    /// Bytes it holds, roughly: what `status` and the measurements report.
    pub fn heap_bytes(&self) -> usize {
        use std::mem::size_of;
        let strings: usize = self
            .nodes
            .iter()
            .map(|n| n.name.len() + n.qualified.len() + n.signature.as_ref().map_or(0, |s| s.len()))
            .sum::<usize>()
            + self.files.iter().map(|f| f.len()).sum::<usize>()
            + self.kinds.iter().map(|k| k.len()).sum::<usize>();
        self.nodes.capacity() * size_of::<Node>()
            + (self.files.capacity() + self.kinds.capacity()) * size_of::<Box<str>>()
            + self.common.capacity()
            + strings
            + self.matrix.heap_bytes()
    }

    fn is_file(&self, i: usize) -> bool {
        &*self.kinds[self.nodes[i].kind as usize] == "file"
    }

    fn kind(&self, i: usize) -> &str {
        &self.kinds[self.nodes[i].kind as usize]
    }

    /// Node indices of the files `f` names (the path, or a suffix of it
    /// after a `/`).
    fn files_named(&self, f: &str) -> Vec<u32> {
        let f = f.trim().trim_start_matches("./");
        if f.is_empty() {
            return Vec::new();
        }
        let suffix = format!("/{f}");
        (0..self.files.len() as u32)
            .filter(|&i| {
                let p = &*self.files[i as usize];
                p == f || p.ends_with(&suffix)
            })
            .collect()
    }

    /// The symbols a focus term names: `file::name`, a qualified name (or
    /// its tail), or a bare one; never a file symbol.
    fn symbols_named(&self, term: &str) -> Vec<usize> {
        let (file, name) = match crate::state::split_file_symbol(term) {
            Some((f, n)) => (Some(self.files_named(f)), n),
            None => (None, term),
        };
        let name = name.trim().replace("::", ".");
        if name.is_empty() {
            return Vec::new();
        }
        let tail = format!(".{name}");
        let dotted = name.contains('.');
        (0..self.nodes.len())
            .filter(|&i| !self.is_file(i))
            .filter(|&i| {
                file.as_ref()
                    .is_none_or(|f| f.contains(&self.nodes[i].file))
            })
            .filter(|&i| {
                let n = &self.nodes[i];
                if dotted {
                    *n.qualified == *name || n.qualified.ends_with(&tail)
                } else {
                    *n.name == *name
                }
            })
            .collect()
    }
}

/// What the request's focus resolved to.
#[derive(Debug, Default)]
struct Focus {
    /// Files (indices into `files`).
    files: HashSet<u32>,
    /// Symbols named.
    mentioned: Vec<usize>,
    /// One item per thing asked, for the closing line.
    items: Vec<String>,
    /// Something was asked.
    asked: bool,
}

fn resolve_focus(g: &MapGraph, q: &RepoMapQuery) -> Focus {
    let mut f = Focus::default();
    for name in &q.focus_files {
        if name.trim().is_empty() {
            continue;
        }
        f.asked = true;
        let found = g.files_named(name);
        f.items.push(if found.is_empty() {
            format!("file {}: not found", clip(name.trim(), FOCUS_ITEM_CHARS))
        } else {
            format!("file {}", clip(name.trim(), FOCUS_ITEM_CHARS))
        });
        f.files.extend(found);
    }
    let from_query: HashSet<String> = query_identifiers(q.query.as_deref().unwrap_or(""))
        .into_iter()
        .collect();
    if q.query.as_deref().is_some_and(|s| !s.trim().is_empty()) && from_query.is_empty() {
        f.asked = true;
        f.items
            .push("query: no identifier in it (camelCase, snake_case, Type.member)".to_string());
    }
    for term in q.terms() {
        f.asked = true;
        let found = g.symbols_named(&term);
        let what = if from_query.contains(&term) && !q.focus_symbols.contains(&term) {
            "query identifier"
        } else {
            "symbol"
        };
        let t = clip(&term, FOCUS_ITEM_CHARS);
        f.items.push(match found.len() {
            0 => format!("{what} {t}: not found"),
            1 => format!("{what} {t} (1 definition)"),
            n => format!("{what} {t} ({n} definitions)"),
        });
        for i in found {
            if !f.mentioned.contains(&i) {
                f.mentioned.push(i);
            }
        }
    }
    f
}

/// `s` cut to `max` characters, with `…` when cut.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// The token estimate of the whole product: characters / 4 (rounded up).
pub fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// A signature as the map shows it: leading annotations (`@Override`,
/// `@Path("/x")`, `#[inline]`) out, a field's or constant's value out, at
/// most [`SIGNATURE_MAX`] characters; `kind name` when there is none.
fn display_signature(g: &MapGraph, i: usize) -> String {
    let n = &g.nodes[i];
    let kind = g.kind(i);
    let mut s = match n
        .signature
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(sig) => strip_annotations(sig).to_string(),
        None => format!("{kind} {}", n.name),
    };
    if matches!(kind, "field" | "const") {
        if let Some(at) = s.find(" = ") {
            s.truncate(at);
        }
    }
    clip(&s, SIGNATURE_MAX)
}

/// `sig` without its leading annotations and attributes; itself when that
/// would leave nothing.
fn strip_annotations(sig: &str) -> &str {
    let mut rest = sig.trim_start();
    loop {
        let bytes = rest.as_bytes();
        let skipped = if bytes.first() == Some(&b'@') {
            let name_end = rest[1..]
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
                .map_or(rest.len(), |e| e + 1);
            // `@interface Marker` declares an annotation: it is the head.
            if name_end == 1 || &rest[1..name_end] == "interface" {
                break;
            }
            let after = &rest[name_end..];
            if after.starts_with('(') {
                match closing(after, '(', ')') {
                    Some(end) => name_end + end + 1,
                    None => break,
                }
            } else {
                name_end
            }
        } else if rest.starts_with("#[") {
            match closing(&rest[1..], '[', ']') {
                Some(end) => end + 2,
                None => break,
            }
        } else {
            break;
        };
        let next = rest[skipped..].trim_start();
        if next.is_empty() {
            break;
        }
        rest = next;
    }
    if rest.is_empty() {
        sig
    } else {
        rest
    }
}

/// Byte offset of the bracket closing the one `s` starts with.
fn closing(s: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// The containers of node `i` shown above it (no file symbol, no test),
/// innermost first.
fn ancestors(g: &MapGraph, i: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = g.nodes[i].parent;
    while let Some(p) = at.map(|p| p as usize) {
        if g.is_file(p) || out.len() >= 32 || p == i {
            break;
        }
        if !g.nodes[p].test {
            out.push(p);
        }
        at = g.nodes[p].parent;
    }
    out
}

/// What one map is drawn from: the ranked candidates and their scores.
struct Plan<'a> {
    g: &'a MapGraph,
    /// Candidate node indices, best first.
    order: Vec<usize>,
    /// Score of every node (personalized or global).
    score: Vec<f64>,
    /// Candidates per file.
    per_file: HashMap<u32, usize>,
    /// Closing lines that do not depend on how many symbols are shown.
    head_notes: Vec<String>,
    tail_notes: Vec<String>,
    /// How the symbols were ranked, for the closing line.
    how: &'static str,
    budget: usize,
}

impl Plan<'_> {
    /// The map of the first `k` candidates, closing lines included.
    fn draw(&self, k: usize) -> String {
        let g = self.g;
        let chosen = &self.order[..k];
        let mut by_file: HashMap<u32, Vec<usize>> = HashMap::new();
        for &i in chosen {
            by_file.entry(g.nodes[i].file).or_default().push(i);
        }
        let rank_of = |i: usize| g.nodes[i].rank.unwrap_or(0.0);
        let mut files: Vec<(u32, f64, f64)> = by_file
            .iter()
            .map(|(&f, list)| {
                (
                    f,
                    list.iter().map(|&i| self.score[i]).sum(),
                    list.iter().map(|&i| rank_of(i)).sum(),
                )
            })
            .collect();
        files.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then(b.2.total_cmp(&a.2))
                .then_with(|| g.files[a.0 as usize].cmp(&g.files[b.0 as usize]))
        });
        let mut out = String::new();
        for (f, _, _) in &files {
            let list = &by_file[f];
            let mut rows: HashMap<usize, usize> = HashMap::new();
            for &i in list {
                let up = ancestors(g, i);
                rows.insert(i, up.len());
                for (d, &a) in up.iter().rev().enumerate() {
                    rows.entry(a).or_insert(d);
                }
            }
            let mut rows: Vec<(usize, usize)> = rows.into_iter().collect();
            rows.sort_by_key(|&(i, d)| (g.nodes[i].line, d, g.nodes[i].id));
            out.push_str(&g.files[*f as usize]);
            out.push('\n');
            for (i, depth) in rows {
                out.push_str("│ ");
                for _ in 0..depth {
                    out.push_str("  ");
                }
                out.push_str(&display_signature(g, i));
                out.push('\n');
            }
            if self.per_file.get(f).copied().unwrap_or(0) > list.len() {
                out.push_str("⋮\n");
            }
        }
        for n in &self.head_notes {
            out.push_str(n);
            out.push('\n');
        }
        let total = self.order.len();
        let rest = total - k;
        let left = if rest == 0 {
            "nothing left out".to_string()
        } else {
            format!(
                "{rest} more did not fit the {}-token budget (a focus or a larger budget shows others)",
                self.budget
            )
        };
        out.push_str(&format!(
            "[devctx] repo map: {k} of {total} symbols in {} files, {}; {left}. To read code: read_symbol / read_file.\n",
            files.len(),
            self.how
        ));
        for n in &self.tail_notes {
            out.push_str(n);
            out.push('\n');
        }
        out
    }
}

/// The map `q` asks for over `g`, within its budget — clamped to
/// [`MIN_REPO_MAP_BUDGET`]..=`max_budget` — closed by `[devctx]` lines (what
/// the focus matched, how many symbols did not fit) and then `tail` (the
/// caller's notes: a branch fallback).
pub fn render(g: &MapGraph, q: &RepoMapQuery, max_budget: usize, tail: &[String]) -> String {
    select(&plan(g, q, max_budget, tail))
}

/// The ranked candidates of a map and its fixed closing lines.
fn plan<'a>(g: &'a MapGraph, q: &RepoMapQuery, max_budget: usize, tail: &[String]) -> Plan<'a> {
    let asked = q.budget.unwrap_or(DEFAULT_REPO_MAP_BUDGET);
    let max_budget = max_budget.max(MIN_REPO_MAP_BUDGET);
    let budget = asked.clamp(MIN_REPO_MAP_BUDGET, max_budget);
    let mut tail_notes: Vec<String> = Vec::new();
    if budget != asked {
        tail_notes.push(format!(
            "[devctx] budget {asked} is outside {MIN_REPO_MAP_BUDGET}..={max_budget} tokens: used {budget}"
        ));
    }
    tail_notes.extend(tail.iter().cloned());
    let focus = resolve_focus(g, q);
    let n = g.nodes.len();
    let mut p: Vec<f64> = g
        .nodes
        .iter()
        .map(|x| {
            if focus.files.contains(&x.file) {
                FOCUS_FILE_WEIGHT
            } else {
                0.0
            }
        })
        .collect();
    for &i in &focus.mentioned {
        p[i] += MENTIONED_WEIGHT;
    }
    let personalized = p.iter().any(|x| *x > 0.0);
    let (score, how): (Vec<f64>, &'static str) = if personalized {
        let mut mult = vec![1.0f64; n];
        for (i, m) in mult.iter_mut().enumerate() {
            if g.nodes[i].name.starts_with('_') {
                *m *= DAMPENED;
            }
            if g.common[i] {
                *m *= DAMPENED;
            }
        }
        for &i in &focus.mentioned {
            mult[i] *= MENTIONED_WEIGHT;
        }
        (
            g.matrix.personalized(&p, &mult).ranks,
            "ranked by PageRank personalized to the focus",
        )
    } else if focus.asked {
        (
            g.nodes.iter().map(|x| x.rank.unwrap_or(0.0)).collect(),
            "ranked by global PageRank (the focus matched nothing)",
        )
    } else {
        (
            g.nodes.iter().map(|x| x.rank.unwrap_or(0.0)).collect(),
            "ranked by global PageRank (no focus: give focus_files, focus_symbols or query)",
        )
    };
    let mut order: Vec<usize> = (0..n)
        .filter(|&i| !g.nodes[i].test && !g.is_file(i) && !focus.files.contains(&g.nodes[i].file))
        .collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&g.nodes[a], &g.nodes[b]);
        score[b]
            .total_cmp(&score[a])
            .then_with(|| match (x.rank, y.rank) {
                (Some(r), Some(s)) => s.total_cmp(&r),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| g.files[x.file as usize].cmp(&g.files[y.file as usize]))
            .then(x.line.cmp(&y.line))
            .then(x.id.cmp(&y.id))
    });
    let mut per_file: HashMap<u32, usize> = HashMap::new();
    for &i in &order {
        *per_file.entry(g.nodes[i].file).or_default() += 1;
    }
    let mut head_notes = Vec::new();
    if focus.asked {
        let mut items: Vec<String> = focus
            .items
            .iter()
            .take(FOCUS_ITEMS_SHOWN)
            .cloned()
            .collect();
        if focus.items.len() > FOCUS_ITEMS_SHOWN {
            items.push(format!("+{} more", focus.items.len() - FOCUS_ITEMS_SHOWN));
        }
        let files_note = if focus.files.is_empty() {
            ""
        } else {
            " (focus files are not listed: read them with read_file)"
        };
        head_notes.push(format!("[devctx] focus: {}{files_note}", items.join("; ")));
    }
    if order.iter().all(|&i| g.nodes[i].rank.is_none()) && !order.is_empty() && !personalized {
        head_notes.push(
            "[devctx] this branch has no ranks yet (an interrupted link pass): run devctx index"
                .to_string(),
        );
    }
    Plan {
        g,
        order,
        score,
        per_file,
        head_notes,
        tail_notes,
        how,
        budget,
    }
}

/// The binary search of DD-14: the most candidates whose map fits the
/// budget, stopping once one is within [`TOLERANCE`] under it. The map
/// never takes more than the budget, unless its closing lines alone do.
fn select(plan: &Plan) -> String {
    let budget = plan.budget;
    // Each symbol costs a line of at least a token: no more than this fit.
    let (mut lo, mut hi) = (0usize, plan.order.len().min(budget + budget / 6));
    let mut best = plan.draw(0);
    let mut best_k = 0;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let text = plan.draw(mid);
        let t = tokens(&text);
        if t <= budget {
            if mid >= best_k {
                best_k = mid;
                best = text;
            }
            if (budget - t) as f64 <= TOLERANCE * budget as f64 {
                break;
            }
            lo = mid + 1;
        } else {
            if mid == 0 {
                break;
            }
            hi = mid - 1;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_store::{RankConfidence, RankKind};

    /// A deterministic generator (no `rand` in the tree).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    fn sym(id: u64, file: &str, kind: &str, q: &str, parent: Option<u64>) -> MapSymbol {
        MapSymbol {
            id,
            parent_id: parent,
            file: file.into(),
            kind: kind.into(),
            name: q.rsplit('.').next().unwrap().into(),
            qualified: q.into(),
            signature: Some(format!("{kind} {q}()")),
            start_line: id as i32,
            is_test: false,
            rank: None,
        }
    }

    fn edge(src: u64, dst: u64) -> RankEdge {
        RankEdge {
            src,
            dst,
            kind: RankKind::Calls,
            confidence: RankConfidence::High,
            from_test: false,
            n: 1,
        }
    }

    /// The global ranks of `syms` over `edges`, as the link pass writes them.
    fn ranked(mut syms: Vec<MapSymbol>, edges: &[RankEdge]) -> Vec<MapSymbol> {
        let ids: Vec<u64> = syms.iter().map(|s| s.id).collect();
        let m = RankMatrix::new(&ids, edges);
        let g = m.global();
        for s in &mut syms {
            s.rank = Some(g.ranks[m.index_of(s.id).unwrap()]);
        }
        syms
    }

    /// The symbol lines of a map (its `│` rows), trimmed.
    fn rows(map: &str) -> Vec<String> {
        map.lines()
            .filter_map(|l| l.strip_prefix("│ "))
            .map(|l| l.trim().to_string())
            .collect()
    }

    fn q(budget: usize) -> RepoMapQuery {
        RepoMapQuery {
            budget: Some(budget),
            ..Default::default()
        }
    }

    /// A random small repository: files with classes and methods (some in
    /// test files, some with long signatures and paths), random calls.
    fn random_repo(rng: &mut Lcg) -> (Vec<MapSymbol>, Vec<RankEdge>) {
        let files = 1 + rng.below(12);
        let mut syms = Vec::new();
        let mut id = 1u64;
        for f in 0..files {
            let deep = "d/".repeat(rng.below(40) as usize);
            let test = rng.below(5) == 0;
            let path = if test {
                format!("src/test/{deep}F{f}Test.java")
            } else {
                format!("src/{deep}F{f}.java")
            };
            let file_id = id;
            syms.push(sym(file_id, &path, "file", &path, None));
            id += 1;
            for c in 0..1 + rng.below(3) {
                let class_id = id;
                let mut cls = sym(
                    class_id,
                    &path,
                    "class",
                    &format!("C{f}x{c}"),
                    Some(file_id),
                );
                cls.is_test = test;
                syms.push(cls);
                id += 1;
                for m in 0..rng.below(8) {
                    let mut s = sym(
                        id,
                        &path,
                        "method",
                        &format!("C{f}x{c}.m{m}"),
                        Some(class_id),
                    );
                    if rng.below(4) == 0 {
                        s.signature = Some(format!(
                            "@Override public {} m{m}({})",
                            "X".repeat(rng.below(150) as usize),
                            "Arg a, ".repeat(rng.below(30) as usize)
                        ));
                    }
                    if rng.below(6) == 0 {
                        s.signature = None;
                    }
                    s.is_test = test;
                    syms.push(s);
                    id += 1;
                }
            }
        }
        let n = syms.len() as u64;
        let edges = (0..rng.below(4 * n))
            .map(|_| edge(1 + rng.below(n), 1 + rng.below(n)))
            .collect();
        (syms, edges)
    }

    /// (a) The map never takes more than the budget × 1.15 — a property over
    /// 300 random small graphs, random budgets (inside and outside the
    /// accepted range) and random focus (found and not).
    #[test]
    fn the_map_never_exceeds_the_budget() {
        let mut rng = Lcg(42);
        for case in 0..300 {
            let (syms, edges) = random_repo(&mut rng);
            let names: Vec<String> = syms.iter().map(|s| s.qualified.clone()).collect();
            let g = MapGraph::build(ranked(syms, &edges), &edges);
            let budget = [64, 256, 300, 512, 1024, 2048, 4096][rng.below(7) as usize];
            let mut req = q(budget);
            if rng.below(2) == 0 {
                req.focus_symbols
                    .push(names[rng.below(names.len() as u64) as usize].clone());
                req.focus_symbols
                    .push(format!("Missing{}", "Z".repeat(100)));
            }
            if rng.below(3) == 0 {
                req.focus_files.push(format!("F{}.java", rng.below(12)));
                req.focus_files.push("nowhere/at/all.java".repeat(5));
            }
            if rng.below(3) == 0 {
                req.query = Some("how does C0x0.m1 relate to some_thing".into());
            }
            let tail = vec!["[devctx] branch_fallback: current branch x is not indexed; answering from branch main".to_string()];
            let map = render(&g, &req, 8000, &tail);
            let effective = budget.clamp(MIN_REPO_MAP_BUDGET, 8000);
            assert!(
                tokens(&map) as f64 <= effective as f64 * 1.15,
                "case {case}: {} tokens for a budget of {effective}:\n{map}",
                tokens(&map)
            );
        }
    }

    /// The binary search fills the budget: with many small symbols the map
    /// is within the tolerance under it (and never over).
    #[test]
    fn the_search_fills_the_budget_within_the_tolerance() {
        let mut syms = vec![sym(1, "src/a.rs", "file", "src/a.rs", None)];
        for i in 2..2000 {
            syms.push(sym(i, "src/a.rs", "function", &format!("f{i}"), Some(1)));
        }
        let g = MapGraph::build(ranked(syms, &[]), &[]);
        for budget in [256, 1024, 4000] {
            let t = tokens(&render(&g, &q(budget), 8000, &[]));
            assert!(t <= budget, "{t} > {budget}");
            assert!(
                t as f64 >= budget as f64 * (1.0 - TOLERANCE),
                "{t} for {budget}"
            );
        }
    }

    /// A service the tests below share: `Hub.get` is called from six places
    /// (globally central); the focus file `src/web/Page.java` calls
    /// `Repo.load`, which nothing else calls.
    fn service() -> (Vec<MapSymbol>, Vec<RankEdge>) {
        let mut syms = vec![
            sym(1, "src/core/Hub.java", "file", "src/core/Hub.java", None),
            sym(2, "src/core/Hub.java", "class", "Hub", Some(1)),
            sym(3, "src/core/Hub.java", "method", "Hub.get", Some(2)),
            sym(10, "src/data/Repo.java", "file", "src/data/Repo.java", None),
            sym(11, "src/data/Repo.java", "class", "Repo", Some(10)),
            sym(12, "src/data/Repo.java", "method", "Repo.load", Some(11)),
            sym(20, "src/web/Page.java", "file", "src/web/Page.java", None),
            sym(21, "src/web/Page.java", "class", "Page", Some(20)),
            sym(22, "src/web/Page.java", "method", "Page.show", Some(21)),
        ];
        let mut edges = vec![edge(22, 12)];
        for i in 0..6u64 {
            let f = format!("src/use/U{i}.java");
            syms.push(sym(100 + 3 * i, &f, "file", &f, None));
            syms.push(sym(
                101 + 3 * i,
                &f,
                "class",
                &format!("U{i}"),
                Some(100 + 3 * i),
            ));
            syms.push(sym(
                102 + 3 * i,
                &f,
                "method",
                &format!("U{i}.run"),
                Some(101 + 3 * i),
            ));
            edges.push(edge(102 + 3 * i, 3));
        }
        (ranked(syms, &edges), edges)
    }

    /// (b) With `focus_files = [X]`, what X uses comes before what is
    /// globally central; the focus file itself is not listed.
    #[test]
    fn what_a_focus_file_uses_comes_before_the_central_code() {
        let (syms, edges) = service();
        let g = MapGraph::build(syms, &edges);
        let global = render(&g, &q(1024), 8000, &[]);
        let at = |m: &str, s: &str| rows(m).iter().position(|r| r.contains(s));
        assert!(
            at(&global, "Hub.get") < at(&global, "Repo.load"),
            "{global}"
        );
        let focused = render(
            &g,
            &RepoMapQuery {
                focus_files: vec!["web/Page.java".into()],
                ..q(1024)
            },
            8000,
            &[],
        );
        let (load, hub) = (at(&focused, "Repo.load"), at(&focused, "Hub.get"));
        assert!(load.is_some() && hub.is_some() && load < hub, "{focused}");
        assert!(!focused.contains("Page.show"), "{focused}");
        assert!(!focused.contains("src/web/Page.java\n"), "{focused}");
        assert!(focused.contains("personalized to the focus"), "{focused}");
        assert!(
            focused.contains("[devctx] focus: file web/Page.java"),
            "{focused}"
        );
        // The first file of the map is the one the focus uses.
        assert!(focused.starts_with("src/data/Repo.java\n"), "{focused}");
    }

    /// (b) A focus symbol (or an identifier of the query) ranks itself and
    /// what it uses first; one that names nothing is said, and the ranking
    /// stays global.
    #[test]
    fn a_focus_symbol_ranks_first_and_a_missing_one_is_said() {
        let (syms, edges) = service();
        let g = MapGraph::build(syms, &edges);
        for req in [
            RepoMapQuery {
                focus_symbols: vec!["Page.show".into()],
                ..q(1024)
            },
            RepoMapQuery {
                query: Some("why does Page.show render twice".into()),
                ..q(1024)
            },
        ] {
            let map = render(&g, &req, 8000, &[]);
            let r = rows(&map);
            let first = |s: &str| r.iter().position(|x| x.contains(s)).unwrap();
            assert!(first("Page.show") < first("Hub.get"), "{map}");
            assert!(first("Repo.load") < first("Hub.get"), "{map}");
        }
        let map = render(
            &g,
            &RepoMapQuery {
                focus_symbols: vec!["Nowhere.at".into()],
                ..q(1024)
            },
            8000,
            &[],
        );
        assert!(map.contains("symbol Nowhere.at: not found"), "{map}");
        assert!(map.contains("the focus matched nothing"), "{map}");
        let map = render(
            &g,
            &RepoMapQuery {
                query: Some("how are pages shown".into()),
                ..q(1024)
            },
            8000,
            &[],
        );
        assert!(map.contains("query: no identifier in it"), "{map}");
    }

    /// (c) Without a focus the symbols are the top of the global rank, in
    /// that order of selection: a small budget shows the best-ranked first.
    #[test]
    fn without_focus_the_global_rank_decides() {
        let mut rng = Lcg(7);
        let (syms, edges) = random_repo(&mut rng);
        let syms = ranked(syms, &edges);
        let g = MapGraph::build(syms.clone(), &edges);
        let mut cands: Vec<&MapSymbol> = syms
            .iter()
            .filter(|s| !s.is_test && s.kind != "file")
            .collect();
        cands.sort_by(|a, b| {
            b.rank
                .unwrap()
                .total_cmp(&a.rank.unwrap())
                .then(a.file.cmp(&b.file))
                .then(a.start_line.cmp(&b.start_line))
        });
        for budget in [256, 512, 2048] {
            let map = render(&g, &q(budget), 8000, &[]);
            let shown: usize = map
                .lines()
                .last()
                .and_then(|l| l.split("repo map: ").nth(1))
                .and_then(|l| l.split(' ').next())
                .and_then(|k| k.parse().ok())
                .unwrap_or_else(|| panic!("{map}"));
            for s in &cands[..shown] {
                let sig = display_signature(&g, g.matrix.index_of(s.id).unwrap());
                assert!(rows(&map).contains(&sig), "{} missing:\n{map}", s.qualified);
            }
            assert!(map.contains("no focus"), "{map}");
        }
    }

    /// (d) Test code never appears — not even when the focus names it or it
    /// ranks highest — and libraries have no rows to appear with.
    #[test]
    fn test_code_never_appears() {
        let mut syms = vec![
            sym(1, "src/App.java", "file", "src/App.java", None),
            sym(2, "src/App.java", "class", "App", Some(1)),
            sym(3, "src/App.java", "method", "App.run", Some(2)),
            sym(4, "src/AppTest.java", "file", "src/AppTest.java", None),
            sym(5, "src/AppTest.java", "class", "AppTest", Some(4)),
            sym(6, "src/AppTest.java", "method", "AppTest.runs", Some(5)),
            // Test code inside a production file (`#[cfg(test)] mod tests`).
            sym(7, "src/App.java", "function", "tests.helper", Some(1)),
        ];
        for s in &mut syms {
            s.is_test = s.file.ends_with("Test.java") || s.qualified == "tests.helper";
        }
        let edges: Vec<RankEdge> = (0..20).map(|_| edge(3, 6)).chain([edge(3, 7)]).collect();
        let g = MapGraph::build(ranked(syms, &edges), &edges);
        for req in [
            q(1024),
            RepoMapQuery {
                focus_symbols: vec!["AppTest.runs".into(), "helper".into()],
                ..q(1024)
            },
            RepoMapQuery {
                focus_files: vec!["src/AppTest.java".into()],
                ..q(1024)
            },
        ] {
            let map = render(&g, &req, 8000, &[]);
            assert!(
                !map.contains("AppTest.runs()") && !map.contains("tests.helper()"),
                "{map}"
            );
            assert!(rows(&map).iter().any(|r| r.contains("App.run")), "{map}");
        }
    }

    /// (e) The hierarchy by `parent_id`: a method under its class, indented,
    /// the class shown even when only its method was selected; files by
    /// their summed rank, symbols by line; `⋮` when a file has more.
    #[test]
    fn members_are_drawn_under_their_containers() {
        let mut syms = vec![
            sym(1, "src/Svc.java", "file", "src/Svc.java", None),
            sym(2, "src/Svc.java", "class", "Svc", Some(1)),
            sym(3, "src/Svc.java", "class", "Svc.Inner", Some(2)),
            sym(4, "src/Svc.java", "method", "Svc.Inner.deep", Some(3)),
            sym(5, "src/Svc.java", "method", "Svc.top", Some(2)),
            sym(6, "src/Svc.java", "method", "Svc.unused", Some(2)),
        ];
        syms[3].signature = Some("@Override @Path(\"/a(b)\") public void deep(int x)".into());
        let edges = vec![edge(5, 4), edge(5, 4)];
        let g = MapGraph::build(ranked(syms, &edges), &edges);
        let map = render(
            &g,
            &RepoMapQuery {
                focus_symbols: vec!["Svc.top".into()],
                ..q(256)
            },
            8000,
            &[],
        );
        let lines: Vec<&str> = map
            .lines()
            .take_while(|l| !l.starts_with("[devctx]"))
            .collect();
        assert_eq!(lines[0], "src/Svc.java", "{map}");
        let pos = |s: &str| lines.iter().position(|l| l.contains(s)).unwrap();
        assert!(lines[pos("class Svc()")].starts_with("│ class"), "{map}");
        assert!(
            lines[pos("class Svc.Inner()")].starts_with("│   class"),
            "{map}"
        );
        assert_eq!(
            lines[pos("deep(int x)")],
            "│     public void deep(int x)",
            "{map}"
        );
        assert!(pos("class Svc()") < pos("Svc.Inner()") && pos("deep") < pos("Svc.top"));
        // With a budget for two symbols only, the third one of the file is
        // cut and the file says so.
        let mut cut = q(256);
        cut.focus_symbols.push("Svc.top".into());
        let g2 = MapGraph::build(
            ranked(
                (0..80)
                    .map(|i| {
                        sym(
                            100 + i,
                            "src/Big.java",
                            "function",
                            &format!("f{i:02}"),
                            None,
                        )
                    })
                    .collect(),
                &[],
            ),
            &[],
        );
        let map = render(&g2, &q(256), 8000, &[]);
        assert!(map.contains("⋮\n"), "{map}");
    }

    /// Determinism: the same graph built from rows in another order draws
    /// the same map, ties included (equal ranks broken by file, line, id).
    #[test]
    fn the_map_is_deterministic() {
        let mut rng = Lcg(99);
        for _ in 0..20 {
            let (syms, edges) = random_repo(&mut rng);
            let syms = ranked(syms, &edges);
            let mut rs = syms.clone();
            rs.reverse();
            let mut re = edges.clone();
            re.reverse();
            let (a, b) = (MapGraph::build(syms, &edges), MapGraph::build(rs, &re));
            let req = RepoMapQuery {
                focus_symbols: vec!["m1".into()],
                ..q(512)
            };
            assert_eq!(render(&a, &req, 8000, &[]), render(&b, &req, 8000, &[]));
            assert_eq!(
                render(&a, &q(512), 8000, &[]),
                render(&b, &q(512), 8000, &[])
            );
        }
        // Equal ranks: by file, then line, then id — not by id alone.
        let syms: Vec<MapSymbol> = [
            (1, "b.rs", 1),
            (2, "a.rs", 20),
            (3, "a.rs", 10),
            (4, "a.rs", 10),
        ]
        .iter()
        .map(|&(id, f, line)| {
            let mut s = sym(id, f, "function", &format!("f{id}"), None);
            s.rank = Some(0.5);
            s.start_line = line;
            s
        })
        .collect();
        let g = MapGraph::build(syms, &[]);
        let p = plan(&g, &q(256), 8000, &[]);
        let ids: Vec<u64> = p.order.iter().map(|&i| g.nodes[i].id).collect();
        assert_eq!(ids, [3, 4, 2, 1]);
    }

    /// DD-14's dampening: of two callees equally reached from the focus, a
    /// `_private` one and one whose name is defined in more than five files
    /// rank below a plain one.
    #[test]
    fn private_and_generic_names_are_dampened() {
        // The tie-break (file, then line) favours the dampened ones.
        let mut syms = vec![
            sym(1, "src/a.py", "file", "src/a.py", None),
            sym(2, "src/a.py", "function", "caller", Some(1)),
            sym(3, "src/z.py", "function", "plain", None),
            sym(4, "src/b1.py", "function", "_hidden", None),
            sym(5, "src/b2.py", "function", "get", None),
        ];
        for i in 1..6u64 {
            syms.push(sym(
                10 + i,
                &format!("src/c{i}.py"),
                "function",
                "get",
                None,
            ));
        }
        let edges = vec![edge(2, 3), edge(2, 4), edge(2, 5)];
        let g = MapGraph::build(ranked(syms, &edges), &edges);
        let map = render(
            &g,
            &RepoMapQuery {
                focus_files: vec!["src/a.py".into()],
                ..q(1024)
            },
            8000,
            &[],
        );
        let r = rows(&map);
        let at = |s: &str| {
            r.iter()
                .position(|x| x == s)
                .unwrap_or_else(|| panic!("{map}"))
        };
        assert!(at("function plain()") < at("function _hidden()"), "{map}");
        assert!(at("function plain()") < at("function get()"), "{map}");
    }

    /// Annotations and attributes leave the signature; a field loses its
    /// value; a missing signature is `kind name`.
    #[test]
    fn signatures_are_cleaned() {
        assert_eq!(
            strip_annotations("@Override @GET @Path(\"/x(y)\") public Uni<Dto> get(Long id)"),
            "public Uni<Dto> get(Long id)"
        );
        assert_eq!(strip_annotations("#[inline] pub fn f()"), "pub fn f()");
        assert_eq!(strip_annotations("@interface Marker"), "@interface Marker");
        assert_eq!(strip_annotations("@Deprecated"), "@Deprecated");
        let mut f = sym(2, "a.java", "field", "A.cache", None);
        f.signature = Some("private final Map<String, X> cache = new HashMap<>();".into());
        let mut none = sym(3, "a.java", "method", "A.run", None);
        none.signature = None;
        let g = MapGraph::build(vec![f, none], &[]);
        assert_eq!(
            display_signature(&g, 0),
            "private final Map<String, X> cache"
        );
        assert_eq!(display_signature(&g, 1), "method run");
    }

    /// The query string of `GET /repo-map` round-trips.
    #[test]
    fn the_query_round_trips_through_its_parameters() {
        let req = RepoMapQuery {
            budget: Some(900),
            focus_files: vec!["src/a.rs".into(), "src/b.rs".into()],
            focus_symbols: vec!["Store.open".into()],
            query: Some("why".into()),
        };
        let pairs: HashMap<&str, String> = req.query_pairs().into_iter().collect();
        let back = RepoMapQuery::from_params(
            pairs.get("budget").and_then(|b| b.parse().ok()),
            pairs.get("focus_files").map(String::as_str),
            pairs.get("focus_symbols").map(String::as_str),
            pairs.get("query").cloned(),
        );
        assert_eq!(back, req);
        assert_eq!(query_identifiers("Cached"), ["Cached"]);
        assert_eq!(
            query_identifiers("how does search_ranked fuse"),
            ["search_ranked"]
        );
        assert!(query_identifiers("how are lists fused").is_empty());
    }
}
