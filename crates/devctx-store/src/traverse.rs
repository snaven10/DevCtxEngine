//! `traverse` over the symbol graph (PLAN-009 DD-16, TASK-013): the agent
//! walks the graph itself, from one symbol (or every definition a name
//! designates), along the relations it picks, in the direction and to the
//! depth it asks, one level at a time.
//!
//! It is the level-by-level walk of `impact_analysis` ([`crate::impact`])
//! with other relations: one statement per level and direction over the
//! whole frontier ([`FrontierSql::Auto`]), always over `live_edges` (a call
//! the link pass discarded reaches nothing), nodes by id, the best edge of a
//! level deciding a node's confidence, the same filters (DD-11) — a node
//! under `min_confidence`, of a test or external is left out, counted and not
//! walked — and, when `calls` is among the relations, the same dispatch
//! through supertypes ([`DispatchIndex`]).
//!
//! What differs: the edges are part of the answer (every edge by which a
//! listed node was reached at its depth, one per pair and relation with the
//! number of occurrences), there is no `max_nodes` but a page — the walk stops
//! reading deeper levels once it holds more nodes than the page needs
//! ([`TraverseOptions::need`]) — and the roots follow `impact_analysis`'s
//! rule: the name asked is never listed, but of the definitions a bare name
//! stood for, one reached from another is.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::dispatch::DispatchIndex;
use crate::error::Result;
use crate::impact::{
    better, confidence_rank, level_order, Direction, FrontierSql, Key, Level, LevelRow,
    DISPATCH_VIA,
};
use crate::lookup::MinConfidence;
use crate::store::Store;
use crate::symbols::{row_to_symbol, StoredSymbol, SYMBOL_SELECT};

/// The deepest `traverse` walks (DD-16): one statement per level and
/// direction, and a level of `references` can be a whole repository.
pub const TRAVERSE_MAX_DEPTH: usize = 4;

/// Every relation `traverse` may follow, as the extractor writes them.
pub const TRAVERSE_KINDS: &[&str] = &[
    "calls",
    "instantiates",
    "imports",
    "inherits",
    "implements",
    "contains",
    "references",
];

/// The relations a dispatch row stands for (a call through a supertype).
const DISPATCH_KINDS: &[&str] = &["calls", "instantiates"];

/// Which way an edge is followed from the node the walk stands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraverseDirection {
    /// To the sources of the edges into it: callers, importers, subtypes,
    /// containers, users of a type.
    In,
    /// To the destinations of its edges: callees, what it imports, its
    /// supertypes, its members, the types it uses.
    Out,
    /// Both, at every level.
    Both,
}

impl TraverseDirection {
    /// `in`, `out` or `both` (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "in" => Some(Self::In),
            "out" => Some(Self::Out),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    /// The name the tool takes back.
    pub fn name(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
            Self::Both => "both",
        }
    }

    fn walks(self) -> &'static [Direction] {
        match self {
            Self::In => &[Direction::Upstream],
            Self::Out => &[Direction::Downstream],
            Self::Both => &[Direction::Upstream, Direction::Downstream],
        }
    }
}

/// What `traverse` follows and shows (DD-16; filters of DD-11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraverseOptions {
    /// Relations followed, a subset of [`TRAVERSE_KINDS`].
    pub kinds: Vec<&'static str>,
    /// Which way.
    pub direction: TraverseDirection,
    /// Levels to walk, 1..=[`TRAVERSE_MAX_DEPTH`].
    pub depth: usize,
    /// Lowest confidence of the edge that reaches a node.
    pub min_confidence: MinConfidence,
    /// List (and walk) symbols of test files.
    pub include_tests: bool,
    /// List external destinations (never walked).
    pub include_external: bool,
    /// Follow dispatch through supertypes when `calls` is followed.
    pub dispatch: bool,
    /// Nodes the caller will show (`offset + limit`): once the walk holds
    /// more, no deeper level is read. 0 = walk every level.
    pub need: usize,
    /// The name asked about (dotted), for `impact_analysis`'s rule on roots:
    /// a root is never listed when it is that name — every root of a
    /// qualified name, or of a single definition — but one root a bare name
    /// stood for, reached from another (`Resource.update` →
    /// `Service.update`), is. Empty: no root is listed.
    pub asked: String,
    /// The roots are what `asked` names, with no file (not a `sym`, not
    /// `file::name`): the calls to that name the index could not decide are
    /// then callers too — counted (`undecided_calls`), or listed with `low`
    /// — when `calls` is followed `in` (review of TASK-013, MAJOR 1).
    pub by_name: bool,
}

impl Default for TraverseOptions {
    fn default() -> Self {
        Self {
            kinds: vec!["calls"],
            direction: TraverseDirection::Out,
            depth: 1,
            min_confidence: MinConfidence::Medium,
            include_tests: false,
            include_external: false,
            dispatch: true,
            need: 0,
            asked: String::new(),
            by_name: false,
        }
    }
}

impl TraverseOptions {
    /// Whether this walk reads dispatch: on, `calls` followed, and a
    /// confidence a dispatch can have (it is never `high`).
    pub fn follows_dispatch(&self) -> bool {
        self.dispatch && self.kinds.contains(&"calls") && self.min_confidence != MinConfidence::High
    }
}

