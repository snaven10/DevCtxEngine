//! `impact_analysis` over the symbol graph (PLAN-009 DD-11, TASK-009): the
//! blast radius of a symbol walked **level by level over ids**, one query per
//! level and direction whatever the size of the frontier, over `live_edges`
//! (a call the link pass discarded reaches no answer).
//!
//! - The name is expanded once, into the ids of its definitions
//!   ([`Store::lookup_symbols`]); every later hop follows ids, never names.
//! - Each level keeps the best edge that reached each node; a node whose best
//!   edge is under `min_confidence`, an external, or a test (by default) is
//!   left out **and counted**, never dropped in silence, and is not walked.
//!   Calls to the name the link pass left undecided are counted apart, in
//!   calls (`undecided_calls`), or listed with `low`.
//! - Within a level nodes are ordered by `confidence`, then `rank` (the
//!   global PageRank the link pass computes, TASK-010; a NULL rank sorts
//!   last), then the name.
//! - The cap (`max_nodes`, per direction) is applied **before** the next
//!   level is computed: the nodes of the level that overflow it are counted,
//!   not walked, and no deeper level is read.
//!
//! The 0.9.0 walk over `graph_edges` ([`Store::impact_analysis`]) stays as
//! it is: it is the old path for a branch the current extractor has not
//! indexed (DD-19).

use std::collections::{HashMap, HashSet};

use duckdb::params_from_iter;

use crate::dispatch::{dispatch_confidence, DispatchIndex, Equivalent};
use crate::error::Result;
use crate::lookup::{dotted, MinConfidence};
use crate::store::Store;

/// Default of `max_nodes` (DD-11).
pub const DEFAULT_IMPACT_MAX_NODES: usize = 200;

/// Default depth of `impact_analysis`.
pub const DEFAULT_IMPACT_DEPTH: usize = 3;

/// How many definitions a name may expand into. 0.9.0 had no cap; this one
/// is only a guard against a pathological name.
const SEED_LIMIT: usize = 10_000;

/// The relations a blast radius follows: calls, as 0.9.0 did, and
/// instantiations (`new Foo()` calls `Foo`'s constructor), as
/// `get_references` lists by default.
const IMPACT_KINDS: &str = "('calls', 'instantiates')";

/// What `impact_analysis` shows and how far it walks (DD-11, Q-2 of the
/// plan): by default `high` and `medium`, no tests, no externals, 200 nodes
/// per direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImpactOptions {
    /// Levels to walk.
    pub depth: usize,
    /// Lowest confidence of the edge that reaches a node.
    pub min_confidence: MinConfidence,
    /// Show (and walk) symbols of test files.
    pub include_tests: bool,
    /// Show the external destinations of the downstream side (never walked:
    /// they have no definition here).
    pub include_external: bool,
    /// Nodes per direction; 0 = no cap.
    pub max_nodes: usize,
    /// Follow dispatch through supertypes (TASK-017): callers of the methods
    /// a method overrides, implementations of an interface or abstract
    /// method; `via: "dispatch"`, never surer than `medium`.
    pub dispatch: bool,
}

impl Default for ImpactOptions {
    fn default() -> Self {
        Self {
            depth: DEFAULT_IMPACT_DEPTH,
            min_confidence: MinConfidence::Medium,
            include_tests: false,
            include_external: false,
            max_nodes: DEFAULT_IMPACT_MAX_NODES,
            dispatch: true,
        }
    }
}

/// Frontiers up to this many ids go as an `IN` list under
/// [`FrontierSql::Auto`]; larger ones as one `UBIGINT[]` parameter. 512
/// (review of TASK-009, NIT 1; it was 1 024): the widest frontier of a real
/// walk measured was 373 ids, so every measured walk keeps the `IN` list,
/// and the only width measured above that (1 000 ids) already favoured the
/// parameter (24 against 36 ms), so the band in between goes to it.
const AUTO_IN_MAX: usize = 512;

/// How a level query names its frontier: the default, [`Auto`](Self::Auto),
/// was chosen by measuring them all on copies of two Java repositories and
/// of this one (TASK-009 and its review); the others stay for measuring
/// (feature `bench`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(not(any(test, feature = "bench")), allow(dead_code))]
pub enum FrontierSql {
    /// `IN (1, 2, …)`, the ids written into the statement (integers:
    /// nothing to escape). An id above `i64::MAX` types the whole list as
    /// `HUGEINT`, so `dst_id` is cast on every row (review m3).
    InList,
    /// `IN (1::UBIGINT, …)`: the same list, typed as the column.
    InListTyped,
    /// A temporary table refilled per level (delete, append, join).
    TempTable,
    /// One `UBIGINT[]` parameter, unnested.
    ListParam,
    /// [`InList`](Self::InList) up to [`AUTO_IN_MAX`] ids,
    /// [`ListParam`](Self::ListParam) above. Measured on a copy of a Java
    /// repository of 1 250 files (review of TASK-009, m3): in real walks
    /// (frontiers up to ~400 ids) the `IN` list is 10-40 ms faster per walk
    /// than the parameter; over synthetic frontiers of 1 000-20 000 ids the
    /// parameter is 1.5-3× faster than the `IN` list (and the typed list
    /// slower than both), and no statement grows with the frontier.
    #[default]
    Auto,
}

/// The `via` of a node reached through dispatch (TASK-017).
pub const DISPATCH_VIA: &str = "dispatch";

/// A direction of the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    /// Callers: who is affected if the symbol changes.
    Upstream,
    /// Callees: what the symbol depends on.
    Downstream,
}

/// A node of the blast radius.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactNode {
    /// The definition's id; `None` for a destination with no definition here
    /// (an external, or a call the link pass could not decide).
    pub id: Option<u64>,
    /// Qualified name; the name as written when there is no definition.
    pub symbol: String,
    /// File of the definition.
    pub file: Option<String>,
    /// First line of the definition.
    pub line: Option<i32>,
    /// Level it was reached at (1 = direct).
    pub depth: usize,
    /// Confidence of the best edge that reached it; `None` = undecided.
    pub confidence: Option<String>,
    /// Kind of that edge (`calls`, `instantiates`), or `dispatch`.
    pub via: String,
    /// Reached through dispatch: the method the edge went through (upstream:
    /// the supertype method its caller calls; downstream: the method it
    /// overrides).
    pub through: Option<String>,
    /// A symbol of a test file (upstream: the occurrence is in one).
    pub test: bool,
    /// Defined outside the repository, with evidence (DD-9).
    pub external: bool,
    /// Global rank (PageRank), once the link pass computes it.
    pub rank: Option<f64>,
    /// Reached only through a call the link pass could not decide (an
    /// upstream caller of the name, review of TASK-009 M1): listed with
    /// `low`, never walked.
    pub unsure: bool,
}

impl ImpactNode {
    /// The link pass could not decide: a destination neither resolved nor
    /// external, or a caller reached only through such a call.
    pub fn undecided(&self) -> bool {
        self.unsure || (self.id.is_none() && !self.external)
    }
}

/// One direction of a blast radius, with what it left out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImpactSide {
    /// The nodes shown, by depth and then by the order of DD-11.
    pub nodes: Vec<ImpactNode>,
    /// Nodes found at `capped_at` that did not fit `max_nodes`: counted,
    /// not shown, not walked; no deeper level was read.
    pub capped: usize,
    /// The level the cap stopped the walk at.
    pub capped_at: Option<usize>,
    /// Nodes reached only by edges under `min_confidence`.
    pub below_confidence: usize,
    /// Test symbols left out (`include_tests` false).
    pub tests: usize,
    /// External destinations left out (`include_external` false).
    pub external: usize,
    /// Override-equivalent methods past the dispatch cap of a node
    /// (`DISPATCH_MAX_PER_NODE`): counted, not followed (TASK-017).
    pub dispatch_capped: usize,
    /// Override-equivalent methods in a type past `DISPATCH_DEPTH` supertype
    /// levels: counted, not followed (review of TASK-017, point 6).
    pub dispatch_beyond_depth: usize,
    /// Level queries run.
    pub levels: usize,
    /// The largest frontier a level query read (ids).
    pub widest: usize,
}

/// The blast radius of a name over the symbol graph.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SymbolImpact {
    /// Qualified names of the definitions the name expanded into, sorted.
    pub resolved: Vec<String>,
    /// For a name with no definition here: the destination whose external
    /// call sites stood for it (their callers are the upstream).
    pub external_target: Option<String>,
    /// Callers.
    pub upstream: ImpactSide,
    /// Callees.
    pub downstream: ImpactSide,
    /// Calls to the name the link pass left undecided, counted one by one
    /// (review of TASK-009, m-a): only from code that is not a test and from
    /// callers not already listed upstream (nor the definitions themselves).
    /// 0 when `min_confidence` is `low` (their callers are listed instead)
    /// and under `file::name`.
    pub undecided_calls: usize,
}

/// One edge of a level, as a level query reads it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LevelRow {
    /// The frontier id the edge leaves from (upstream: its destination).
    pub from: u64,
    /// The node reached.
    pub id: Option<u64>,
    /// Its qualified name, or the destination as written.
    pub symbol: String,
    /// File of its definition.
    pub file: Option<String>,
    /// First line of its definition.
    pub line: Option<i32>,
    /// Edge kind.
    pub kind: String,
    /// Edge confidence.
    pub confidence: Option<String>,
    /// External destination.
    pub external: bool,
    /// A test symbol, or an occurrence in a test file.
    pub test: bool,
    /// The node's rank.
    pub rank: Option<f64>,
    /// The edge is an undecided call to the name (the node is its caller).
    pub unsure: bool,
    /// Reached through dispatch: the method it went through.
    pub through: Option<String>,
    /// A downstream dispatch row: no surer than the edge that reached
    /// `from` (`min(medium, original)`).
    pub from_reach: bool,
    /// The node's container (its type, for a method): what dispatch starts
    /// from at the next level.
    pub parent: Option<u64>,
    /// Line of the edge's occurrence (`traverse`, TASK-013).
    pub edge_line: Option<i32>,
    /// The node's symbol kind, when it has a definition.
    pub node_kind: Option<String>,
}

#[cfg(test)]
thread_local! {
    /// Statements the level queries ran on this thread (test (a)).
    pub(crate) static LEVEL_STATEMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn count_statement() {
    #[cfg(test)]
    LEVEL_STATEMENTS.with(|c| c.set(c.get() + 1));
}

fn row_to_level(r: &duckdb::Row<'_>) -> duckdb::Result<LevelRow> {
    Ok(LevelRow {
        from: r.get(0)?,
        id: r.get(1)?,
        symbol: r.get(2)?,
        file: r.get(3)?,
        line: r.get(4)?,
        kind: r.get(5)?,
        confidence: r.get(6)?,
        external: r.get(7)?,
        test: r.get(8)?,
        rank: r.get(9)?,
        unsure: false,
        through: None,
        from_reach: false,
        parent: r.get(10)?,
        edge_line: r.get(11)?,
        node_kind: r.get(12)?,
    })
}

/// What one level query of a walk read: its rows, and the
/// override-equivalent methods the dispatch cap left out (TASK-017).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Level {
    /// The edges of the level.
    pub rows: Vec<LevelRow>,
    /// Ids of the equivalents past the cap of their node.
    pub cut: Vec<u64>,
    /// Ids of the equivalents past the depth cap of the supertype chain.
    pub beyond: Vec<u64>,
}

impl From<Vec<LevelRow>> for Level {
    fn from(rows: Vec<LevelRow>) -> Self {
        Level {
            rows,
            cut: Vec::new(),
            beyond: Vec::new(),
        }
    }
}

/// A node, for deduplication within a direction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Key {
    Id(u64),
    Name(String, bool),
}

impl Key {
    pub(crate) fn of(r: &LevelRow) -> Key {
        match r.id {
            Some(id) => Key::Id(id),
            None => Key::Name(r.symbol.clone(), r.external),
        }
    }
}

/// `high` 3, `medium` 2, anything else 1.
pub(crate) fn confidence_rank(c: Option<&str>) -> u8 {
    match c {
        Some("high") => 3,
        Some("medium") => 2,
        _ => 1,
    }
}

/// Whether `r` is a better edge to its node than `b`.
pub(crate) fn better(r: &LevelRow, b: &LevelRow) -> bool {
    let (rc, bc) = (
        confidence_rank(r.confidence.as_deref()),
        confidence_rank(b.confidence.as_deref()),
    );
    // A decided edge beats an undecided one of the same confidence, and a
    // direct one an edge through dispatch.
    let direct = |x: &LevelRow| x.kind != DISPATCH_VIA;
    rc > bc || (rc == bc && (!r.unsure, direct(r), &b.kind) > (!b.unsure, direct(b), &r.kind))
}