/// A node of a traversal.
#[derive(Debug, Clone, PartialEq)]
pub struct TraverseNode {
    /// The definition's id; `None` for an external or undecided destination.
    pub id: Option<u64>,
    /// Qualified name, or the destination as written.
    pub symbol: String,
    /// Symbol kind (`method`, `class`, `file`…), when it has a definition.
    pub kind: Option<String>,
    /// File of the definition.
    pub file: Option<String>,
    /// First line of the definition.
    pub line: Option<i32>,
    /// Level it was reached at (1 = next to a root).
    pub depth: usize,
    /// Confidence of the best edge that reached it; `None` = undecided.
    pub confidence: Option<String>,
    /// Relation of that edge, or `dispatch`.
    pub via: String,
    /// Reached through dispatch: the method it went through.
    pub through: Option<String>,
    /// A symbol of a test file (or reached from an occurrence in one).
    pub test: bool,
    /// Defined outside the repository, with evidence (DD-9).
    pub external: bool,
    /// Global rank (PageRank).
    pub rank: Option<f64>,
    /// Reached only through a call to the name the link pass could not
    /// decide: listed with `low`, never walked.
    pub unsure: bool,
}

impl TraverseNode {
    /// The link pass could not decide where the edge goes (or whether this
    /// caller's call reaches the name at all).
    pub fn undecided(&self) -> bool {
        self.unsure || (self.id.is_none() && !self.external)
    }
}

/// One end of an edge: the id when it has a definition, and its name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraverseEnd {
    /// The definition's id.
    pub id: Option<u64>,
    /// Qualified name, or the destination as written.
    pub symbol: String,
}

/// An edge of a traversal, in its own direction (`from` holds the
/// occurrence): one per pair of ends, relation and dispatch, however many
/// times it occurs.
#[derive(Debug, Clone, PartialEq)]
pub struct TraverseEdge {
    /// Source.
    pub from: TraverseEnd,
    /// Destination.
    pub to: TraverseEnd,
    /// Relation (`calls` for a dispatch).
    pub kind: String,
    /// Reached through dispatch (`via: "dispatch"`).
    pub dispatch: bool,
    /// The method a dispatch went through.
    pub through: Option<String>,
    /// The best confidence among its occurrences.
    pub confidence: Option<String>,
    /// Line of its first occurrence, when it has one.
    pub line: Option<i32>,
    /// Occurrences.
    pub occurrences: usize,
    /// Index in [`Traversal::nodes`] of the node it reached.
    pub reached: usize,
    /// A call to the name the link pass could not decide (`to` is the name).
    pub undecided: bool,
}

/// A traversal: nodes by depth and DD-11's order, the edges that reached
/// them, and what the filters, the dispatch caps and the page left out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Traversal {
    /// Nodes, by depth, then confidence, rank and name.
    pub nodes: Vec<TraverseNode>,
    /// Every edge by which a node was reached at its depth.
    pub edges: Vec<TraverseEdge>,
    /// The first depth not read because the page was already full.
    pub unread_from: Option<usize>,
    /// Nodes reached only by edges under `min_confidence`.
    pub below_confidence: usize,
    /// Test symbols left out.
    pub tests: usize,
    /// External destinations left out.
    pub external: usize,
    /// Override-equivalent methods past the dispatch cap of a node.
    pub dispatch_capped: usize,
    /// Override-equivalent methods past the dispatch depth cap.
    pub dispatch_beyond_depth: usize,
    /// Calls to the name the link pass left undecided, counted one by one as
    /// `impact_analysis` counts them (not from tests, nor from a caller
    /// listed, nor from a root); 0 when `low` lists their callers.
    pub undecided_calls: usize,
    /// Level statements run (one per level and direction).
    pub levels: usize,
}

/// The `IN (…)` list of `kinds` (validated against [`TRAVERSE_KINDS`]: a
/// literal list, nothing to escape).
fn kinds_sql(kinds: &[&str]) -> String {
    let known: Vec<String> = kinds
        .iter()
        .filter(|k| TRAVERSE_KINDS.contains(k))
        .map(|k| format!("'{k}'"))
        .collect();
    format!("({})", known.join(", "))
}