/// DD-11's order within a level: confidence, rank (NULL last), name; then
/// file, line and id so the answer is deterministic.
pub(crate) fn level_order(a: &LevelRow, b: &LevelRow) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    confidence_rank(b.confidence.as_deref())
        .cmp(&confidence_rank(a.confidence.as_deref()))
        .then_with(|| match (a.rank, b.rank) {
            (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(Ordering::Equal),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
        .then_with(|| a.symbol.cmp(&b.symbol))
        .then_with(|| a.file.cmp(&b.file))
        .then_with(|| a.line.cmp(&b.line))
        .then_with(|| a.id.cmp(&b.id))
}

/// The level-by-level walk of one direction from `seeds`, reading each level
/// with `level` (one call per level). `itself` are the seeds that are the
/// name asked about as written: never reported (0.9.0 did not report the
/// name asked about); any other seed is reported when reached from another
/// node (`Resource.update → Service.update`, both answering to `update`).
///
/// With `from_sites`, the first level is read even with an empty frontier:
/// the callers of a name the repository does not define (its external call
/// sites), or of undecided calls to it, as `get_references` lists them.
pub(crate) fn walk(
    seeds: &[u64],
    itself: &HashSet<u64>,
    opts: &ImpactOptions,
    from_sites: bool,
    mut level: impl FnMut(&[u64]) -> Result<Level>,
) -> Result<ImpactSide> {
    let mut side = ImpactSide::default();
    let mut walked: HashSet<u64> = HashSet::new();
    let mut frontier: Vec<u64> = seeds
        .iter()
        .copied()
        .filter(|&s| walked.insert(s))
        .collect();
    let mut reported: HashSet<Key> = itself.iter().map(|&i| Key::Id(i)).collect();
    // Nodes shown only through an undecided call (never walked): a decided
    // edge at a later level replaces them and walks them (review NIT 2).
    let mut unsure_shown: HashSet<Key> = HashSet::new();
    let (mut below, mut tests, mut external) = (HashSet::new(), HashSet::new(), HashSet::new());
    // Equivalents the dispatch cap left out, and the confidence each walked
    // node was reached with (a seed: none, so `high` caps nothing).
    let mut cut: HashSet<Key> = HashSet::new();
    let mut beyond: HashSet<Key> = HashSet::new();
    let mut reach: HashMap<u64, Option<String>> = HashMap::new();
    let cap = if opts.max_nodes == 0 {
        usize::MAX
    } else {
        opts.max_nodes
    };
    for depth in 1..=opts.depth {
        if frontier.is_empty() && !(from_sites && depth == 1) {
            break;
        }
        let read = level(&frontier)?;
        side.levels += 1;
        side.widest = side.widest.max(frontier.len());
        cut.extend(read.cut.into_iter().map(Key::Id));
        beyond.extend(read.beyond.into_iter().map(Key::Id));
        let mut best: HashMap<Key, LevelRow> = HashMap::new();
        for mut r in read.rows {
            // Downstream dispatch: no surer than the edge that reached the
            // method it was dispatched from (`min(medium, original)`).
            if r.from_reach {
                if let Some(Some(c)) = reach.get(&r.from) {
                    if confidence_rank(Some(c)) < confidence_rank(r.confidence.as_deref()) {
                        r.confidence = Some(c.clone());
                    }
                }
            }
            // A recursive call reaches nothing new.
            if r.id == Some(r.from) {
                continue;
            }
            let key = Key::of(&r);
            if reported.contains(&key) && (r.unsure || !unsure_shown.contains(&key)) {
                continue;
            }
            match best.get(&key) {
                Some(b) if !better(&r, b) => {}
                _ => {
                    best.insert(key, r);
                }
            }
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
        // An unsure node reached now by a decided edge leaves its old place.
        let upgraded: HashSet<u64> = admitted
            .iter()
            .filter(|(k, _)| unsure_shown.remove(k))
            .filter_map(|(_, r)| r.id)
            .collect();
        if !upgraded.is_empty() {
            side.nodes
                .retain(|n| !(n.unsure && n.id.is_some_and(|i| upgraded.contains(&i))));
        }
        let room = cap.saturating_sub(side.nodes.len());
        let over = admitted.len() > room;
        if over {
            side.capped = admitted.len() - room;
            side.capped_at = Some(depth);
            admitted.truncate(room);
        }
        let mut next = Vec::new();
        for (key, r) in admitted {
            if r.unsure {
                unsure_shown.insert(key.clone());
            }
            reported.insert(key);
            if let Some(id) = r.id.filter(|_| !r.unsure) {
                if walked.insert(id) {
                    next.push(id);
                    reach.insert(id, r.confidence.clone());
                }
            }
            side.nodes.push(ImpactNode {
                id: r.id,
                symbol: r.symbol,
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
        if over {
            break;
        }
        frontier = next;
    }
    // A node left out at one level and reached at a later one by a better
    // edge is shown, not counted.
    let left_out = |set: HashSet<Key>| set.iter().filter(|k| !reported.contains(k)).count();
    // A method one node's cap cut and another node reached and left out (a
    // test, `low`) is counted where it was left out, once (review MINOR 7).
    cut.retain(|k| !below.contains(k) && !tests.contains(k) && !external.contains(k));
    beyond.retain(|k| {
        !below.contains(k) && !tests.contains(k) && !external.contains(k) && !cut.contains(k)
    });
    side.dispatch_beyond_depth = left_out(beyond);
    side.dispatch_capped = left_out(cut);
    side.below_confidence = left_out(below);
    side.tests = left_out(tests);
    side.external = left_out(external);
    Ok(side)
}

impl Store {
    /// The blast radius of `name` over the symbol graph (DD-11): the
    /// definitions it designates ([`lookup_symbols`](Self::lookup_symbols);
    /// `file` restricts to `file::name`), then each direction walked level by
    /// level over `live_edges`.
    pub fn impact_graph(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
        file: Option<&str>,
        opts: &ImpactOptions,
    ) -> Result<SymbolImpact> {
        self.impact_graph_with(repo, branch, name, file, opts, FrontierSql::default())
    }

    /// [`impact_graph`](Self::impact_graph) with an explicit frontier form,
    /// for measuring them (feature `bench`).
    #[cfg(feature = "bench")]
    pub fn impact_graph_bench(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
        file: Option<&str>,
        opts: &ImpactOptions,
        mode: FrontierSql,
    ) -> Result<SymbolImpact> {
        self.impact_graph_with(repo, branch, name, file, opts, mode)
    }

    /// One level query over an arbitrary `frontier` with an explicit form:
    /// the rows it read (feature `bench`, to measure frontiers wider than a
    /// real walk reaches).
    #[cfg(feature = "bench")]
    pub fn impact_level_bench(
        &self,
        repo: &str,
        branch: &str,
        frontier: &[u64],
        upstream: bool,
        mode: FrontierSql,
    ) -> Result<usize> {
        let dir = if upstream {
            Direction::Upstream
        } else {
            Direction::Downstream
        };
        Ok(self.impact_level(repo, branch, frontier, dir, mode)?.len())
    }

    /// [`impact_graph`](Self::impact_graph) with an explicit frontier form.
    pub(crate) fn impact_graph_with(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
        file: Option<&str>,
        opts: &ImpactOptions,
        mode: FrontierSql,
    ) -> Result<SymbolImpact> {
        let syms = self.lookup_symbols(repo, branch, name, file, SEED_LIMIT)?;
        let asked = dotted(name.trim());
        let seeds: Vec<u64> = syms.iter().map(|s| s.id).collect();
        // The name asked about is never reported as reached: a qualified
        // name designates every definition it matched (`Inner.m` is
        // `Outer.Inner.m`, review m4); a bare one only a definition whose
        // qualified name is the name itself (a top-level function), so a
        // `Resource.update → Service.update` pair still shows.
        let itself: HashSet<u64> = syms
            .iter()
            .filter(|s| asked.contains('.') || s.qualified == asked)
            .map(|s| s.id)
            .collect();
        // A name with no definition here stands for its external call
        // sites (the rule of `lookup_external`; never under `file::name`):
        // their callers are its upstream, it has no downstream.
        let sites = if seeds.is_empty() && file.is_none() {
            self.external_target(repo, branch, &asked)?
        } else {
            None
        };
        // Calls to the name the link pass left undecided are callers 0.9.0
        // listed (review M1): with `low` their callers join the first
        // upstream level, marked and not walked; otherwise they are counted
        // as `get_references` counts them. Never under `file::name`: an
        // undecided row names a name, not a file.
        let undecided = file.is_none();
        let list_undecided = undecided && opts.min_confidence == MinConfidence::Low;
        // Dispatch through supertypes (TASK-017): the supertype graph is
        // read once, as id pairs, and each level reads the methods its
        // frontier could dispatch to (one statement). Nothing is read when no
        // dispatch could show: off, `min_confidence: high` (a dispatch is
        // never `high`) or no definition to start from (review MAJOR 2).
        let dispatch =
            if opts.dispatch && opts.min_confidence != MinConfidence::High && !seeds.is_empty() {
                self.dispatch_index(repo, branch)?
            } else {
                None
            };
        // The container of every node walked, so a level asks for dispatch
        // only for the methods of a type with a supertype or a subtype.
        let seed_parents: HashMap<u64, u64> = syms
            .iter()
            .filter_map(|s| s.parent_id.map(|p| (s.id, p)))
            .collect();
        let side = |dir: Direction| {
            let up = dir == Direction::Upstream;
            let extra = up && (sites.is_some() || list_undecided);
            let mut first = true;
            let mut parents = seed_parents.clone();
            walk(&seeds, &itself, opts, extra, |f| {
                let mut read = if let Some(index) = &dispatch {
                    self.dispatch_level(
                        repo,
                        branch,
                        f,
                        dir,
                        mode,
                        index,
                        &parents,
                        opts.include_tests,
                        IMPACT_KINDS,
                    )?
                } else {
                    self.impact_level(repo, branch, f, dir, mode)?.into()
                };
                if up && std::mem::take(&mut first) {
                    let rows: &mut Vec<LevelRow> = &mut read.rows;
                    if let Some(t) = &sites {
                        rows.extend(self.impact_site_callers(repo, branch, t)?);
                    }
                    if list_undecided {
                        rows.extend(self.impact_undecided_callers(repo, branch, &asked)?);
                    }
                }
                if dispatch.is_some() {
                    for r in &read.rows {
                        if let (Some(id), Some(p)) = (r.id, r.parent) {
                            parents.insert(id, p);
                        }
                    }
                }
                Ok(read)
            })
        };
        let upstream = side(Direction::Upstream)?;
        // Counted in calls, in a count of their own (review m-a): not in
        // `below_confidence`, which counts symbols. A call from a test, from
        // a caller already listed or from a definition of the name hides no
        // caller, so it is not counted.
        let undecided_calls = if undecided && !list_undecided {
            let shown: HashSet<u64> = upstream
                .nodes
                .iter()
                .filter_map(|n| n.id)
                .chain(seeds.iter().copied())
                .collect();
            self.undecided_call_rows(repo, branch, &asked)?
                .iter()
                .filter(|r| !r.test && r.id.is_some_and(|id| !shown.contains(&id)))
                .count()
        } else {
            0
        };
        Ok(SymbolImpact {
            resolved: Store::qualified_names(&syms),
            external_target: sites.clone(),
            upstream,
            downstream: side(Direction::Downstream)?,
            undecided_calls,
        })
    }

    /// The `n` symbols with the most resolved incoming calls on a branch,
    /// by bare name (what a measurement picks its cases from).
    #[cfg(feature = "bench")]
    pub fn impact_top_called(
        &self,
        repo: &str,
        branch: &str,
        n: usize,
    ) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT s.name, count(*) AS c FROM live_edges e
               JOIN symbols s ON s.repo = e.repo AND s.branch = e.branch AND s.id = e.dst_id
              WHERE e.repo = ? AND e.branch = ? AND e.kind IN {IMPACT_KINDS}
              GROUP BY s.name ORDER BY c DESC, s.name LIMIT {n}"
        ))?;
        let rows = stmt.query_map(duckdb::params![repo, branch], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The first upstream level of a name with no definition: the symbols
    /// holding its external call sites (`target`, dotted, as written or as
    /// the last segment of a qualified destination).
    fn impact_site_callers(&self, repo: &str, branch: &str, target: &str) -> Result<Vec<LevelRow>> {
        let d = "replace(e.dst_name, '::', '.')";
        count_statement();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT 0, e.src_id, s.qualified, s.file, s.start_line, e.kind, e.confidence,
                    false, coalesce(e.from_test, false) OR coalesce(s.is_test, false), s.rank,
                    s.parent_id, e.line, s.kind
               FROM live_edges e
               JOIN symbols s ON s.repo = e.repo AND s.branch = e.branch AND s.id = e.src_id
              WHERE e.repo = ? AND e.branch = ? AND e.kind IN {IMPACT_KINDS}
                AND e.dst_id IS NULL AND coalesce(e.external, false)
                AND ({d} = ? OR ends_with({d}, '.' || ?))"
        ))?;
        let rows = stmt.query_map(duckdb::params![repo, branch, target, target], row_to_level)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The callers of the calls to `name` (dotted) the link pass left
    /// undecided: written `name` or ending in `.name`, as
    /// [`count_undecided_references`](Self::count_undecided_references)
    /// counts them. Each row is `unsure`.
    fn impact_undecided_callers(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
    ) -> Result<Vec<LevelRow>> {
        count_statement();
        self.undecided_call_rows(repo, branch, name)
    }

    /// One row per call to `name` (dotted) the link pass left undecided,
    /// with its caller: what [`impact_undecided_callers`](Self::impact_undecided_callers)
    /// lists and the `undecided_calls` count counts.
    fn undecided_call_rows(&self, repo: &str, branch: &str, name: &str) -> Result<Vec<LevelRow>> {
        let d = "replace(e.dst_name, '::', '.')";
        let mut stmt = self.conn.prepare(&format!(
            "SELECT 0, e.src_id, s.qualified, s.file, s.start_line, e.kind, e.confidence,
                    false, coalesce(e.from_test, false) OR coalesce(s.is_test, false), s.rank,
                    s.parent_id, e.line, s.kind
               FROM live_edges e
               JOIN symbols s ON s.repo = e.repo AND s.branch = e.branch AND s.id = e.src_id
              WHERE e.repo = ? AND e.branch = ? AND e.kind IN {IMPACT_KINDS}
                AND e.dst_id IS NULL AND NOT coalesce(e.external, false)
                AND ({d} = ? OR ends_with({d}, '.' || ?))"
        ))?;
        let rows = stmt.query_map(duckdb::params![repo, branch, name, name], row_to_level)?;
        rows.map(|r| {
            r.map(|mut l| {
                l.unsure = true;
                l
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    /// One level of the walk with dispatch (TASK-017): the override-
    /// equivalents of the frontier (one statement, over the types of
    /// `index`), then the level query.
    /// Upstream, the level reads the callers of the frontier and of the
    /// methods it overrides, the latter as `dispatch` rows of the method
    /// that overrides them; downstream, the frontier's own callees plus its
    /// overriding methods as `dispatch` rows. Never surer than `medium`, nor
    /// than the edge they stand for.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dispatch_level(
        &self,
        repo: &str,
        branch: &str,
        frontier: &[u64],
        dir: Direction,
        mode: FrontierSql,
        index: &DispatchIndex,
        parents: &HashMap<u64, u64>,
        include_tests: bool,
        kinds: &str,
    ) -> Result<Level> {
        // Only a method of a type in the supertype graph can dispatch: the
        // others cost no statement.
        let methods: Vec<u64> = frontier
            .iter()
            .copied()
            .filter(|id| parents.get(id).is_some_and(|p| index.has_type(*p)))
            .collect();
        let eq =
            self.override_equivalents(repo, branch, &methods, dir, mode, index, include_tests)?;
        let mut out = Level {
            rows: Vec::new(),
            cut: eq.cut,
            beyond: eq.beyond,
        };
        match dir {
            Direction::Upstream => {
                let direct: HashSet<u64> = frontier.iter().copied().collect();
                let mut by_id: HashMap<u64, Vec<&Equivalent>> = HashMap::new();
                for e in &eq.kept {
                    by_id.entry(e.id).or_default().push(e);
                }
                let mut set = frontier.to_vec();
                set.extend(by_id.keys().filter(|id| !direct.contains(id)));
                for r in self.edge_level(repo, branch, &set, dir, mode, kinds)? {
                    // Only a call stands for a dispatch: another relation into
                    // an equivalent (`traverse`) is that method's own.
                    let stands = crate::traverse::dispatches(&r.kind);
                    for e in by_id.get(&r.from).into_iter().flatten().filter(|_| stands) {
                        out.rows.push(LevelRow {
                            from: e.origin,
                            kind: DISPATCH_VIA.to_string(),
                            confidence: dispatch_confidence(r.confidence.as_deref(), e.chain),
                            through: Some(e.symbol.clone()),
                            ..r.clone()
                        });
                    }
                    if direct.contains(&r.from) {
                        out.rows.push(r);
                    }
                }
            }
            Direction::Downstream => {
                out.rows = self.edge_level(repo, branch, frontier, dir, mode, kinds)?;
                out.rows.extend(eq.kept.into_iter().map(|e| LevelRow {
                    from: e.origin,
                    id: Some(e.id),
                    symbol: e.symbol,
                    file: e.file,
                    line: e.line,
                    kind: DISPATCH_VIA.to_string(),
                    confidence: dispatch_confidence(Some("high"), e.chain),
                    external: false,
                    test: e.test,
                    rank: e.rank,
                    unsure: false,
                    through: Some(e.origin_symbol),
                    from_reach: true,
                    parent: Some(e.parent),
                    edge_line: None,
                    node_kind: Some(e.kind),
                }));
            }
        }
        Ok(out)
    }

    /// The SQL set `frontier` is read as (`IN {set}`), in the form `mode`
    /// says ([`FrontierSql::Auto`] by width); a list parameter is pushed onto
    /// `args`, so the set goes where its `?` is in the statement's order.
    pub(crate) fn frontier_set(
        &self,
        frontier: &[u64],
        mode: FrontierSql,
        args: &mut Vec<duckdb::types::Value>,
    ) -> Result<String> {
        let ids = |suffix: &str| {
            frontier
                .iter()
                .map(|id| format!("{id}{suffix}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mode = match mode {
            FrontierSql::Auto if frontier.len() <= AUTO_IN_MAX => FrontierSql::InList,
            FrontierSql::Auto => FrontierSql::ListParam,
            m => m,
        };
        Ok(match mode {
            FrontierSql::InListTyped => format!("({})", ids("::UBIGINT")),
            FrontierSql::InList => format!("({})", ids("")),
            FrontierSql::ListParam => {
                args.push(format!("[{}]", ids("")).into());
                "(SELECT unnest(CAST(? AS UBIGINT[])))".to_string()
            }
            FrontierSql::TempTable => {
                self.conn.execute_batch(
                    "CREATE TEMP TABLE IF NOT EXISTS impact_frontier (id UBIGINT);
                     DELETE FROM impact_frontier;",
                )?;
                count_statement();
                let mut app = self.conn.appender("impact_frontier")?;
                for &id in frontier {
                    app.append_row([id])?;
                }
                app.flush()?;
                count_statement();
                "(SELECT id FROM impact_frontier)".to_string()
            }
            FrontierSql::Auto => unreachable!("resolved above"),
        })
    }

    /// One level of the walk: every `calls`/`instantiates` edge of
    /// `live_edges` into (upstream) or out of (downstream) the ids of
    /// `frontier`, with the node at its other end.
    pub(crate) fn impact_level(
        &self,
        repo: &str,
        branch: &str,
        frontier: &[u64],
        dir: Direction,
        mode: FrontierSql,
    ) -> Result<Vec<LevelRow>> {
        self.edge_level(repo, branch, frontier, dir, mode, IMPACT_KINDS)
    }

    /// [`impact_level`](Self::impact_level) over the relations `kinds` (an
    /// SQL list of literals, `('calls', …)`): the level query `traverse`
    /// shares (TASK-013).
    pub(crate) fn edge_level(
        &self,
        repo: &str,
        branch: &str,
        frontier: &[u64],
        dir: Direction,
        mode: FrontierSql,
        kinds: &str,
    ) -> Result<Vec<LevelRow>> {
        if frontier.is_empty() {
            return Ok(Vec::new());
        }
        let mut args: Vec<duckdb::types::Value> =
            vec![repo.to_string().into(), branch.to_string().into()];
        let set = self.frontier_set(frontier, mode, &mut args)?;
        let sql = match dir {
            Direction::Upstream => format!(
                "SELECT e.dst_id, e.src_id, s.qualified, s.file, s.start_line, e.kind,
                        e.confidence, false,
                        coalesce(e.from_test, false) OR coalesce(s.is_test, false), s.rank,
                        s.parent_id, e.line, s.kind
                   FROM live_edges e
                   JOIN symbols s ON s.repo = e.repo AND s.branch = e.branch AND s.id = e.src_id
                  WHERE e.repo = ? AND e.branch = ? AND e.kind IN {kinds}
                    AND e.dst_id IN {set}"
            ),
            Direction::Downstream => format!(
                "SELECT e.src_id, e.dst_id, coalesce(t.qualified, e.dst_name), t.file,
                        t.start_line, e.kind, e.confidence, coalesce(e.external, false),
                        coalesce(t.is_test, false), t.rank, t.parent_id,
                        e.line, t.kind
                   FROM live_edges e
                   LEFT JOIN symbols t ON t.repo = e.repo AND t.branch = e.branch
                                      AND t.id = e.dst_id
                  WHERE e.repo = ? AND e.branch = ? AND e.kind IN {kinds}
                    AND e.src_id IN {set}"
            ),
        };
        count_statement();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), row_to_level)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{StoredSymbol, StoredSymbolEdge};
    use std::collections::BTreeMap;

    /// An edge of a synthetic symbol graph: `src` → `dst` (a qualified name
    /// that gets a symbol, or, with `resolved` false, a name as written).
    #[derive(Clone)]
    pub(crate) struct E {
        pub src: String,
        pub dst: String,
        pub resolved: bool,
        pub confidence: Option<&'static str>,
        pub external: bool,
        pub resolution: Option<&'static str>,
        pub line: i32,
    }

    /// A resolved `high` call.
    pub(crate) fn call(src: &str, dst: &str) -> E {
        E {
            src: src.into(),
            dst: dst.into(),
            resolved: true,
            confidence: Some("high"),
            external: false,
            resolution: Some("local"),
            line: 1,
        }
    }

    /// The file a qualified name lives in: one file for the code, one per
    /// test class (`Test` in its first segment), so a graph of thousands of
    /// symbols is a handful of writes.
    fn file_of(q: &str) -> String {
        let head = q.split('.').next().unwrap_or(q);
        if head.contains("Test") {
            format!("src/test/{head}.java")
        } else {
            "src/main/Main.java".to_string()
        }
    }

    /// Writes `symbols` and `edges` for `edges` on `repo`/`main`: one
    /// symbol per qualified name that is a source or a resolved destination
    /// (ids by first appearance), `ranks` set on the ones named there.
    pub(crate) fn symbol_graph(
        store: &Store,
        edges: &[E],
        ranks: &[(&str, f64)],
    ) -> BTreeMap<String, u64> {
        symbol_graph_from(store, edges, ranks, 1000)
    }

    /// [`symbol_graph`] with ids counted up from `base`.
    pub(crate) fn symbol_graph_from(
        store: &Store,
        edges: &[E],
        ranks: &[(&str, f64)],
        base: u64,
    ) -> BTreeMap<String, u64> {
        let mut ids: BTreeMap<String, u64> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        for e in edges {
            for (q, is_sym) in [(&e.src, true), (&e.dst, e.resolved)] {
                if is_sym && !ids.contains_key(q) {
                    ids.insert(q.clone(), base + ids.len() as u64);
                    order.push(q.clone());
                }
            }
        }
        let mut by_file: BTreeMap<String, Vec<StoredSymbol>> = BTreeMap::new();
        for (i, q) in order.iter().enumerate() {
            let file = file_of(q);
            by_file.entry(file.clone()).or_default().push(StoredSymbol {
                id: ids[q],
                file: file.clone(),
                kind: "method".into(),
                name: q.rsplit('.').next().unwrap().to_string(),
                qualified: q.clone(),
                start_line: 10 + i as i32,
                end_line: 12 + i as i32,
                is_test: file.contains("/test/"),
                rank: ranks.iter().find(|(n, _)| n == q).map(|(_, r)| *r),
                ..Default::default()
            });
        }
        for (file, syms) in &by_file {
            store
                .replace_file_symbols("repo", "main", file, syms)
                .unwrap();
        }
        let mut edges_by_file: BTreeMap<String, Vec<StoredSymbolEdge>> = BTreeMap::new();
        for e in edges {
            let file = file_of(&e.src);
            edges_by_file
                .entry(file.clone())
                .or_default()
                .push(StoredSymbolEdge {
                    kind: "calls".into(),
                    src_id: ids[&e.src],
                    dst_id: e.resolved.then(|| ids[&e.dst]),
                    dst_name: e.dst.clone(),
                    file: file.clone(),
                    line: e.line,
                    confidence: e.confidence.map(str::to_string),
                    resolution: e.resolution.map(str::to_string),
                    external: Some(e.external),
                    from_test: file.contains("/test/"),
                    edge_source: "treesitter".into(),
                    hint: None,
                });
        }
        for (file, rows) in &edges_by_file {
            store
                .replace_file_symbol_edges("repo", "main", file, rows)
                .unwrap();
        }
        ids
    }

    fn names(side: &ImpactSide) -> Vec<&str> {
        side.nodes.iter().map(|n| n.symbol.as_str()).collect()
    }

    fn all_in() -> ImpactOptions {
        ImpactOptions {
            min_confidence: MinConfidence::Low,
            include_tests: true,
            include_external: true,
            max_nodes: 0,
            ..Default::default()
        }
    }

    /// (a) One statement per level and direction, whatever the size of the
    /// frontier: three levels of 1 000 callers each, depth 3, are at most
    /// 2 × 3 level statements (the seed lookup is one more, constant). The
    /// 0.9.0 walk ran one query per node: 3 000 here.
    #[test]
    fn a_level_is_one_query_whatever_the_frontier() {
        let store = Store::open_in_memory(3).unwrap();
        let mut edges = Vec::new();
        for i in 0..1000 {
            edges.push(call(&format!("A{i}.a"), "Core.run"));
            edges.push(call(&format!("B{i}.b"), &format!("A{i}.a")));
            edges.push(call(&format!("C{i}.c"), &format!("B{i}.b")));
        }
        edges.push(call("Core.run", "Dep.x"));
        symbol_graph(&store, &edges, &[]);
        for mode in [
            FrontierSql::InList,
            FrontierSql::InListTyped,
            FrontierSql::ListParam,
            FrontierSql::Auto,
        ] {
            LEVEL_STATEMENTS.with(|c| c.set(0));
            let opts = ImpactOptions {
                max_nodes: 0,
                ..Default::default()
            };
            let im = store
                .impact_graph_with("repo", "main", "Core.run", None, &opts, mode)
                .unwrap();
            assert_eq!(im.upstream.nodes.len(), 3000, "{mode:?}");
            assert_eq!(names(&im.downstream), ["Dep.x"], "{mode:?}");
            let n = LEVEL_STATEMENTS.with(|c| c.get());
            assert!(n <= 2 * 3, "{mode:?}: {n} level statements");
            assert!(im.upstream.levels <= 3 && im.downstream.levels <= 3);
        }
        // The temporary table costs two more statements per level (empty
        // and fill), still independent of the frontier.
        LEVEL_STATEMENTS.with(|c| c.set(0));
        let im = store
            .impact_graph_with(
                "repo",
                "main",
                "Core.run",
                None,
                &ImpactOptions {
                    max_nodes: 0,
                    ..Default::default()
                },
                FrontierSql::TempTable,
            )
            .unwrap();
        assert_eq!(im.upstream.nodes.len(), 3000);
        assert!(LEVEL_STATEMENTS.with(|c| c.get()) <= 3 * 2 * 3);
    }

    /// (b) 1 000 callers and `max_nodes` 200: 200 shown, 800 counted, and
    /// the next level is never read.
    #[test]
    fn the_cap_counts_the_rest_and_stops_before_the_next_level() {
        let store = Store::open_in_memory(3).unwrap();
        let mut edges: Vec<E> = (0..1000)
            .map(|i| call(&format!("C{i:04}.c"), "Core.run"))
            .collect();
        edges.push(call("Far.away", "C0000.c"));
        symbol_graph(&store, &edges, &[]);
        let im = store
            .impact_graph("repo", "main", "Core.run", None, &ImpactOptions::default())
            .unwrap();
        assert_eq!(im.upstream.nodes.len(), 200);
        assert_eq!(im.upstream.capped, 800);
        assert_eq!(im.upstream.capped_at, Some(1));
        assert_eq!(im.upstream.levels, 1, "the cap is applied before level 2");
        assert!(!names(&im.upstream).contains(&"Far.away"));
        // By name, as rank is NULL: the first 200.
        assert_eq!(im.upstream.nodes[0].symbol, "C0000.c");
        assert_eq!(im.upstream.nodes[199].symbol, "C0199.c");
        // A level that fills the cap exactly lets the next one be counted.
        let opts = ImpactOptions {
            max_nodes: 1000,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "Core.run", None, &opts)
            .unwrap();
        assert_eq!((im.upstream.nodes.len(), im.upstream.capped), (1000, 1));
        assert_eq!(im.upstream.capped_at, Some(2));
    }

    /// (c) Tests and externals are left out by default, counted, and not
    /// walked; with `include_*` they are listed and marked.
    #[test]
    fn tests_and_externals_are_counted_by_default_and_listed_on_request() {
        let store = Store::open_in_memory(3).unwrap();
        let mut ext = call("Svc.run", "serde.from_str");
        ext.resolved = false;
        ext.external = true;
        ext.confidence = Some("medium");
        let edges = vec![
            call("Api.handle", "Svc.run"),
            call("SvcTest.runs", "Svc.run"),
            call("Helper.setup", "SvcTest.runs"),
            call("Svc.run", "Repo.save"),
            ext,
        ];
        symbol_graph(&store, &edges, &[]);
        let im = store
            .impact_graph("repo", "main", "Svc.run", None, &ImpactOptions::default())
            .unwrap();
        assert_eq!(names(&im.upstream), ["Api.handle"]);
        assert_eq!(im.upstream.tests, 1);
        assert_eq!(names(&im.downstream), ["Repo.save"]);
        assert_eq!(im.downstream.external, 1);

        let opts = ImpactOptions {
            include_tests: true,
            include_external: true,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "Svc.run", None, &opts)
            .unwrap();
        assert_eq!(
            names(&im.upstream),
            ["Api.handle", "SvcTest.runs", "Helper.setup"]
        );
        let t = &im.upstream.nodes[1];
        assert!(t.test && t.depth == 1, "{t:?}");
        assert_eq!(
            im.upstream.nodes[2].depth, 2,
            "a test is walked when listed"
        );
        assert_eq!(names(&im.downstream), ["Repo.save", "serde.from_str"]);
        let e = &im.downstream.nodes[1];
        assert!(e.external && e.id.is_none() && !e.undecided(), "{e:?}");
        assert_eq!((im.upstream.tests, im.downstream.external), (0, 0));
    }

    /// (d) Within a level: confidence, then rank (NULL last: it falls to the
    /// name), then the name.
    #[test]
    fn a_level_is_ordered_by_confidence_rank_and_name() {
        let store = Store::open_in_memory(3).unwrap();
        let mut medium = call("A.y", "Core.run");
        medium.confidence = Some("medium");
        let edges = vec![
            call("B.z", "Core.run"),
            medium,
            call("D.b", "Core.run"),
            call("C.a", "Core.run"),
            call("E.e", "Core.run"),
        ];
        symbol_graph(&store, &edges, &[("C.a", 0.5), ("E.e", 0.9)]);
        let im = store
            .impact_graph("repo", "main", "Core.run", None, &ImpactOptions::default())
            .unwrap();
        assert_eq!(names(&im.upstream), ["E.e", "C.a", "B.z", "D.b", "A.y"]);
        assert_eq!(im.upstream.nodes[4].confidence.as_deref(), Some("medium"));
    }

    /// Requirement 1 of the review: a discarded call reaches no impact,
    /// whatever the filters; `low` edges are counted by default and listed
    /// with `min_confidence: low`, the undecided ones marked and not walked.
    #[test]
    fn discarded_calls_never_reach_an_impact_and_low_is_counted() {
        let store = Store::open_in_memory(3).unwrap();
        let mut gone = call("Gone.x", "Core.run");
        gone.resolution = Some("discarded");
        let mut gone_down = call("Core.run", "Lost.y");
        gone_down.resolution = Some("discarded");
        gone_down.external = true;
        let mut low = call("Weak.w", "Core.run");
        low.confidence = Some("low");
        let mut undecided = call("Core.run", "flush");
        undecided.resolved = false;
        undecided.confidence = None;
        undecided.resolution = None;
        let edges = vec![gone, gone_down, low, undecided, call("Ok.k", "Core.run")];
        symbol_graph(&store, &edges, &[]);
        let im = store
            .impact_graph("repo", "main", "Core.run", None, &all_in())
            .unwrap();
        assert_eq!(names(&im.upstream), ["Ok.k", "Weak.w"]);
        assert_eq!(names(&im.downstream), ["flush"]);
        assert!(im.downstream.nodes[0].undecided());
        let im = store
            .impact_graph("repo", "main", "Core.run", None, &ImpactOptions::default())
            .unwrap();
        assert_eq!(names(&im.upstream), ["Ok.k"]);
        assert_eq!(im.upstream.below_confidence, 1);
        assert!(im.downstream.nodes.is_empty());
        assert_eq!(im.downstream.below_confidence, 1);
    }

    /// A node left out at one level and reached by a better edge at a later
    /// one is listed, not counted; a recursive call adds nothing.
    #[test]
    fn a_node_shown_later_is_not_counted_and_recursion_adds_nothing() {
        let store = Store::open_in_memory(3).unwrap();
        let mut low = call("Late.l", "Core.run");
        low.confidence = Some("low");
        let edges = vec![
            low,
            call("Mid.m", "Core.run"),
            call("Late.l", "Mid.m"),
            call("Core.run", "Core.run"),
        ];
        symbol_graph(&store, &edges, &[]);
        let im = store
            .impact_graph("repo", "main", "Core.run", None, &ImpactOptions::default())
            .unwrap();
        assert_eq!(names(&im.upstream), ["Mid.m", "Late.l"]);
        assert_eq!(im.upstream.nodes[1].depth, 2);
        assert_eq!(im.upstream.below_confidence, 0);
        assert!(im.downstream.nodes.is_empty());
    }

    /// The walk over the symbol graph as the 0.9.0 tests see it: `(symbol,
    /// depth)` pairs.
    fn as_0_9_0(
        store: &Store,
        opts: ImpactOptions,
    ) -> impl Fn(&str, usize) -> crate::ImpactResult + '_ {
        move |s, depth| {
            let im = store
                .impact_graph("repo", "main", s, None, &ImpactOptions { depth, ..opts })
                .unwrap();
            let pairs = |side: &ImpactSide| -> Vec<(String, usize)> {
                side.nodes
                    .iter()
                    .map(|n| (n.symbol.clone(), n.depth))
                    .collect()
            };
            crate::ImpactResult {
                upstream: pairs(&im.upstream),
                downstream: pairs(&im.downstream),
            }
        }
    }

    /// The `graph_edges` shapes of the 0.9.0 tests, as the symbol graph
    /// holds them: every call resolved `high`.
    fn seeded_graph() -> Store {
        let store = Store::open_in_memory(3).unwrap();
        symbol_graph(
            &store,
            &[call("a", "b"), call("a", "d"), call("b", "c")],
            &[],
        );
        store
    }

    fn java_like_graph(extra: &[E]) -> Store {
        let store = Store::open_in_memory(3).unwrap();
        let mut edges = vec![
            call("OfficeResource.actualizar", "OfficeService.actualizar"),
            call("TicketResource.actualizar", "TicketService.actualizar"),
            call("OfficeService.actualizar", "Office.persist"),
        ];
        edges.extend_from_slice(extra);
        symbol_graph(&store, &edges, &[]);
        store
    }

    /// Review requirement 2: the asserts of the 0.9.0 impact tests
    /// (`graph.rs`), unchanged, over the symbol graph — with the defaults,
    /// and with every filter off (what 0.9.0, which had none, showed).
    #[test]
    fn the_0_9_0_impact_asserts_hold_over_the_symbol_graph() {
        use crate::graph::tests as old;
        for opts in [ImpactOptions::default(), all_in()] {
            let store = seeded_graph();
            old::check_impact_bfs_upstream_and_downstream(&as_0_9_0(&store, opts));
            let store = java_like_graph(&[]);
            old::check_impact_seeds_from_every_form_of_a_bare_name(&as_0_9_0(&store, opts));
            let resolve = |s: &str| {
                store
                    .impact_graph("repo", "main", s, None, &opts)
                    .unwrap()
                    .resolved
            };
            old::check_resolving_a_bare_name_lists_every_declaration_it_could_mean(&resolve);
            let callers = |s: &str| -> Vec<String> {
                let walk = as_0_9_0(&store, opts);
                walk(s, 1).upstream.into_iter().map(|(n, _)| n).collect()
            };
            old::check_a_qualified_name_does_not_collect_its_homonyms(&resolve, &callers);
        }
        // `Office.persist` calls a bare `flush` the link pass could not
        // decide; an unrelated `Repo.flush` calls something that must never
        // surface. 0.9.0 listed `flush`: so does the walk with every filter
        // off, and it never follows it (it has no id).
        let mut flush = call("Office.persist", "flush");
        flush.resolved = false;
        flush.confidence = None;
        flush.resolution = None;
        let mut deeper = call("Repo.flush", "noDebeAparecer");
        deeper.resolved = false;
        let store = java_like_graph(&[flush, deeper]);
        old::check_traversal_does_not_re_expand_the_names_it_walks(&as_0_9_0(&store, all_in()));
        // By default it is counted, not listed.
        let im = store
            .impact_graph(
                "repo",
                "main",
                "OfficeService.actualizar",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert_eq!(names(&im.downstream), ["Office.persist"]);
        assert_eq!(im.downstream.below_confidence, 1);
    }

    /// A name with no definition here stands for its external call sites
    /// (as in `get_references`): their callers are the upstream, walked by
    /// id from there; there is no downstream. A name with neither is empty.
    #[test]
    fn a_name_with_no_definition_walks_from_its_external_sites() {
        let store = Store::open_in_memory(3).unwrap();
        let mut ext = call("Svc.run", "serde_json::from_str");
        ext.resolved = false;
        ext.external = true;
        ext.confidence = Some("medium");
        let mut test_site = ext.clone();
        test_site.src = "SvcTest.parses".into();
        symbol_graph(
            &store,
            &[ext, test_site, call("Api.handle", "Svc.run")],
            &[],
        );
        for name in ["from_str", "serde_json::from_str", "serde_json.from_str"] {
            let im = store
                .impact_graph("repo", "main", name, None, &ImpactOptions::default())
                .unwrap();
            assert!(im.resolved.is_empty(), "{name}");
            assert!(im.external_target.is_some(), "{name}");
            assert_eq!(names(&im.upstream), ["Svc.run", "Api.handle"], "{name}");
            assert_eq!(im.upstream.nodes[0].confidence.as_deref(), Some("medium"));
            assert_eq!(im.upstream.tests, 1, "{name}");
            assert!(im.downstream.nodes.is_empty(), "{name}");
        }
        let im = store
            .impact_graph("repo", "main", "nothing", None, &ImpactOptions::default())
            .unwrap();
        assert!(im.external_target.is_none() && im.upstream.nodes.is_empty());
        // `file::name` never stands for a library's sites.
        let im = store
            .impact_graph(
                "repo",
                "main",
                "from_str",
                Some("src/main/Main.java"),
                &ImpactOptions::default(),
            )
            .unwrap();
        assert!(im.external_target.is_none() && im.upstream.nodes.is_empty());
    }

    /// Review of TASK-009, M1: a call to the name that the link pass left
    /// undecided (`svc.update()` on an untyped receiver) is a caller 0.9.0
    /// listed. By default it is counted in `undecided_calls` (m-a); with `low` its
    /// caller is listed, marked undecided, and not walked. Under
    /// `file::name` it is neither (it names a name, not a file).
    #[test]
    fn undecided_calls_to_the_name_are_counted_or_listed_upstream() {
        let store = Store::open_in_memory(3).unwrap();
        let mut unsure = call("Api.handle", "update");
        unsure.resolved = false;
        unsure.confidence = None;
        unsure.resolution = None;
        let edges = vec![
            call("Web.go", "Svc.update"),
            unsure,
            call("Top.t", "Api.handle"),
        ];
        symbol_graph(&store, &edges, &[]);
        let im = store
            .impact_graph("repo", "main", "update", None, &ImpactOptions::default())
            .unwrap();
        assert_eq!(names(&im.upstream), ["Web.go"]);
        assert_eq!(im.undecided_calls, 1);
        assert_eq!(im.upstream.below_confidence, 0, "symbols only (m-a)");
        let low = ImpactOptions {
            min_confidence: MinConfidence::Low,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "update", None, &low)
            .unwrap();
        assert_eq!(names(&im.upstream), ["Web.go", "Api.handle"]);
        let n = &im.upstream.nodes[1];
        assert!(n.undecided() && n.id.is_some() && n.depth == 1, "{n:?}");
        assert!(
            !names(&im.upstream).contains(&"Top.t"),
            "an undecided caller is not walked"
        );
        assert_eq!(im.upstream.below_confidence, 0);
        // A qualified name counts only the undecided calls written that way.
        let im = store
            .impact_graph(
                "repo",
                "main",
                "Svc.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert_eq!(im.undecided_calls, 0);
        // `file::name`: neither listed nor counted.
        for opts in [ImpactOptions::default(), low] {
            let im = store
                .impact_graph("repo", "main", "update", Some("src/main/Main.java"), &opts)
                .unwrap();
            assert_eq!(names(&im.upstream), ["Web.go"]);
            assert_eq!(im.undecided_calls, 0);
        }
    }

    /// Review of TASK-009, m-a: `below_confidence` counts symbols, and the
    /// undecided calls to the name go to their own count, in calls: none
    /// from a test, none from a caller already listed (by a decided edge) or
    /// from the definition itself.
    #[test]
    fn undecided_calls_have_their_own_count_without_tests_or_listed_callers() {
        let store = Store::open_in_memory(3).unwrap();
        let unsure = |src: &str| {
            let mut e = call(src, "update");
            e.resolved = false;
            e.confidence = None;
            e.resolution = None;
            e
        };
        let mut weak = call("Weak.w", "Svc.update");
        weak.confidence = Some("low");
        let mut second = unsure("Api.handle");
        second.line = 2;
        let edges = vec![
            call("Web.go", "Svc.update"),
            weak,
            unsure("Api.handle"),
            second,
            unsure("Web.go"),
            unsure("SvcTest.runs"),
            unsure("Svc.update"),
        ];
        symbol_graph(&store, &edges, &[]);
        let im = store
            .impact_graph("repo", "main", "update", None, &ImpactOptions::default())
            .unwrap();
        assert_eq!(names(&im.upstream), ["Web.go"]);
        assert_eq!(im.upstream.below_confidence, 1, "only Weak.w, a symbol");
        assert_eq!(im.undecided_calls, 2, "the two calls of Api.handle");
        // Listed with `low`: nothing left to count.
        let low = ImpactOptions {
            min_confidence: MinConfidence::Low,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "update", None, &low)
            .unwrap();
        assert_eq!(im.undecided_calls, 0);
        assert_eq!(im.upstream.below_confidence, 0);
    }

    /// Review of TASK-009, NIT 2: a caller listed at level 1 only through an
    /// undecided call, and reached at level 2 by a decided edge, is that
    /// decided node (no longer `undecided`) and is walked.
    #[test]
    fn an_unsure_caller_reached_later_by_a_decided_edge_is_walked() {
        let store = Store::open_in_memory(3).unwrap();
        let mut unsure = call("Api.handle", "update");
        unsure.resolved = false;
        unsure.confidence = None;
        unsure.resolution = None;
        let edges = vec![
            call("Web.go", "Svc.update"),
            unsure,
            call("Api.handle", "Web.go"),
            call("Top.t", "Api.handle"),
        ];
        symbol_graph(&store, &edges, &[]);
        let low = ImpactOptions {
            min_confidence: MinConfidence::Low,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "update", None, &low)
            .unwrap();
        assert_eq!(names(&im.upstream), ["Web.go", "Api.handle", "Top.t"]);
        let n = &im.upstream.nodes[1];
        assert!(!n.undecided() && n.depth == 2, "{n:?}");
        assert_eq!(n.confidence.as_deref(), Some("high"));
        assert_eq!(im.upstream.nodes[2].depth, 3);
    }

    /// Review of TASK-009, m4: a qualified name names its definitions, all
    /// of them: `Inner.m` for `Outer.Inner.m` in a mutual recursion is not
    /// its own caller.
    #[test]
    fn a_qualified_name_is_never_its_own_caller() {
        let store = Store::open_in_memory(3).unwrap();
        symbol_graph(
            &store,
            &[
                call("Outer.Inner.m", "Outer.Inner.n"),
                call("Outer.Inner.n", "Outer.Inner.m"),
            ],
            &[],
        );
        for name in ["Inner.m", "Outer.Inner.m"] {
            let im = store
                .impact_graph("repo", "main", name, None, &ImpactOptions::default())
                .unwrap();
            assert_eq!(names(&im.upstream), ["Outer.Inner.n"], "{name}");
            assert_eq!(names(&im.downstream), ["Outer.Inner.n"], "{name}");
        }
    }

    /// Review m3: every frontier form reads the same rows, also over a
    /// frontier wider than the `IN` list of `Auto` takes and over ids above
    /// `i64::MAX` (which type a bare `IN` list as `HUGEINT`).
    #[test]
    fn every_frontier_form_reads_the_same_wide_frontier_of_huge_ids() {
        let store = Store::open_in_memory(3).unwrap();
        let n = AUTO_IN_MAX + 476;
        let mut edges = Vec::new();
        for i in 0..n {
            edges.push(call(&format!("C{i:04}.c"), "Core.run"));
            edges.push(call(&format!("D{i:04}.d"), &format!("C{i:04}.c")));
        }
        let ids = symbol_graph_from(&store, &edges, &[], u64::MAX - 10 * n as u64);
        assert!(ids.values().all(|&i| i > i64::MAX as u64));
        let opts = ImpactOptions {
            depth: 2,
            max_nodes: 0,
            ..Default::default()
        };
        let mut seen: Vec<Vec<String>> = Vec::new();
        for mode in [
            FrontierSql::InList,
            FrontierSql::InListTyped,
            FrontierSql::ListParam,
            FrontierSql::TempTable,
            FrontierSql::Auto,
        ] {
            let im = store
                .impact_graph_with("repo", "main", "Core.run", None, &opts, mode)
                .unwrap();
            assert_eq!(im.upstream.nodes.len(), 2 * n, "{mode:?}");
            assert_eq!(im.upstream.widest, n, "{mode:?}");
            seen.push(names(&im.upstream).iter().map(|s| s.to_string()).collect());
        }
        assert!(seen.windows(2).all(|w| w[0] == w[1]));
    }

    /// A symbol graph with types (PLAN-009 TASK-017): every method named in
    /// `calls` or `methods` (`Type.m`) is a `method` of its type, whose kind
    /// is `interface` for a name like `IService` and `class` otherwise;
    /// `supers` are `(sub, super, kind, confidence)` edges between types.
    /// A method's signature is `methods`' or `public void m(String id)`. A
    /// key may carry `@tag` (`IRepo.save@User`): another symbol with the same
    /// qualified name, an overload.
    pub(crate) fn typed_graph(
        store: &Store,
        supers: &[(&str, &str, &str, &str)],
        methods: &[(&str, &str)],
        calls: &[E],
    ) -> BTreeMap<String, u64> {
        let mut ids: BTreeMap<String, u64> = BTreeMap::new();
        let mut order: Vec<(String, bool)> = Vec::new();
        let mut add = |q: &str, is_type: bool, ids: &mut BTreeMap<String, u64>| {
            if !ids.contains_key(q) {
                ids.insert(q.to_string(), 5000 + ids.len() as u64);
                order.push((q.to_string(), is_type));
            }
        };
        let untag = |q: &str| q.split('@').next().unwrap_or(q).to_string();
        let type_of = |q: &str| untag(q).rsplit_once('.').map(|(t, _)| t.to_string());
        let sigs: HashMap<&str, &str> = methods.iter().copied().collect();
        let mut named: Vec<String> = methods.iter().map(|(q, _)| q.to_string()).collect();
        for e in calls {
            named.push(e.src.clone());
            if e.resolved {
                named.push(e.dst.clone());
            }
        }
        for (sub, sup, _, _) in supers {
            add(sub, true, &mut ids);
            add(sup, true, &mut ids);
        }
        for q in &named {
            if let Some(t) = type_of(q) {
                add(&t, true, &mut ids);
            }
            add(q, false, &mut ids);
        }
        let mut by_file: BTreeMap<String, Vec<StoredSymbol>> = BTreeMap::new();
        for (i, (q, is_type)) in order.iter().enumerate() {
            let file = file_of(q);
            let qualified = untag(q);
            let name = qualified.rsplit('.').next().unwrap().to_string();
            let interface = name.len() > 1
                && name.starts_with('I')
                && name[1..2].chars().all(char::is_uppercase);
            let (kind, parent, signature) = if *is_type {
                let kind = if interface { "interface" } else { "class" };
                (kind, None, format!("public {kind} {name}"))
            } else {
                let sig = sigs
                    .get(q.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("public void {name}(String id)"));
                ("method", type_of(q).map(|t| ids[&t]), sig)
            };
            by_file.entry(file.clone()).or_default().push(StoredSymbol {
                id: ids[q],
                parent_id: parent,
                file: file.clone(),
                kind: kind.into(),
                name,
                qualified,
                signature: Some(signature),
                start_line: 10 + i as i32,
                end_line: 12 + i as i32,
                is_test: file.contains("/test/"),
                ..Default::default()
            });
        }
        for (file, syms) in &by_file {
            store
                .replace_file_symbols("repo", "main", file, syms)
                .unwrap();
        }
        let mut edges_by_file: BTreeMap<String, Vec<StoredSymbolEdge>> = BTreeMap::new();
        for e in calls {
            let file = file_of(&e.src);
            edges_by_file
                .entry(file.clone())
                .or_default()
                .push(StoredSymbolEdge {
                    kind: "calls".into(),
                    src_id: ids[&e.src],
                    dst_id: e.resolved.then(|| ids[&e.dst]),
                    dst_name: e.dst.clone(),
                    file: file.clone(),
                    line: e.line,
                    confidence: e.confidence.map(str::to_string),
                    resolution: e.resolution.map(str::to_string),
                    external: Some(e.external),
                    from_test: file.contains("/test/"),
                    edge_source: "treesitter".into(),
                    hint: None,
                });
        }
        for (sub, sup, kind, conf) in supers {
            let file = file_of(sub);
            edges_by_file
                .entry(file.clone())
                .or_default()
                .push(StoredSymbolEdge {
                    kind: kind.to_string(),
                    src_id: ids[*sub],
                    dst_id: Some(ids[*sup]),
                    dst_name: sup.to_string(),
                    file: file.clone(),
                    line: 1,
                    confidence: Some(conf.to_string()),
                    resolution: Some("same_package".into()),
                    external: Some(false),
                    from_test: file.contains("/test/"),
                    edge_source: "treesitter".into(),
                    hint: None,
                });
        }
        for (file, rows) in &edges_by_file {
            store
                .replace_file_symbol_edges("repo", "main", file, rows)
                .unwrap();
        }
        ids
    }

    fn node<'a>(side: &'a ImpactSide, symbol: &str) -> &'a ImpactNode {
        side.nodes
            .iter()
            .find(|n| n.symbol == symbol)
            .unwrap_or_else(|| panic!("no {symbol} in {:?}", names(side)))
    }

    /// `IService` ← `ServiceImpl`; `Api.handle` calls `IService.update`
    /// (a field typed by the interface), `ServiceImpl.update` calls
    /// `Repo.save`.
    fn di_graph(store: &Store, extra: &[E]) -> BTreeMap<String, u64> {
        let mut calls = vec![
            call("Api.handle", "IService.update"),
            call("ServiceImpl.update", "Repo.save"),
        ];
        calls.extend_from_slice(extra);
        typed_graph(
            store,
            &[("ServiceImpl", "IService", "implements", "high")],
            &[("IService.update", "void update(String id);")],
            &calls,
        )
    }

    /// TASK-017 (b-up): upstream, `Impl.m` reaches the callers of the
    /// methods it overrides, through dispatch: `medium`, never `high`, at the
    /// depth of a direct caller, with the method it went through.
    #[test]
    fn upstream_dispatch_reaches_the_callers_of_the_overridden_method() {
        let store = Store::open_in_memory(3).unwrap();
        di_graph(&store, &[]);
        let im = store
            .impact_graph(
                "repo",
                "main",
                "ServiceImpl.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert_eq!(names(&im.upstream), ["Api.handle"]);
        let n = node(&im.upstream, "Api.handle");
        assert_eq!(n.via, DISPATCH_VIA);
        assert_eq!(n.confidence.as_deref(), Some("medium"));
        assert_eq!(n.depth, 1);
        assert_eq!(n.through.as_deref(), Some("IService.update"));
        // Its callees are what they were.
        assert_eq!(names(&im.downstream), ["Repo.save"]);
        // `dispatch: false` is the walk of TASK-009.
        let off = ImpactOptions {
            dispatch: false,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "ServiceImpl.update", None, &off)
            .unwrap();
        assert!(im.upstream.nodes.is_empty());
    }

    /// TASK-017 (b-down): downstream, an interface method reaches its
    /// implementations (one level below it), and their callees after them.
    #[test]
    fn downstream_dispatch_reaches_the_implementations() {
        let store = Store::open_in_memory(3).unwrap();
        di_graph(&store, &[]);
        let im = store
            .impact_graph(
                "repo",
                "main",
                "IService.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert_eq!(names(&im.downstream), ["ServiceImpl.update", "Repo.save"]);
        let n = node(&im.downstream, "ServiceImpl.update");
        assert_eq!((n.via.as_str(), n.depth), (DISPATCH_VIA, 1));
        assert_eq!(n.confidence.as_deref(), Some("medium"));
        assert_eq!(n.through.as_deref(), Some("IService.update"));
        assert_eq!(node(&im.downstream, "Repo.save").depth, 2);
        // The direct caller of the interface method is upstream, as before.
        assert_eq!(names(&im.upstream), ["Api.handle"]);
        assert_eq!(im.upstream.nodes[0].via, "calls");
        // From a caller: `Api.handle` → `IService.update` (1) → the
        // implementation (2), no surer than the call that reached it.
        let mut weak = call("Web.go", "IService.update");
        weak.confidence = Some("medium");
        let store = Store::open_in_memory(3).unwrap();
        di_graph(&store, &[weak]);
        let im = store
            .impact_graph("repo", "main", "Web.go", None, &ImpactOptions::default())
            .unwrap();
        assert_eq!(
            names(&im.downstream),
            ["IService.update", "ServiceImpl.update", "Repo.save"]
        );
        assert_eq!(node(&im.downstream, "ServiceImpl.update").depth, 2);
    }

    /// TASK-017 (b-deep): every node of every level is expanded, not only the
    /// seed: a direct caller of `ServiceImpl.update` that is itself called
    /// through its interface brings that caller at depth 2.
    #[test]
    fn dispatch_expands_every_level_not_only_the_seed() {
        let store = Store::open_in_memory(3).unwrap();
        typed_graph(
            &store,
            &[
                ("ServiceImpl", "IService", "implements", "high"),
                ("FacadeImpl", "IFacade", "implements", "high"),
            ],
            &[
                ("IService.update", "void update(String id);"),
                ("IFacade.run", "void run(String id);"),
            ],
            &[
                call("FacadeImpl.run", "ServiceImpl.update"),
                call("Api.handle", "IFacade.run"),
                call("Top.go", "Api.handle"),
            ],
        );
        let im = store
            .impact_graph(
                "repo",
                "main",
                "ServiceImpl.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert_eq!(
            names(&im.upstream),
            ["FacadeImpl.run", "Api.handle", "Top.go"]
        );
        let n = node(&im.upstream, "Api.handle");
        assert_eq!((n.via.as_str(), n.depth), (DISPATCH_VIA, 2));
        assert_eq!(n.through.as_deref(), Some("IFacade.run"));
        // After the dispatch, a direct edge is what it is.
        let t = node(&im.upstream, "Top.go");
        assert_eq!((t.via.as_str(), t.depth), ("calls", 3));
        // Downstream, at depth 2: `Api.handle` → `IFacade.run` →
        // `FacadeImpl.run` → `ServiceImpl.update`.
        let im = store
            .impact_graph(
                "repo",
                "main",
                "Api.handle",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert_eq!(
            names(&im.downstream),
            ["IFacade.run", "FacadeImpl.run", "ServiceImpl.update"]
        );
        assert_eq!(node(&im.downstream, "FacadeImpl.run").depth, 2);
    }

    /// TASK-017 (b-chain): the chain of supertypes is transitive — an
    /// implementation of an abstract class that implements an interface is
    /// reached through the interface — and capped at `DISPATCH_DEPTH`
    /// levels.
    #[test]
    fn dispatch_climbs_a_chain_of_supertypes_up_to_its_cap() {
        let store = Store::open_in_memory(3).unwrap();
        typed_graph(
            &store,
            &[
                ("ServiceImpl", "BaseService", "inherits", "high"),
                ("BaseService", "IService", "implements", "high"),
                ("IService", "IBase", "inherits", "high"),
                ("IBase", "IRoot", "inherits", "high"),
                ("IRoot", "ITop", "inherits", "high"),
            ],
            &[
                (
                    "BaseService.update",
                    "public abstract void update(String id);",
                ),
                ("ServiceImpl.update", "public void update(String id)"),
                ("IService.update", "void update(String id);"),
                ("IBase.update", "void update(String id);"),
                ("IRoot.update", "void update(String id);"),
                ("ITop.update", "void update(String id);"),
            ],
            &[
                call("Api.viaBase", "BaseService.update"),
                call("Api.viaInterface", "IService.update"),
                call("Api.viaRoot", "IRoot.update"),
                call("Api.viaTop", "ITop.update"),
            ],
        );
        let im = store
            .impact_graph(
                "repo",
                "main",
                "ServiceImpl.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        // `ITop` is five levels up: past `DISPATCH_DEPTH` (4).
        assert_eq!(crate::DISPATCH_DEPTH, 4);
        assert_eq!(
            names(&im.upstream),
            ["Api.viaBase", "Api.viaInterface", "Api.viaRoot"]
        );
        assert!(im
            .upstream
            .nodes
            .iter()
            .all(|n| n.via == DISPATCH_VIA && n.depth == 1));
        assert_eq!(
            node(&im.upstream, "Api.viaInterface").through.as_deref(),
            Some("IService.update")
        );
        // Downstream the other way: the interface reaches the abstract
        // method and the implementation.
        let im = store
            .impact_graph(
                "repo",
                "main",
                "IService.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert_eq!(
            names(&im.downstream),
            ["BaseService.update", "ServiceImpl.update"]
        );
    }

    /// Review of TASK-017, point 6: what the depth cap of the supertype
    /// chain leaves out is counted, not dropped in silence — `ITop.update`,
    /// five levels above, upstream; `ServiceImpl.update`, five below,
    /// downstream — and never followed.
    #[test]
    fn the_dispatch_depth_cap_is_counted() {
        let store = Store::open_in_memory(3).unwrap();
        typed_graph(
            &store,
            &[
                ("ServiceImpl", "BaseService", "inherits", "high"),
                ("BaseService", "IService", "implements", "high"),
                ("IService", "IBase", "inherits", "high"),
                ("IBase", "IRoot", "inherits", "high"),
                ("IRoot", "ITop", "inherits", "high"),
            ],
            &[
                (
                    "BaseService.update",
                    "public abstract void update(String id);",
                ),
                ("ServiceImpl.update", "public void update(String id)"),
                ("IService.update", "void update(String id);"),
                ("IBase.update", "void update(String id);"),
                ("IRoot.update", "void update(String id);"),
                ("ITop.update", "void update(String id);"),
                ("ITop.other", "void other(String id);"),
            ],
            &[call("Api.viaTop", "ITop.update")],
        );
        let im = store
            .impact_graph(
                "repo",
                "main",
                "ServiceImpl.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert!(!names(&im.upstream).contains(&"Api.viaTop"));
        assert_eq!(im.upstream.dispatch_beyond_depth, 1);
        assert_eq!(im.upstream.dispatch_capped, 0);
        // Downstream at depth 1 (at depth 2 `IRoot.update` would reach it
        // within its own cap, and then it is listed, not counted).
        let one = ImpactOptions {
            depth: 1,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "ITop.update", None, &one)
            .unwrap();
        assert_eq!(
            names(&im.downstream),
            [
                "BaseService.update",
                "IBase.update",
                "IRoot.update",
                "IService.update"
            ]
        );
        assert_eq!(im.downstream.dispatch_beyond_depth, 1);
        let im = store
            .impact_graph(
                "repo",
                "main",
                "ITop.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert!(names(&im.downstream).contains(&"ServiceImpl.update"));
        assert_eq!(im.downstream.dispatch_beyond_depth, 0);
        // Within the cap nothing is counted.
        let im = store
            .impact_graph(
                "repo",
                "main",
                "IService.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert_eq!(im.downstream.dispatch_beyond_depth, 0);
    }

    /// TASK-017 (c): an interface with many implementations does not explode:
    /// `DISPATCH_MAX_PER_NODE` per node, by rank and then name, the rest
    /// counted in `dispatch_capped` and not followed.
    #[test]
    fn many_implementations_are_capped_and_counted() {
        let store = Store::open_in_memory(3).unwrap();
        let impls: Vec<String> = (0..40).map(|i| format!("Impl{i:02}")).collect();
        let supers: Vec<(&str, &str, &str, &str)> = impls
            .iter()
            .map(|i| (i.as_str(), "IRepo", "implements", "high"))
            .collect();
        let methods: Vec<String> = impls.iter().map(|i| format!("{i}.save")).collect();
        let mut sigs: Vec<(&str, &str)> = methods
            .iter()
            .map(|m| (m.as_str(), "public void save(String id)"))
            .collect();
        sigs.push(("IRepo.save", "void save(String id);"));
        typed_graph(&store, &supers, &sigs, &[call("Impl39.save", "Deep.x")]);
        let im = store
            .impact_graph(
                "repo",
                "main",
                "IRepo.save",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        let cap = crate::DISPATCH_MAX_PER_NODE;
        assert_eq!(
            im.downstream.nodes.len(),
            cap,
            "{:?}",
            names(&im.downstream)
        );
        assert_eq!(im.downstream.nodes[0].symbol, "Impl00.save");
        assert_eq!(im.downstream.dispatch_capped, 40 - cap);
        assert!(
            !names(&im.downstream).contains(&"Deep.x"),
            "a cut implementation is not followed"
        );
        // `max_nodes` is not what cut them.
        assert_eq!(im.downstream.capped, 0);
    }

    /// TASK-017 (c'): `min(medium, original)` — a `low` call stays `low`
    /// through dispatch (counted by default, listed with `low`), and
    /// `min_confidence: high` leaves every dispatch out and counts it.
    #[test]
    fn a_low_edge_stays_low_through_dispatch_and_high_excludes_it() {
        let store = Store::open_in_memory(3).unwrap();
        let mut weak = call("Weak.w", "IService.update");
        weak.confidence = Some("low");
        di_graph(&store, &[weak]);
        let im = store
            .impact_graph(
                "repo",
                "main",
                "ServiceImpl.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert_eq!(names(&im.upstream), ["Api.handle"]);
        assert_eq!(im.upstream.below_confidence, 1);
        let low = ImpactOptions {
            min_confidence: MinConfidence::Low,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "ServiceImpl.update", None, &low)
            .unwrap();
        let w = node(&im.upstream, "Weak.w");
        assert_eq!(
            (w.via.as_str(), w.confidence.as_deref()),
            (DISPATCH_VIA, Some("low"))
        );
        let high = ImpactOptions {
            min_confidence: MinConfidence::High,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "ServiceImpl.update", None, &high)
            .unwrap();
        assert!(im.upstream.nodes.is_empty());
        // Review MAJOR 2: with `high` no dispatch can show, so none is read
        // (and none counted).
        assert_eq!(im.upstream.below_confidence, 0);
        // No edge through dispatch is ever `high`, whatever the filters.
        for opts in [ImpactOptions::default(), all_in()] {
            for name in ["ServiceImpl.update", "IService.update", "Api.handle"] {
                let im = store
                    .impact_graph("repo", "main", name, None, &opts)
                    .unwrap();
                for n in im.upstream.nodes.iter().chain(&im.downstream.nodes) {
                    if n.via == DISPATCH_VIA {
                        assert_ne!(n.confidence.as_deref(), Some("high"), "{name}: {n:?}");
                    }
                }
            }
        }
    }

    /// TASK-017: an override-equivalent has the same name and an arity that
    /// can match; a private or static method overrides nothing.
    #[test]
    fn only_override_equivalent_methods_are_dispatched_to() {
        let store = Store::open_in_memory(3).unwrap();
        typed_graph(
            &store,
            &[("ServiceImpl", "IService", "implements", "high")],
            &[
                ("IService.update", "void update(String id);"),
                ("ServiceImpl.update", "public void update(String id, int n)"),
                ("IService.load", "void load(String id);"),
                ("ServiceImpl.load", "private void load(String id)"),
                ("IService.find", "void find(String id);"),
                ("ServiceImpl.find", "public Item find(String id)"),
            ],
            &[
                call("Api.a", "IService.update"),
                call("Api.b", "IService.load"),
                call("Api.c", "IService.find"),
            ],
        );
        let up = |s: &str| {
            let im = store
                .impact_graph("repo", "main", s, None, &ImpactOptions::default())
                .unwrap();
            names(&im.upstream)
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        };
        assert!(up("ServiceImpl.update").is_empty(), "another arity");
        assert!(up("ServiceImpl.load").is_empty(), "private");
        assert_eq!(up("ServiceImpl.find"), ["Api.c"]);
    }

    /// TASK-017, lesson 6 (and review MAJOR 2): at most one more statement
    /// per level and direction, whatever the frontier — the supertype graph
    /// is read once per call, as id pairs, and each level reads only the
    /// methods with a frontier method's name in a type of it.
    #[test]
    fn dispatch_costs_at_most_one_statement_per_level() {
        let store = Store::open_in_memory(3).unwrap();
        let mut calls = Vec::new();
        for i in 0..300 {
            calls.push(call(&format!("A{i}.a"), "IService.update"));
            calls.push(call(&format!("B{i}.b"), &format!("A{i}.a")));
        }
        calls.push(call("Lone.x", "Lone.y"));
        di_graph(&store, &calls);
        // A walk whose nodes belong to no type with a supertype or a subtype
        // pays for no dispatch statement, though the branch has some.
        LEVEL_STATEMENTS.with(|c| c.set(0));
        let im = store
            .impact_graph("repo", "main", "Lone.x", None, &ImpactOptions::default())
            .unwrap();
        assert_eq!(names(&im.downstream), ["Lone.y"]);
        let levels = im.upstream.levels + im.downstream.levels;
        assert_eq!(LEVEL_STATEMENTS.with(|c| c.get()), levels);
        LEVEL_STATEMENTS.with(|c| c.set(0));
        crate::dispatch::INDEX_READS.with(|c| c.set(0));
        let opts = ImpactOptions {
            max_nodes: 0,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "ServiceImpl.update", None, &opts)
            .unwrap();
        assert_eq!(im.upstream.nodes.len(), 601);
        let n = LEVEL_STATEMENTS.with(|c| c.get());
        let levels = im.upstream.levels + im.downstream.levels;
        assert!(n <= 2 * levels, "{n} statements for {levels} levels");
        assert_eq!(crate::dispatch::INDEX_READS.with(|c| c.get()), 1);
        // Review MAJOR 2: nothing is read when dispatch cannot show — off,
        // `min_confidence: high`, or no definition to start from.
        let reads = |name: &str, opts: &ImpactOptions| {
            crate::dispatch::INDEX_READS.with(|c| c.set(0));
            store
                .impact_graph("repo", "main", name, None, opts)
                .unwrap();
            crate::dispatch::INDEX_READS.with(|c| c.get())
        };
        let off = ImpactOptions {
            dispatch: false,
            ..Default::default()
        };
        let high = ImpactOptions {
            min_confidence: MinConfidence::High,
            ..Default::default()
        };
        assert_eq!(reads("ServiceImpl.update", &off), 0);
        assert_eq!(reads("ServiceImpl.update", &high), 0);
        assert_eq!(reads("nothingHere", &ImpactOptions::default()), 0);
        assert_eq!(reads("ServiceImpl.update", &ImpactOptions::default()), 1);
    }

    /// TASK-017: downstream, an implementation is no surer than the edge
    /// that reached the method it was dispatched from — a `low` call to the
    /// interface gives a `low` implementation, never `medium`.
    #[test]
    fn a_downstream_dispatch_is_no_surer_than_what_reached_its_method() {
        let store = Store::open_in_memory(3).unwrap();
        let mut weak = call("Weak.w", "IService.update");
        weak.confidence = Some("low");
        di_graph(&store, &[weak]);
        let low = ImpactOptions {
            min_confidence: MinConfidence::Low,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "Weak.w", None, &low)
            .unwrap();
        let n = node(&im.downstream, "ServiceImpl.update");
        assert_eq!((n.via.as_str(), n.depth), (DISPATCH_VIA, 2));
        assert_eq!(n.confidence.as_deref(), Some("low"));
        // By default the interface method is counted, and nothing below it
        // is read.
        let im = store
            .impact_graph("repo", "main", "Weak.w", None, &ImpactOptions::default())
            .unwrap();
        assert!(im.downstream.nodes.is_empty());
        assert_eq!(im.downstream.below_confidence, 1);
    }

    /// TASK-017, lesson 4: a supertype edge the link pass discarded is never
    /// followed (only `live_edges`).
    #[test]
    fn a_discarded_supertype_edge_dispatches_nothing() {
        let store = Store::open_in_memory(3).unwrap();
        // Another, live, supertype edge: the branch has dispatch to follow.
        typed_graph(
            &store,
            &[
                ("ServiceImpl", "IService", "implements", "high"),
                ("OtherImpl", "IOther", "implements", "high"),
            ],
            &[("IService.update", "void update(String id);")],
            &[
                call("Api.handle", "IService.update"),
                call("ServiceImpl.update", "Repo.save"),
            ],
        );
        let file = "src/main/Main.java";
        let mut rows = store.file_symbol_edges("repo", "main", file).unwrap();
        for e in rows
            .iter_mut()
            .filter(|e| e.kind == "implements" && e.dst_name == "IService")
        {
            e.resolution = Some("discarded".into());
        }
        store
            .replace_file_symbol_edges("repo", "main", file, &rows)
            .unwrap();
        let im = store
            .impact_graph("repo", "main", "ServiceImpl.update", None, &all_in())
            .unwrap();
        assert!(im.upstream.nodes.is_empty(), "{:?}", names(&im.upstream));
        let im = store
            .impact_graph("repo", "main", "IService.update", None, &all_in())
            .unwrap();
        assert!(!names(&im.downstream).contains(&"ServiceImpl.update"));
        // With every supertype edge discarded, the branch has no dispatch
        // to follow: no level pays for the dispatch statement.
        let mut rows = store.file_symbol_edges("repo", "main", file).unwrap();
        for e in rows.iter_mut().filter(|e| e.kind == "implements") {
            e.resolution = Some("discarded".into());
        }
        store
            .replace_file_symbol_edges("repo", "main", file, &rows)
            .unwrap();
        LEVEL_STATEMENTS.with(|c| c.set(0));
        let im = store
            .impact_graph(
                "repo",
                "main",
                "ServiceImpl.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        let levels = im.upstream.levels + im.downstream.levels;
        assert_eq!(LEVEL_STATEMENTS.with(|c| c.get()), levels);
    }

    /// TASK-017: a caller reached both directly and through dispatch with
    /// the same confidence is the direct one, whatever its kind (an
    /// `instantiates` sorts after `dispatch` by name).
    #[test]
    fn a_direct_edge_beats_a_dispatch_of_the_same_confidence() {
        let store = Store::open_in_memory(3).unwrap();
        let mut direct = call("Api.handle", "ServiceImpl.update");
        direct.confidence = Some("medium");
        direct.line = 7;
        di_graph(&store, &[direct]);
        let file = "src/main/Main.java";
        let mut rows = store.file_symbol_edges("repo", "main", file).unwrap();
        for e in rows.iter_mut().filter(|e| e.line == 7) {
            e.kind = "instantiates".into();
        }
        store
            .replace_file_symbol_edges("repo", "main", file, &rows)
            .unwrap();
        let im = store
            .impact_graph(
                "repo",
                "main",
                "ServiceImpl.update",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        let n = node(&im.upstream, "Api.handle");
        assert_eq!(
            (n.via.as_str(), n.through.as_deref()),
            ("instantiates", None)
        );
        assert_eq!(n.confidence.as_deref(), Some("medium"));
    }

    /// TASK-017, lesson 2: the asserts of the 0.9.0 impact tests hold with
    /// dispatch on, over a graph where it reaches something (`Office` has a
    /// subclass overriding `persist`): dispatch only adds.
    #[test]
    fn the_0_9_0_impact_asserts_hold_with_dispatch() {
        use crate::graph::tests as old;
        let mut flush = call("Office.persist", "flush");
        flush.resolved = false;
        flush.confidence = None;
        flush.resolution = None;
        let mut deeper = call("Repo.flush", "noDebeAparecer");
        deeper.resolved = false;
        let calls = vec![
            call("OfficeResource.actualizar", "OfficeService.actualizar"),
            call("TicketResource.actualizar", "TicketService.actualizar"),
            call("OfficeService.actualizar", "Office.persist"),
            flush,
            deeper,
        ];
        let store = Store::open_in_memory(3).unwrap();
        typed_graph(
            &store,
            &[("OfficeDraft", "Office", "inherits", "high")],
            &[
                ("Office.persist", "public void persist()"),
                ("OfficeDraft.persist", "public void persist()"),
            ],
            &calls,
        );
        for opts in [ImpactOptions::default(), all_in()] {
            assert!(opts.dispatch);
            old::check_impact_seeds_from_every_form_of_a_bare_name(&as_0_9_0(&store, opts));
            let resolve = |s: &str| {
                store
                    .impact_graph("repo", "main", s, None, &opts)
                    .unwrap()
                    .resolved
            };
            old::check_resolving_a_bare_name_lists_every_declaration_it_could_mean(&resolve);
            let callers = |s: &str| -> Vec<String> {
                let walk = as_0_9_0(&store, opts);
                walk(s, 1).upstream.into_iter().map(|(n, _)| n).collect()
            };
            old::check_a_qualified_name_does_not_collect_its_homonyms(&resolve, &callers);
            // Dispatch did reach something here.
            let im = store
                .impact_graph("repo", "main", "OfficeService.actualizar", None, &opts)
                .unwrap();
            let n = node(&im.downstream, "OfficeDraft.persist");
            assert_eq!((n.via.as_str(), n.depth), (DISPATCH_VIA, 2));
        }
        old::check_traversal_does_not_re_expand_the_names_it_walks(&as_0_9_0(&store, all_in()));
    }

    /// Review of TASK-017, NIT: the BFS asserts of 0.9.0 hold on a branch
    /// whose supertype graph is not empty — the walk then goes through
    /// `dispatch_level` at every level (the index is read), not through the
    /// plain level query the `seeded_graph` of the other test takes.
    #[test]
    fn the_0_9_0_bfs_asserts_hold_with_a_supertype_graph() {
        use crate::graph::tests as old;
        let store = Store::open_in_memory(3).unwrap();
        typed_graph(
            &store,
            &[("ServiceImpl", "IService", "implements", "high")],
            &[("IService.update", "void update(String id);")],
            &[
                call("a", "b"),
                call("a", "d"),
                call("b", "c"),
                call("Api.handle", "IService.update"),
            ],
        );
        crate::dispatch::INDEX_READS.with(|c| c.set(0));
        for opts in [ImpactOptions::default(), all_in()] {
            assert!(opts.dispatch);
            old::check_impact_bfs_upstream_and_downstream(&as_0_9_0(&store, opts));
        }
        assert!(
            crate::dispatch::INDEX_READS.with(|c| c.get()) > 0,
            "the supertype graph must be read"
        );
    }

    /// Review of TASK-017, NIT: a diamond of supertypes (`Impl` implements
    /// `ILeft` and `IRight`, both extend `IRoot`) reaches `IRoot.run` by two
    /// chains: its caller is listed once, by the surest chain, upstream; and
    /// downstream every method of the diamond is listed (or counted) once.
    #[test]
    fn a_diamond_of_supertypes_lists_each_method_once() {
        let store = Store::open_in_memory(3).unwrap();
        typed_graph(
            &store,
            &[
                ("Impl", "ILeft", "implements", "high"),
                ("Impl", "IRight", "implements", "high"),
                ("ILeft", "IRoot", "inherits", "low"),
                ("IRight", "IRoot", "inherits", "high"),
            ],
            &[
                ("IRoot.run", "void run(String id);"),
                ("ILeft.run", "void run(String id);"),
                ("IRight.run", "void run(String id);"),
                ("Impl.run", "public void run(String id)"),
            ],
            &[
                call("Api.viaRoot", "IRoot.run"),
                call("Api.viaLeft", "ILeft.run"),
                call("Api.viaRight", "IRight.run"),
            ],
        );
        let im = store
            .impact_graph("repo", "main", "Impl.run", None, &ImpactOptions::default())
            .unwrap();
        assert_eq!(
            names(&im.upstream),
            ["Api.viaLeft", "Api.viaRight", "Api.viaRoot"]
        );
        // `IRoot` is reached by its surest chain (through `IRight`, all
        // `high`): `medium`, the cap of a dispatch, not the `low` of the
        // `ILeft` edge.
        let n = node(&im.upstream, "Api.viaRoot");
        assert_eq!(
            (n.via.as_str(), n.confidence.as_deref(), n.depth),
            (DISPATCH_VIA, Some("medium"), 1)
        );
        let im = store
            .impact_graph("repo", "main", "IRoot.run", None, &ImpactOptions::default())
            .unwrap();
        // Downstream, `Impl.run` once (by the surest chain); `ILeft.run`
        // hangs off the `low` edge only: counted, not listed.
        let mut down = names(&im.downstream);
        down.sort_unstable();
        assert_eq!(down, ["IRight.run", "Impl.run"]);
        assert_eq!(im.downstream.below_confidence, 1);
        assert_eq!(im.downstream.dispatch_capped, 0);
    }

    /// Review of TASK-017, NIT: a cycle of supertypes (a broken hierarchy,
    /// or two generic types the resolver tied together) ends: each method
    /// of the cycle is reached once, the origin never through itself.
    #[test]
    fn a_cycle_of_supertypes_ends_and_lists_each_method_once() {
        let store = Store::open_in_memory(3).unwrap();
        typed_graph(
            &store,
            &[
                ("CycA", "CycB", "inherits", "high"),
                ("CycB", "CycC", "inherits", "high"),
                ("CycC", "CycA", "inherits", "high"),
            ],
            &[
                ("CycA.run", "public void run()"),
                ("CycB.run", "public void run()"),
                ("CycC.run", "public void run()"),
            ],
            &[call("Api.viaB", "CycB.run"), call("Api.viaC", "CycC.run")],
        );
        let im = store
            .impact_graph("repo", "main", "CycA.run", None, &ImpactOptions::default())
            .unwrap();
        assert_eq!(names(&im.upstream), ["Api.viaB", "Api.viaC"]);
        let mut down = names(&im.downstream);
        down.sort_unstable();
        assert_eq!(down, ["CycB.run", "CycC.run"]);
    }

    /// Review of TASK-017, MAJOR 1: a Rust `'static` lifetime (`-> &'static
    /// str`, `T: 'static`) is no `static` modifier — the trait method still
    /// dispatches, both ways; only a modifier before the name counts.
    #[test]
    fn a_static_lifetime_does_not_turn_dispatch_off() {
        let store = Store::open_in_memory(3).unwrap();
        typed_graph(
            &store,
            &[("FileSink", "Sink", "implements", "high")],
            &[
                ("Sink.name", "fn name(&self) -> &'static str;"),
                ("FileSink.name", "fn name(&self) -> &'static str"),
                ("Sink.spawn", "fn spawn<T: 'static + Send>(&self, t: T);"),
                ("FileSink.spawn", "fn spawn<T: 'static + Send>(&self, t: T)"),
            ],
            &[
                call("App.label", "Sink.name"),
                call("App.run", "Sink.spawn"),
            ],
        );
        for (imp, caller, through) in [
            ("FileSink.name", "App.label", "Sink.name"),
            ("FileSink.spawn", "App.run", "Sink.spawn"),
        ] {
            let im = store
                .impact_graph("repo", "main", imp, None, &ImpactOptions::default())
                .unwrap();
            assert_eq!(names(&im.upstream), [caller], "{imp}");
            let im = store
                .impact_graph("repo", "main", through, None, &ImpactOptions::default())
                .unwrap();
            assert_eq!(names(&im.downstream), [imp], "{through}");
        }
    }

    /// Review of TASK-017, MINOR 3: Java overloads of the same arity in a
    /// supertype (`save(User)`, `save(Order)`) are not both equivalent when
    /// one matches the parameter types exactly; with no exact match (a
    /// generic parameter), the arity still decides.
    #[test]
    fn an_exact_overload_wins_over_its_same_arity_siblings() {
        let store = Store::open_in_memory(3).unwrap();
        typed_graph(
            &store,
            &[
                ("RepoImpl", "IRepo", "implements", "high"),
                ("GenImpl", "IGen", "implements", "high"),
            ],
            &[
                ("IRepo.save@User", "void save(User u);"),
                ("IRepo.save@Order", "void save(Order o);"),
                (
                    "RepoImpl.save",
                    "@Override public void save(final User user)",
                ),
                ("IGen.put@T", "void put(T item);"),
                ("IGen.put@List", "void put(List<T> items);"),
                ("GenImpl.put", "public void put(Item item)"),
            ],
            &[
                call("Api.a", "IRepo.save@User"),
                call("Api.b", "IRepo.save@Order"),
                call("Api.c", "IGen.put@T"),
                call("Api.d", "IGen.put@List"),
            ],
        );
        let up = |s: &str| {
            let im = store
                .impact_graph("repo", "main", s, None, &ImpactOptions::default())
                .unwrap();
            names(&im.upstream)
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(up("RepoImpl.save"), ["Api.a"]);
        assert_eq!(up("GenImpl.put"), ["Api.c", "Api.d"]);
    }

    /// Review of TASK-017, MINOR 5: the per-node cap never cuts a production
    /// implementation to make room for a test fake that the default leaves
    /// out anyway — 16 fakes ranked first and 16 real implementations: the
    /// 16 real ones are listed, the fakes counted as tests, nothing cut.
    #[test]
    fn the_dispatch_cap_keeps_production_before_tests() {
        let store = Store::open_in_memory(3).unwrap();
        let types: Vec<String> = (0..16)
            .flat_map(|i| [format!("AaTest{i:02}"), format!("Impl{i:02}")])
            .collect();
        let supers: Vec<(&str, &str, &str, &str)> = types
            .iter()
            .map(|t| (t.as_str(), "IRepo", "implements", "high"))
            .collect();
        let methods: Vec<String> = types.iter().map(|t| format!("{t}.save")).collect();
        let mut sigs: Vec<(&str, &str)> = methods
            .iter()
            .map(|m| (m.as_str(), "public void save(String id)"))
            .collect();
        sigs.push(("IRepo.save", "void save(String id);"));
        typed_graph(&store, &supers, &sigs, &[]);
        let im = store
            .impact_graph(
                "repo",
                "main",
                "IRepo.save",
                None,
                &ImpactOptions::default(),
            )
            .unwrap();
        assert_eq!(im.downstream.nodes.len(), 16, "{:?}", names(&im.downstream));
        assert!(im
            .downstream
            .nodes
            .iter()
            .all(|n| n.symbol.starts_with("Impl")));
        assert_eq!(im.downstream.tests, 16);
        assert_eq!(im.downstream.dispatch_capped, 0);
        // With the tests listed, production still comes first under the cap.
        let all = ImpactOptions {
            include_tests: true,
            ..Default::default()
        };
        let im = store
            .impact_graph("repo", "main", "IRepo.save", None, &all)
            .unwrap();
        assert!(im
            .downstream
            .nodes
            .iter()
            .all(|n| n.symbol.starts_with("Impl")));
        assert_eq!(im.downstream.dispatch_capped, 16);
    }

    /// Review of TASK-017, MINOR 7: a method the cap of one node cut and that
    /// another node reaches as `low` is counted once, in `below_confidence`,
    /// not also in `dispatch_capped`.
    #[test]
    fn a_method_cut_by_the_cap_and_left_out_elsewhere_is_counted_once() {
        let store = Store::open_in_memory(3).unwrap();
        let impls: Vec<String> = (0..17).map(|i| format!("Impl{i:02}")).collect();
        let mut supers: Vec<(&str, &str, &str, &str)> = impls
            .iter()
            .map(|t| (t.as_str(), "IBig", "implements", "high"))
            .collect();
        supers.push(("Impl16", "ISmall", "implements", "low"));
        let methods: Vec<String> = impls.iter().map(|t| format!("{t}.run")).collect();
        let mut sigs: Vec<(&str, &str)> = methods
            .iter()
            .map(|m| (m.as_str(), "public void run(String id)"))
            .collect();
        sigs.push(("IBig.run", "void run(String id);"));
        sigs.push(("ISmall.run", "void run(String id);"));
        typed_graph(
            &store,
            &supers,
            &sigs,
            &[call("Top.go", "IBig.run"), call("Top.go", "ISmall.run")],
        );
        let im = store
            .impact_graph("repo", "main", "Top.go", None, &ImpactOptions::default())
            .unwrap();
        assert!(!names(&im.downstream).contains(&"Impl16.run"));
        assert_eq!(im.downstream.below_confidence, 1);
        assert_eq!(im.downstream.dispatch_capped, 0);
    }

    /// Measurement (PLAN-009 TASK-017 review, MAJOR 2; not a test): a
    /// synthetic hierarchy far wider than the measured Java repositories —
    /// 500 entities extending `BaseEntity` and implementing `IEntity` and
    /// `IAudited` (1 500 supertype edges), 40 accessors each (20 000
    /// methods) — and the time of `impact_graph` for a seed with dispatch,
    /// an interface method with 500 implementations, a seed with no
    /// hierarchy, and the same seeds with `min_confidence: high` and with
    /// `dispatch: false`. p50 / p95 over `$DEVCTX_DISPATCH_BENCH_RUNS` (30).
    ///
    /// `cargo test --release -p devctx-store --lib dispatch_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dispatch_bench() {
        let runs: usize = std::env::var("DEVCTX_DISPATCH_BENCH_RUNS")
            .ok()
            .and_then(|r| r.parse().ok())
            .unwrap_or(30);
        let store = Store::open_in_memory(3).unwrap();
        let entities: Vec<String> = (0..500).map(|i| format!("Entity{i:03}")).collect();
        let mut supers: Vec<(&str, &str, &str, &str)> = Vec::new();
        for e in &entities {
            supers.push((e.as_str(), "BaseEntity", "inherits", "high"));
            supers.push((e.as_str(), "IEntity", "implements", "high"));
            supers.push((e.as_str(), "IAudited", "implements", "high"));
        }
        let mut keys: Vec<String> = Vec::new();
        for t in entities
            .iter()
            .map(String::as_str)
            .chain(["BaseEntity", "IEntity", "IAudited"])
        {
            for a in 0..40 {
                keys.push(format!("{t}.get{a:02}"));
            }
        }
        let methods: Vec<(&str, &str)> = keys
            .iter()
            .map(|k| (k.as_str(), "public String getX()"))
            .collect();
        let mut calls = vec![
            call("Api.a", "IEntity.get00"),
            call("Api.b", "BaseEntity.get00"),
            call("Api.c", "IAudited.get00"),
            call("Top.t", "Api.a"),
            call("Plain.run", "Plain.helper"),
            call("Plain.caller", "Plain.run"),
        ];
        for (i, e) in entities.iter().enumerate().take(50) {
            calls.push(call(&format!("Svc{i:03}.use"), &format!("{e}.get01")));
        }
        let t = std::time::Instant::now();
        typed_graph(&store, &supers, &methods, &calls);
        eprintln!("graph: {:.1} s", t.elapsed().as_secs_f64());
        let high = ImpactOptions {
            min_confidence: MinConfidence::High,
            ..Default::default()
        };
        let off = ImpactOptions {
            dispatch: false,
            ..Default::default()
        };
        for (label, seed, opts) in [
            (
                "impl, dispatch up",
                "Entity000.get00",
                ImpactOptions::default(),
            ),
            (
                "interface, 500 impls",
                "IEntity.get05",
                ImpactOptions::default(),
            ),
            ("no hierarchy", "Plain.run", ImpactOptions::default()),
            ("impl, min high", "Entity000.get00", high),
            ("impl, dispatch off", "Entity000.get00", off),
            ("no hierarchy, dispatch off", "Plain.run", off),
        ] {
            let mut n = 0;
            let mut ms: Vec<f64> = (0..runs + 1)
                .map(|_| {
                    let t = std::time::Instant::now();
                    let im = store
                        .impact_graph("repo", "main", seed, None, &opts)
                        .unwrap();
                    n = im.upstream.nodes.len() + im.downstream.nodes.len();
                    t.elapsed().as_secs_f64() * 1000.0
                })
                .skip(1)
                .collect();
            ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |p: f64| ms[((ms.len() as f64 - 1.0) * p).round() as usize];
            println!(
                "dispatch_bench | {label} | {seed} | p50 {:.1} ms | p95 {:.1} ms | nodes {n}",
                pct(0.5),
                pct(0.95)
            );
        }
    }
}