/// The walk of `traverse` from `roots`, reading each level and direction
/// with `level`.
/// `first_up` joins the first `in` level: the callers of undecided calls to
/// the name (each `unsure`, listed and never walked).
pub(crate) fn traverse_walk(
    roots: &[StoredSymbol],
    opts: &TraverseOptions,
    first_up: Vec<LevelRow>,
    mut level: impl FnMut(&[u64], Direction) -> Result<Level>,
) -> Result<Traversal> {
    let mut first_up = Some(first_up);
    let mut t = Traversal::default();
    let mut names: HashMap<u64, String> =
        roots.iter().map(|s| (s.id, s.qualified.clone())).collect();
    let mut walked: HashSet<u64> = HashSet::new();
    let mut frontier: Vec<u64> = roots
        .iter()
        .map(|s| s.id)
        .filter(|&id| walked.insert(id))
        .collect();
    // The roots that are the name asked are the question, never part of the
    // answer; another root a bare name stood for is listed when reached
    // (walked once, as a root).
    let asked = opts.asked.as_str();
    let mut reported: HashSet<Key> = roots
        .iter()
        .filter(|s| {
            roots.len() == 1 || asked.is_empty() || asked.contains('.') || s.qualified == asked
        })
        .map(|s| Key::Id(s.id))
        .collect();
    let (mut below, mut tests, mut external) = (HashSet::new(), HashSet::new(), HashSet::new());
    let (mut cut, mut beyond): (HashSet<Key>, HashSet<Key>) = (HashSet::new(), HashSet::new());
    let mut reach: HashMap<u64, Option<String>> = HashMap::new();
    for depth in 1..=opts.depth {
        if frontier.is_empty() {
            break;
        }
        let mut rows: Vec<(Direction, LevelRow)> = Vec::new();
        for &dir in opts.direction.walks() {
            let read = level(&frontier, dir)?;
            t.levels += 1;
            cut.extend(read.cut.into_iter().map(Key::Id));
            beyond.extend(read.beyond.into_iter().map(Key::Id));
            rows.extend(read.rows.into_iter().map(|r| (dir, r)));
        }
        if let Some(extra) = first_up.take() {
            rows.extend(extra.into_iter().map(|r| (Direction::Upstream, r)));
        }
        let mut best: HashMap<Key, LevelRow> = HashMap::new();
        let mut reached_by: HashMap<Key, Vec<(Direction, LevelRow)>> = HashMap::new();
        for (dir, mut r) in rows {
            // A dispatch downstream is no surer than what reached its method.
            if r.from_reach {
                if let Some(Some(c)) = reach.get(&r.from) {
                    if confidence_rank(Some(c)) < confidence_rank(r.confidence.as_deref()) {
                        r.confidence = Some(c.clone());
                    }
                }
            }
            if r.id == Some(r.from) {
                continue;
            }
            let key = Key::of(&r);
            if reported.contains(&key) {
                continue;
            }
            match best.get(&key) {
                Some(b) if !better(&r, b) => {}
                _ => {
                    best.insert(key.clone(), r.clone());
                }
            }
            reached_by.entry(key).or_default().push((dir, r));
        }
        let mut admitted: Vec<(Key, LevelRow)> = Vec::new();
        for (key, r) in best {
            if !opts.min_confidence.admits(r.confidence.as_deref()) {
                below.insert(key);
            } else if r.external && !opts.include_external {
                external.insert(key);
            } else if r.test && !opts.include_tests {
                tests.insert(key);
            } else {
                admitted.push((key, r));
            }
        }
        admitted.sort_by(|a, b| level_order(&a.1, &b.1));
        let mut next = Vec::new();
        for (key, r) in admitted {
            reported.insert(key.clone());
            if let Some(id) = r.id {
                names.insert(id, r.symbol.clone());
                if !r.unsure && walked.insert(id) {
                    next.push(id);
                    reach.insert(id, r.confidence.clone());
                }
            }
            let at = t.nodes.len();
            let me = TraverseEnd {
                id: r.id,
                symbol: r.symbol.clone(),
            };
            // The edges that reached it at this depth, one per pair,
            // relation and dispatch; under `min_confidence`, none.
            let mut edges: BTreeMap<(TraverseEnd, TraverseEnd, String, bool), TraverseEdge> =
                BTreeMap::new();
            for (dir, e) in reached_by.remove(&key).unwrap_or_default() {
                if !opts.min_confidence.admits(e.confidence.as_deref()) {
                    continue;
                }
                // An undecided call goes to the name, not to a definition.
                let other = if e.unsure {
                    TraverseEnd {
                        id: None,
                        symbol: asked.to_string(),
                    }
                } else {
                    TraverseEnd {
                        id: Some(e.from),
                        symbol: names.get(&e.from).cloned().unwrap_or_default(),
                    }
                };
                let (from, to) = match dir {
                    Direction::Downstream => (other, me.clone()),
                    Direction::Upstream => (me.clone(), other),
                };
                let dispatch = e.kind == DISPATCH_VIA;
                let kind = if dispatch {
                    "calls".to_string()
                } else {
                    e.kind.clone()
                };
                let slot = edges
                    .entry((from.clone(), to.clone(), kind.clone(), dispatch))
                    .or_insert_with(|| TraverseEdge {
                        from,
                        to,
                        kind,
                        dispatch,
                        through: e.through.clone(),
                        confidence: e.confidence.clone(),
                        line: e.edge_line,
                        occurrences: 0,
                        reached: at,
                        undecided: e.unsure,
                    });
                slot.occurrences += 1;
                if confidence_rank(e.confidence.as_deref())
                    > confidence_rank(slot.confidence.as_deref())
                {
                    slot.confidence = e.confidence.clone();
                }
                if let Some(l) = e.edge_line {
                    slot.line = Some(slot.line.map_or(l, |s| s.min(l)));
                }
            }
            t.edges.extend(edges.into_values());
            t.nodes.push(TraverseNode {
                id: r.id,
                symbol: r.symbol,
                kind: r.node_kind,
                file: r.file,
                line: r.line,
                depth,
                confidence: r.confidence,
                via: r.kind,
                through: r.through,
                test: r.test,
                external: r.external,
                rank: r.rank,
                unsure: r.unsure,
            });
        }
        // The page is full: what a deeper level holds cannot be shown, so
        // it is not read (the next page reads it).
        if opts.need > 0 && t.nodes.len() > opts.need && depth < opts.depth && !next.is_empty() {
            t.unread_from = Some(depth + 1);
            break;
        }
        frontier = next;
    }
    let left_out = |set: HashSet<Key>| set.iter().filter(|k| !reported.contains(k)).count();
    cut.retain(|k| !below.contains(k) && !tests.contains(k) && !external.contains(k));
    beyond.retain(|k| {
        !below.contains(k) && !tests.contains(k) && !external.contains(k) && !cut.contains(k)
    });
    t.dispatch_capped = left_out(cut);
    t.dispatch_beyond_depth = left_out(beyond);
    t.below_confidence = left_out(below);
    t.tests = left_out(tests);
    t.external = left_out(external);
    Ok(t)
}

impl Store {
    /// The roots `name` designates for `traverse`: its definitions
    /// ([`lookup_symbols`](Self::lookup_symbols); `file` restricts to
    /// `file::name`), or, when it names none and is a path, that file's own
    /// symbol (what `imports` leaves from).
    pub fn traverse_roots(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
        file: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredSymbol>> {
        let found = self.lookup_symbols(repo, branch, name, file, limit)?;
        if !found.is_empty() || file.is_some() {
            return Ok(found);
        }
        let path = name.trim().trim_start_matches("./");
        if path.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(&format!(
            "{SYMBOL_SELECT} WHERE repo = ? AND branch = ? AND kind = 'file'
               AND (file = ? OR ends_with(file, '/' || ?)) ORDER BY file LIMIT {limit}"
        ))?;
        let rows = stmt.query_map(duckdb::params![repo, branch, path, path], row_to_symbol)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The traversal of DD-16 from `roots`.
    pub fn traverse(
        &self,
        repo: &str,
        branch: &str,
        roots: &[StoredSymbol],
        opts: &TraverseOptions,
    ) -> Result<Traversal> {
        self.traverse_with(repo, branch, roots, opts, FrontierSql::default())
    }

    pub(crate) fn traverse_with(
        &self,
        repo: &str,
        branch: &str,
        roots: &[StoredSymbol],
        opts: &TraverseOptions,
        mode: FrontierSql,
    ) -> Result<Traversal> {
        let kinds = kinds_sql(&opts.kinds);
        if roots.is_empty() || kinds == "()" {
            return Ok(Traversal::default());
        }
        // Read only when a dispatch could show (as `impact_analysis`).
        let dispatch: Option<DispatchIndex> = if opts.follows_dispatch() {
            self.dispatch_index(repo, branch)?
        } else {
            None
        };
        let mut parents: HashMap<u64, u64> = roots
            .iter()
            .filter_map(|s| s.parent_id.map(|p| (s.id, p)))
            .collect();
        // Calls to the name the link pass could not decide (review of
        // TASK-013, MAJOR 1, as `impact_analysis`): one statement, for the
        // first `in` level only — listed with `low`, counted otherwise.
        let undecided = opts.by_name
            && !opts.asked.is_empty()
            && opts.kinds.contains(&"calls")
            && opts.direction != TraverseDirection::Out;
        let list = undecided && opts.min_confidence == MinConfidence::Low;
        let first_up = if list {
            crate::impact::count_statement();
            self.undecided_call_rows(repo, branch, &opts.asked)?
        } else {
            Vec::new()
        };
        let mut t = traverse_walk(roots, opts, first_up, |f, dir| {
            let read = match &dispatch {
                Some(index) => self.dispatch_level(
                    repo,
                    branch,
                    f,
                    dir,
                    mode,
                    index,
                    &parents,
                    opts.include_tests,
                    &kinds,
                )?,
                None => self.edge_level(repo, branch, f, dir, mode, &kinds)?.into(),
            };
            if dispatch.is_some() {
                for r in &read.rows {
                    if let (Some(id), Some(p)) = (r.id, r.parent) {
                        parents.insert(id, p);
                    }
                }
            }
            Ok(read)
        })?;
        if undecided && !list {
            let shown: HashSet<u64> = t
                .nodes
                .iter()
                .filter_map(|n| n.id)
                .chain(roots.iter().map(|s| s.id))
                .collect();
            t.undecided_calls = self
                .undecided_call_rows(repo, branch, &opts.asked)?
                .iter()
                .filter(|r| !r.test && r.id.is_some_and(|id| !shown.contains(&id)))
                .count();
        }
        Ok(t)
    }
}

/// Whether a row of relation `kind` into an override-equivalent stands for a
/// dispatch (a call through a supertype), not another relation of it.
pub(crate) fn dispatches(kind: &str) -> bool {
    DISPATCH_KINDS.contains(&kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::tests::{call, typed_graph};
    use crate::impact::LEVEL_STATEMENTS;
    use crate::StoredSymbolEdge;

    /// A symbol of a hand-made graph: qualified name, kind, parent.
    type Sym<'a> = (&'a str, &'a str, Option<&'a str>);
    /// An edge: source, relation, destination, confidence, line.
    type Rel<'a> = (&'a str, &'a str, &'a str, &'a str, i32);

    /// Writes `syms` (all in `src/App.java`, test ones — a name starting with
    /// `Test` — in `src/test/…`) and `rels` on `repo`/`main`; a destination
    /// that is no symbol is written unresolved (`low`, or external when its
    /// confidence is `ext`). Returns the symbols by name.
    fn graph(store: &Store, syms: &[Sym], rels: &[Rel]) -> HashMap<String, StoredSymbol> {
        let file_of = |q: &str| {
            if q.starts_with("Test") {
                "src/test/AppTest.java".to_string()
            } else {
                "src/App.java".to_string()
            }
        };
        let mut by: HashMap<String, StoredSymbol> = HashMap::new();
        for (i, (q, kind, _)) in syms.iter().enumerate() {
            by.insert(
                q.to_string(),
                StoredSymbol {
                    id: 9000 + i as u64,
                    file: file_of(q),
                    kind: kind.to_string(),
                    name: q.rsplit('.').next().unwrap().to_string(),
                    qualified: q.to_string(),
                    start_line: 10 + i as i32,
                    end_line: 11 + i as i32,
                    is_test: q.starts_with("Test"),
                    ..Default::default()
                },
            );
        }
        for (q, _, parent) in syms {
            let p = parent.map(|p| by[p].id);
            by.get_mut(*q).unwrap().parent_id = p;
        }
        let mut files: BTreeMap<String, Vec<StoredSymbol>> = BTreeMap::new();
        for s in by.values() {
            files.entry(s.file.clone()).or_default().push(s.clone());
        }
        for (f, list) in &files {
            store.replace_file_symbols("repo", "main", f, list).unwrap();
        }
        let mut edges: BTreeMap<String, Vec<StoredSymbolEdge>> = BTreeMap::new();
        for (src, kind, dst, conf, line) in rels {
            let s = &by[*src];
            let d = by.get(*dst);
            let external = *conf == "ext";
            edges
                .entry(s.file.clone())
                .or_default()
                .push(StoredSymbolEdge {
                    kind: kind.to_string(),
                    src_id: s.id,
                    dst_id: d.map(|d| d.id),
                    dst_name: dst.to_string(),
                    file: s.file.clone(),
                    line: *line,
                    confidence: Some(if external { "medium" } else { conf }.to_string())
                        .filter(|c| c != "none"),
                    resolution: Some(
                        if *conf == "discarded" {
                            "discarded"
                        } else {
                            "local"
                        }
                        .into(),
                    ),
                    external: Some(external),
                    from_test: s.is_test,
                    edge_source: "treesitter".into(),
                    hint: None,
                });
        }
        for (f, rows) in &edges {
            store
                .replace_file_symbol_edges("repo", "main", f, rows)
                .unwrap();
        }
        by
    }

    fn names(t: &Traversal) -> Vec<&str> {
        t.nodes.iter().map(|n| n.symbol.as_str()).collect()
    }

    fn opts(kinds: &[&'static str], direction: TraverseDirection, depth: usize) -> TraverseOptions {
        TraverseOptions {
            kinds: kinds.to_vec(),
            direction,
            depth,
            ..Default::default()
        }
    }

    /// A small service: a resource calls the service, a job calls the
    /// resource, the service calls a repository and a library; the service
    /// class contains its methods and inherits and implements.
    fn service(store: &Store) -> HashMap<String, StoredSymbol> {
        graph(
            store,
            &[
                ("Base", "class", None),
                ("IService", "interface", None),
                ("IRoot", "interface", None),
                ("Service", "class", None),
                ("Service.update", "method", Some("Service")),
                ("Service.cache", "field", Some("Service")),
                ("Resource.put", "method", None),
                ("Job.run", "method", None),
                ("Repo.save", "method", None),
                ("TestService.updates", "method", None),
                ("OrderDto", "class", None),
                ("Gone", "method", None),
            ],
            &[
                ("Resource.put", "calls", "Service.update", "high", 5),
                ("Resource.put", "calls", "Service.update", "high", 9),
                ("Job.run", "calls", "Resource.put", "medium", 3),
                ("TestService.updates", "calls", "Service.update", "high", 4),
                ("Service.update", "calls", "Repo.save", "high", 20),
                ("Service.update", "calls", "mapper.write", "ext", 21),
                ("Service.update", "calls", "flush", "none", 22),
                ("Service", "contains", "Service.update", "high", 12),
                ("Service", "contains", "Service.cache", "high", 13),
                ("Service", "inherits", "Base", "high", 1),
                ("Service", "implements", "IService", "high", 1),
                ("IService", "inherits", "IRoot", "high", 1),
                ("Service.update", "references", "OrderDto", "high", 20),
                ("Gone", "calls", "Service.update", "discarded", 1),
            ],
        )
    }

    /// (a) `in` over `calls` at depth 2: the callers and their callers, each
    /// with the edges that reached it (two occurrences of one call, one
    /// edge); a test caller is left out and counted.
    #[test]
    fn callers_and_their_callers() {
        let store = Store::open_in_memory(3).unwrap();
        let g = service(&store);
        let root = [g["Service.update"].clone()];
        let t = store
            .traverse(
                "repo",
                "main",
                &root,
                &opts(&["calls"], TraverseDirection::In, 2),
            )
            .unwrap();
        assert_eq!(names(&t), ["Resource.put", "Job.run"]);
        assert_eq!(t.nodes.iter().map(|n| n.depth).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(t.tests, 1);
        let e = &t.edges[0];
        assert_eq!(
            (
                e.from.symbol.as_str(),
                e.to.symbol.as_str(),
                e.kind.as_str()
            ),
            ("Resource.put", "Service.update", "calls")
        );
        assert_eq!((e.occurrences, e.line), (2, Some(5)));
        assert_eq!(t.edges[1].from.symbol, "Job.run");
        assert_eq!(t.edges[1].confidence.as_deref(), Some("medium"));
        assert_eq!(t.edges.len(), 2);
    }

    /// (b) `contains` out of a class: its members.
    #[test]
    fn a_class_contains_its_members() {
        let store = Store::open_in_memory(3).unwrap();
        let g = service(&store);
        let t = store
            .traverse(
                "repo",
                "main",
                &[g["Service"].clone()],
                &opts(&["contains"], TraverseDirection::Out, 1),
            )
            .unwrap();
        let mut n = names(&t);
        n.sort_unstable();
        assert_eq!(n, ["Service.cache", "Service.update"]);
        assert!(t.nodes.iter().all(|n| n.via == "contains"));
        let mut kinds: Vec<&str> = t.nodes.iter().filter_map(|n| n.kind.as_deref()).collect();
        kinds.sort_unstable();
        assert_eq!(kinds, ["field", "method"]);
    }

    /// (c) `inherits` + `implements` out of a class, depth 2: its hierarchy.
    #[test]
    fn a_class_climbs_its_hierarchy() {
        let store = Store::open_in_memory(3).unwrap();
        let g = service(&store);
        let t = store
            .traverse(
                "repo",
                "main",
                &[g["Service"].clone()],
                &opts(&["inherits", "implements"], TraverseDirection::Out, 2),
            )
            .unwrap();
        assert_eq!(names(&t), ["Base", "IService", "IRoot"]);
        assert_eq!(t.nodes[2].depth, 2);
        assert_eq!(t.nodes[1].via, "implements");
        // And down again: who implements or extends the interface.
        let t = store
            .traverse(
                "repo",
                "main",
                &[g["IRoot"].clone()],
                &opts(&["inherits", "implements"], TraverseDirection::In, 2),
            )
            .unwrap();
        assert_eq!(names(&t), ["IService", "Service"]);
    }

    /// Out of a method: callees; externals and undecided destinations are
    /// counted by default and listed (never walked) on request; a discarded
    /// call reaches nothing either way.
    #[test]
    fn externals_and_undecided_are_counted_and_discarded_never_seen() {
        let store = Store::open_in_memory(3).unwrap();
        let g = service(&store);
        let root = [g["Service.update"].clone()];
        let t = store
            .traverse(
                "repo",
                "main",
                &root,
                &opts(&["calls"], TraverseDirection::Out, 2),
            )
            .unwrap();
        assert_eq!(names(&t), ["Repo.save"]);
        assert_eq!((t.external, t.below_confidence), (1, 1));
        let all = TraverseOptions {
            min_confidence: MinConfidence::Low,
            include_external: true,
            include_tests: true,
            ..opts(&["calls"], TraverseDirection::Both, 2)
        };
        let t = store.traverse("repo", "main", &root, &all).unwrap();
        let n = names(&t);
        assert!(n.contains(&"mapper.write") && n.contains(&"flush"), "{n:?}");
        assert!(n.contains(&"TestService.updates"), "{n:?}");
        assert!(!n.contains(&"Gone"), "a discarded call: {n:?}");
        let flush = t.nodes.iter().find(|n| n.symbol == "flush").unwrap();
        assert!(flush.undecided() && flush.depth == 1);
    }

    /// `both` reads each level in the two directions; several relations
    /// together.
    #[test]
    fn both_directions_and_several_relations() {
        let store = Store::open_in_memory(3).unwrap();
        let g = service(&store);
        let t = store
            .traverse(
                "repo",
                "main",
                &[g["Service.update"].clone()],
                &opts(
                    &["calls", "contains", "references"],
                    TraverseDirection::Both,
                    1,
                ),
            )
            .unwrap();
        let mut n = names(&t);
        n.sort_unstable();
        assert_eq!(n, ["OrderDto", "Repo.save", "Resource.put", "Service"]);
        assert_eq!(t.levels, 2);
        let e = t
            .edges
            .iter()
            .find(|e| e.to.symbol == "Service.update" && e.kind == "contains");
        assert_eq!(e.unwrap().from.symbol, "Service");
    }

    /// One statement per level and direction, whatever the frontier: 300
    /// callers of callers, depth 3, `in`.
    #[test]
    fn one_statement_per_level_and_direction() {
        let store = Store::open_in_memory(3).unwrap();
        let mut syms: Vec<(String, &str)> = vec![("Core.run".into(), "method")];
        let mut rels: Vec<(String, String)> = Vec::new();
        for i in 0..300 {
            syms.push((format!("A{i}.a"), "method"));
            syms.push((format!("B{i}.b"), "method"));
            rels.push((format!("A{i}.a"), "Core.run".into()));
            rels.push((format!("B{i}.b"), format!("A{i}.a")));
        }
        let s: Vec<Sym> = syms.iter().map(|(q, k)| (q.as_str(), *k, None)).collect();
        let r: Vec<Rel> = rels
            .iter()
            .map(|(a, b)| (a.as_str(), "calls", b.as_str(), "high", 1))
            .collect();
        let g = graph(&store, &s, &r);
        LEVEL_STATEMENTS.with(|c| c.set(0));
        let t = store
            .traverse(
                "repo",
                "main",
                &[g["Core.run"].clone()],
                &opts(&["calls"], TraverseDirection::In, 3),
            )
            .unwrap();
        assert_eq!(t.nodes.len(), 600);
        assert_eq!(LEVEL_STATEMENTS.with(|c| c.get()), t.levels);
        assert!(t.levels <= 3);
    }

    /// The page: once the walk holds more nodes than the page needs, no
    /// deeper level is read — a hub type referenced from 300 places, `in`
    /// over `references` at depth 2 with a page of 50, reads one level.
    #[test]
    fn a_full_page_reads_no_deeper_level() {
        let store = Store::open_in_memory(3).unwrap();
        let mut syms: Vec<String> = vec!["Dto".into()];
        for i in 0..300 {
            syms.push(format!("U{i:03}.use"));
            syms.push(format!("C{i:03}.call"));
        }
        let s: Vec<Sym> = syms
            .iter()
            .map(|q| {
                (
                    q.as_str(),
                    if q == "Dto" { "class" } else { "method" },
                    None,
                )
            })
            .collect();
        let mut r: Vec<Rel> = Vec::new();
        for i in 0..300 {
            r.push((&syms[1 + 2 * i], "references", "Dto", "high", 1));
            r.push((&syms[2 + 2 * i], "references", &syms[1 + 2 * i], "high", 1));
        }
        let g = graph(&store, &s, &r);
        let o = TraverseOptions {
            need: 50,
            ..opts(&["references"], TraverseDirection::In, 2)
        };
        LEVEL_STATEMENTS.with(|c| c.set(0));
        let t = store
            .traverse("repo", "main", &[g["Dto"].clone()], &o)
            .unwrap();
        assert_eq!(t.nodes.len(), 300);
        assert_eq!(t.unread_from, Some(2));
        assert_eq!(LEVEL_STATEMENTS.with(|c| c.get()), 1);
        assert_eq!(t.nodes[0].symbol, "U000.use");
        // A page that the first level does not fill reads the next.
        let o = TraverseOptions { need: 400, ..o };
        let t = store
            .traverse("repo", "main", &[g["Dto"].clone()], &o)
            .unwrap();
        assert_eq!((t.nodes.len(), t.unread_from), (600, None));
    }

    /// Dispatch, when `calls` is followed: upstream from an implementation,
    /// the callers of the interface method, `via: dispatch`, never `high`,
    /// with the edge `caller → implementation` marked; `high` reads none.
    #[test]
    fn calls_follow_dispatch() {
        let store = Store::open_in_memory(3).unwrap();
        let ids = typed_graph(
            &store,
            &[("ServiceImpl", "IService", "implements", "high")],
            &[("IService.update", "void update(String id);")],
            &[
                call("Api.handle", "IService.update"),
                call("ServiceImpl.update", "Repo.save"),
            ],
        );
        let root = store
            .symbol_by_id("repo", "main", ids["ServiceImpl.update"])
            .unwrap()
            .unwrap();
        let t = store
            .traverse(
                "repo",
                "main",
                std::slice::from_ref(&root),
                &opts(&["calls"], TraverseDirection::In, 1),
            )
            .unwrap();
        assert_eq!(names(&t), ["Api.handle"]);
        let n = &t.nodes[0];
        assert_eq!(
            (n.via.as_str(), n.confidence.as_deref()),
            (DISPATCH_VIA, Some("medium"))
        );
        assert_eq!(n.through.as_deref(), Some("IService.update"));
        let e = &t.edges[0];
        assert!(e.dispatch && e.kind == "calls");
        assert_eq!(e.to.symbol, "ServiceImpl.update");
        for o in [
            TraverseOptions {
                dispatch: false,
                ..opts(&["calls"], TraverseDirection::In, 1)
            },
            TraverseOptions {
                min_confidence: MinConfidence::High,
                ..opts(&["calls"], TraverseDirection::In, 1)
            },
            opts(&["references"], TraverseDirection::In, 1),
        ] {
            crate::dispatch::INDEX_READS.with(|c| c.set(0));
            let t = store
                .traverse("repo", "main", std::slice::from_ref(&root), &o)
                .unwrap();
            assert!(t.nodes.is_empty(), "{o:?}");
            assert_eq!(crate::dispatch::INDEX_READS.with(|c| c.get()), 0, "{o:?}");
        }
        // Downstream from the interface method: its implementation.
        let iface = store
            .symbol_by_id("repo", "main", ids["IService.update"])
            .unwrap()
            .unwrap();
        let t = store
            .traverse(
                "repo",
                "main",
                &[iface],
                &opts(&["calls"], TraverseDirection::Out, 2),
            )
            .unwrap();
        assert_eq!(names(&t), ["ServiceImpl.update", "Repo.save"]);
        assert_eq!(t.nodes[0].via, DISPATCH_VIA);
    }

    /// `impact_analysis`'s rule on roots: of the definitions a bare name
    /// stood for, one reached from another is listed (`A.run` → `B.run`);
    /// none is when the name was qualified, or a single definition.
    #[test]
    fn a_root_reached_from_another_is_listed_for_a_bare_name() {
        let store = Store::open_in_memory(3).unwrap();
        let g = graph(
            &store,
            &[("A.run", "method", None), ("B.run", "method", None)],
            &[("A.run", "calls", "B.run", "high", 3)],
        );
        let roots = [g["A.run"].clone(), g["B.run"].clone()];
        let o = |asked: &str| TraverseOptions {
            asked: asked.into(),
            ..opts(&["calls"], TraverseDirection::Out, 1)
        };
        let t = store.traverse("repo", "main", &roots, &o("run")).unwrap();
        assert_eq!(names(&t), ["B.run"]);
        assert_eq!(t.edges[0].from.symbol, "A.run");
        for asked in ["", "X.run"] {
            let t = store.traverse("repo", "main", &roots, &o(asked)).unwrap();
            assert!(t.nodes.is_empty(), "{asked}");
        }
    }

    /// Review of TASK-013, MAJOR 1: an undecided call to the name is a
    /// caller — counted by default, listed with `low` at depth 1 and never
    /// walked (`Top.t` calls the undecided caller: not reached) — read once,
    /// not per level; nothing of it without `by_name`.
    #[test]
    fn undecided_calls_to_the_name_are_counted_or_listed() {
        let store = Store::open_in_memory(3).unwrap();
        let g = graph(
            &store,
            &[
                ("Svc.update", "method", None),
                ("Web.go", "method", None),
                ("Api.handle", "method", None),
                ("Top.t", "method", None),
                ("TestApi.runs", "method", None),
            ],
            &[
                ("Web.go", "calls", "Svc.update", "high", 1),
                ("Api.handle", "calls", "update", "none", 2),
                ("TestApi.runs", "calls", "update", "none", 3),
                ("Top.t", "calls", "Api.handle", "high", 4),
            ],
        );
        let root = [g["Svc.update"].clone()];
        let o = TraverseOptions {
            asked: "update".into(),
            by_name: true,
            ..opts(&["calls"], TraverseDirection::In, 2)
        };
        let t = store.traverse("repo", "main", &root, &o).unwrap();
        assert_eq!(names(&t), ["Web.go"]);
        assert_eq!(t.undecided_calls, 1, "not the test's");
        let low = TraverseOptions {
            min_confidence: MinConfidence::Low,
            ..o.clone()
        };
        LEVEL_STATEMENTS.with(|c| c.set(0));
        let t = store.traverse("repo", "main", &root, &low).unwrap();
        assert_eq!(names(&t), ["Web.go", "Api.handle"]);
        assert_eq!(LEVEL_STATEMENTS.with(|c| c.get()), t.levels + 1);
        let n = &t.nodes[1];
        assert!(n.undecided() && n.depth == 1, "{n:?}");
        assert_eq!(t.undecided_calls, 0);
        let e = t
            .edges
            .iter()
            .find(|e| e.from.symbol == "Api.handle")
            .unwrap();
        assert!(
            e.undecided
                && e.to
                    == TraverseEnd {
                        id: None,
                        symbol: "update".into()
                    }
        );
        for o in [
            TraverseOptions {
                by_name: false,
                ..low.clone()
            },
            TraverseOptions {
                direction: TraverseDirection::Out,
                ..low.clone()
            },
        ] {
            let t = store.traverse("repo", "main", &root, &o).unwrap();
            assert!(!names(&t).contains(&"Api.handle"), "{o:?}");
            assert_eq!(t.undecided_calls, 0);
        }
    }

    /// A path names its file's symbol, what `imports` leaves from.
    #[test]
    fn a_path_is_its_file_symbol() {
        let store = Store::open_in_memory(3).unwrap();
        graph(
            &store,
            &[("src/App.java", "file", None), ("App", "class", None)],
            &[],
        );
        for name in ["src/App.java", "App.java"] {
            let r = store
                .traverse_roots("repo", "main", name, None, 10)
                .unwrap();
            assert_eq!(r.len(), 1, "{name}");
            assert_eq!(r[0].kind, "file");
        }
        let r = store
            .traverse_roots("repo", "main", "App", None, 10)
            .unwrap();
        assert_eq!(r[0].kind, "class");
    }
}
