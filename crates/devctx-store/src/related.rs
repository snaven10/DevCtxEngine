//! The ego-graph of `build_context` (PLAN-009 DD-15, TASK-012): what
//! surrounds the best code hits of a brief in the symbol graph, read **per
//! batch, never per hit**.
//!
//! - [`Store::chunk_symbols`] maps chunks to the innermost symbol holding them
//!   (DD-4: by file and line range, never a `file` symbol; a `doc` chunk is
//!   the prose above the symbol that starts where it ends), one statement.
//! - [`Store::related_neighbors`] reads the neighbors of those symbols:
//!   `calls`/`instantiates` both ways and `inherits`/`implements` up (a type's
//!   supertypes, and the supertype methods a method overrides, through the
//!   dispatch index of TASK-017), over `live_edges`, with the level query of
//!   `impact_analysis` (one statement per direction and hop, whatever the
//!   number of seeds). Only `high`/`medium` edges, no tests, no externals,
//!   no `file` symbols; nothing already shown; ordered by hop, then rank;
//!   at most `per_seed` per seed. The signatures are read last, for the
//!   neighbors kept only.

use std::collections::{HashMap, HashSet};

use duckdb::params_from_iter;

use crate::error::Result;
use crate::impact::{confidence_rank, count_statement, Direction, FrontierSql, LevelRow};
use crate::store::Store;
use crate::symbols::{row_to_symbol, StoredSymbol, SYMBOL_SELECT};

/// Neighbors one seed shows at most (DD-15).
pub const RELATED_PER_SEED: usize = 4;

/// Hops the ego-graph walks at most (`graph_hops`, DD-15).
pub const RELATED_MAX_HOPS: u8 = 2;

/// The relations read in both directions.
const CALL_KINDS: &str = "('calls', 'instantiates')";

/// The relations read from a seed outward: its calls and instantiations, and
/// its supertypes.
const OUT_KINDS: &str = "('calls', 'instantiates', 'inherits', 'implements')";

/// How a neighbor relates to the seed it surrounds, read from the seed:
/// "the seed is called by …", "the seed calls …".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Relation {
    /// The neighbor calls the seed.
    CalledBy,
    /// The seed calls the neighbor.
    Calls,
    /// The neighbor instantiates the seed.
    InstantiatedBy,
    /// The seed instantiates the neighbor.
    Instantiates,
    /// The seed (a type) extends the neighbor.
    Extends,
    /// The seed (a type) implements the neighbor.
    Implements,
    /// The seed (a method) overrides the neighbor, a supertype's method.
    Overrides,
}

impl Relation {
    /// How the brief names it.
    pub fn label(self) -> &'static str {
        match self {
            Relation::CalledBy => "called by",
            Relation::Calls => "calls",
            Relation::InstantiatedBy => "instantiated by",
            Relation::Instantiates => "instantiates",
            Relation::Extends => "extends",
            Relation::Implements => "implements",
            Relation::Overrides => "overrides",
        }
    }
}

/// A chunk to map to its symbol ([`Store::chunk_symbols`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRef<'a> {
    /// Repo-relative path.
    pub file: &'a str,
    /// 1-based first line.
    pub start_line: i32,
    /// 1-based last line.
    pub end_line: i32,
    /// A `doc` chunk: the prose above a symbol, ending on its first line.
    pub doc: bool,
}

/// One neighbor of a seed.
#[derive(Debug, Clone, PartialEq)]
pub struct RelatedNeighbor {
    /// The seed it surrounds.
    pub seed: u64,
    /// The neighbor, with its signature.
    pub symbol: StoredSymbol,
    /// How it relates to the seed.
    pub relation: Relation,
    /// 1, or 2 for a neighbor of a neighbor.
    pub hops: u8,
}

/// A candidate neighbor before the signatures are read.
#[derive(Debug, Clone)]
struct Candidate {
    seed: u64,
    id: u64,
    qualified: String,
    file: String,
    line: i32,
    relation: Relation,
    hops: u8,
    confidence: u8,
    rank: Option<f64>,
}

impl Store {
    /// The innermost symbol holding each chunk, in their order (DD-4): by
    /// file and line range, never a `file` symbol; a `doc` chunk maps to the
    /// symbol that starts on its last line. One statement.
    pub fn chunk_symbols(
        &self,
        repo: &str,
        branch: &str,
        chunks: &[ChunkRef<'_>],
    ) -> Result<Vec<Option<StoredSymbol>>> {
        let mut out = vec![None; chunks.len()];
        if chunks.is_empty() {
            return Ok(out);
        }
        let values = vec!["(?, ?, ?, ?, ?)"; chunks.len()].join(", ");
        let mut args: Vec<duckdb::types::Value> = Vec::with_capacity(chunks.len() * 5 + 2);
        for (i, c) in chunks.iter().enumerate() {
            args.push((i as i64).into());
            args.push(c.file.to_string().into());
            args.push(c.start_line.into());
            args.push(c.end_line.into());
            args.push(c.doc.into());
        }
        args.push(repo.to_string().into());
        args.push(branch.to_string().into());
        let sql = format!(
            "SELECT c.i, {cols}
               FROM (VALUES {values}) AS c(i, file, lo, hi, doc)
               JOIN symbols s ON s.file = c.file
                AND ((NOT c.doc AND s.start_line <= c.lo AND s.end_line >= c.hi)
                     OR (c.doc AND s.start_line = c.hi))
              WHERE s.repo = ? AND s.branch = ? AND s.kind <> 'file'
            QUALIFY row_number() OVER (
                PARTITION BY c.i
                ORDER BY s.end_line - s.start_line, s.start_line DESC, s.id) = 1",
            cols = symbol_cols("s"),
        );
        count_statement();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            let i: i64 = r.get(0)?;
            Ok((i, row_to_symbol_at(r, 1)?))
        })?;
        for row in rows {
            let (i, sym) = row?;
            out[i as usize] = Some(sym);
        }
        Ok(out)
    }

    /// The neighbors of `seeds` (DD-15), at most `per_seed` per seed, within
    /// `hops` (1 or 2), leaving out every neighbor `shown` says the brief
    /// already holds (by file and first line) and the seeds themselves; a
    /// neighbor of several seeds goes with the first. In seed order, then hop,
    /// rank (highest first), confidence, name, file, line and id.
    pub fn related_neighbors(
        &self,
        repo: &str,
        branch: &str,
        seeds: &[StoredSymbol],
        hops: u8,
        per_seed: usize,
        shown: &dyn Fn(&str, i32) -> bool,
    ) -> Result<Vec<RelatedNeighbor>> {
        let mut order: Vec<u64> = Vec::new();
        for s in seeds {
            if !order.contains(&s.id) {
                order.push(s.id);
            }
        }
        if order.is_empty() || per_seed == 0 {
            return Ok(Vec::new());
        }
        let seed_set: HashSet<u64> = order.iter().copied().collect();
        let mode = FrontierSql::Auto;
        let mut found: Vec<Candidate> = Vec::new();
        // Hop 1: into the seeds (callers, instantiators) and out of them
        // (callees, instantiations, supertypes): two statements.
        let up = self.edge_level(repo, branch, &order, Direction::Upstream, mode, CALL_KINDS)?;
        let down = self.edge_level(repo, branch, &order, Direction::Downstream, mode, OUT_KINDS)?;
        let mut first_up: Vec<(u64, u64)> = Vec::new();
        let mut first_down: Vec<(u64, u64)> = Vec::new();
        for r in &up {
            if let Some(c) = candidate(r, r.from, Direction::Upstream, 1, &seed_set) {
                first_up.push((c.id, c.seed));
                found.push(c);
            }
        }
        for r in &down {
            if let Some(c) = candidate(r, r.from, Direction::Downstream, 1, &seed_set) {
                if matches!(c.relation, Relation::Calls | Relation::Instantiates) {
                    first_down.push((c.id, c.seed));
                }
                found.push(c);
            }
        }
        // The supertype methods a method seed overrides (TASK-017): the
        // index is one statement, and the methods one more, only when a seed
        // is a method of a type with a supertype.
        let parents: HashMap<u64, u64> = seeds
            .iter()
            .filter(|s| matches!(s.kind.as_str(), "method" | "function"))
            .filter_map(|s| s.parent_id.map(|p| (s.id, p)))
            .collect();
        if !parents.is_empty() {
            count_statement();
            if let Some(index) = self.dispatch_index(repo, branch)? {
                let mut frontier: Vec<u64> = order
                    .iter()
                    .copied()
                    .filter(|id| parents.get(id).is_some_and(|p| index.has_type(*p)))
                    .collect();
                frontier.sort_unstable();
                if !frontier.is_empty() {
                    let eq = self.override_equivalents(
                        repo,
                        branch,
                        &frontier,
                        &parents,
                        Direction::Upstream,
                        mode,
                        &index,
                        false,
                    )?;
                    for q in eq.kept {
                        // `min(medium, chain)`: kept only from a high or
                        // medium chain.
                        if q.chain < 2 || q.test || seed_set.contains(&q.id) {
                            continue;
                        }
                        let (Some(file), Some(line)) = (q.file, q.line) else {
                            continue;
                        };
                        found.push(Candidate {
                            seed: q.origin,
                            id: q.id,
                            qualified: q.symbol,
                            file,
                            line,
                            relation: Relation::Overrides,
                            hops: 1,
                            confidence: 2,
                            rank: q.rank,
                        });
                    }
                }
            }
        }
        // Hop 2: callers of the callers, callees of the callees (one
        // statement per direction).
        if hops >= 2 {
            for (list, dir, kinds) in [
                (&first_up, Direction::Upstream, CALL_KINDS),
                (&first_down, Direction::Downstream, CALL_KINDS),
            ] {
                let mut frontier: Vec<u64> = list.iter().map(|(id, _)| *id).collect();
                frontier.sort_unstable();
                frontier.dedup();
                if frontier.is_empty() {
                    continue;
                }
                let rows = self.edge_level(repo, branch, &frontier, dir, mode, kinds)?;
                for r in &rows {
                    // A node of hop 1 reached by several seeds: every one.
                    for (_, seed) in list.iter().filter(|(id, _)| *id == r.from) {
                        if let Some(c) = candidate(r, *seed, dir, 2, &seed_set) {
                            found.push(c);
                        }
                    }
                }
            }
        }
        // Per seed, in seed order: the best edge per neighbor, nothing shown
        // and nothing an earlier seed took, the order of the brief, the cap.
        let mut taken: HashSet<u64> = HashSet::new();
        let mut kept: Vec<Candidate> = Vec::new();
        for seed in &order {
            let mut best: HashMap<u64, Candidate> = HashMap::new();
            for c in found.iter().filter(|c| c.seed == *seed) {
                if taken.contains(&c.id) || shown(&c.file, c.line) {
                    continue;
                }
                match best.get(&c.id) {
                    Some(b) if !better(c, b) => {}
                    _ => {
                        best.insert(c.id, c.clone());
                    }
                }
            }
            let mut list: Vec<Candidate> = best.into_values().collect();
            list.sort_by(order_within_seed);
            list.truncate(per_seed);
            for c in list {
                taken.insert(c.id);
                kept.push(c);
            }
        }
        // The signatures, for the neighbors kept only: one statement.
        let ids: Vec<u64> = kept.iter().map(|c| c.id).collect();
        let mut symbols = self.symbols_by_ids(repo, branch, &ids)?;
        Ok(kept
            .into_iter()
            .filter_map(|c| {
                symbols.remove(&c.id).map(|symbol| RelatedNeighbor {
                    seed: c.seed,
                    symbol,
                    relation: c.relation,
                    hops: c.hops,
                })
            })
            .collect())
    }

    /// The symbols of `ids` on a branch, by id: one statement.
    fn symbols_by_ids(
        &self,
        repo: &str,
        branch: &str,
        ids: &[u64],
    ) -> Result<HashMap<u64, StoredSymbol>> {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let holes = vec!["?"; ids.len()].join(", ");
        let mut args: Vec<duckdb::types::Value> =
            vec![repo.to_string().into(), branch.to_string().into()];
        args.extend(ids.iter().map(|id| duckdb::types::Value::UBigInt(*id)));
        count_statement();
        let mut stmt = self.conn.prepare(&format!(
            "{SYMBOL_SELECT} WHERE repo = ? AND branch = ? AND id IN ({holes})"
        ))?;
        let rows = stmt.query_map(params_from_iter(args), row_to_symbol)?;
        for r in rows {
            let s = r?;
            out.insert(s.id, s);
        }
        Ok(out)
    }
}

/// The columns of [`SYMBOL_SELECT`], prefixed with `alias`.
fn symbol_cols(alias: &str) -> String {
    SYMBOL_SELECT
        .trim_start_matches("SELECT ")
        .trim_end_matches(" FROM symbols")
        .split(',')
        .map(|c| format!("{alias}.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// [`row_to_symbol`] over the columns from `at` on.
fn row_to_symbol_at(r: &duckdb::Row<'_>, at: usize) -> duckdb::Result<StoredSymbol> {
    Ok(StoredSymbol {
        id: r.get(at)?,
        parent_id: r.get(at + 1)?,
        file: r.get(at + 2)?,
        kind: r.get(at + 3)?,
        name: r.get(at + 4)?,
        qualified: r.get(at + 5)?,
        container: r.get(at + 6)?,
        package: r.get(at + 7)?,
        signature: r.get(at + 8)?,
        start_line: r.get(at + 9)?,
        end_line: r.get(at + 10)?,
        start_byte: r.get::<_, Option<i64>>(at + 11)?.unwrap_or(0),
        end_byte: r.get::<_, Option<i64>>(at + 12)?.unwrap_or(0),
        exported: r.get(at + 13)?,
        rank: r.get(at + 14)?,
        in_degree: r.get(at + 15)?,
        is_test: r.get::<_, Option<bool>>(at + 16)?.unwrap_or(false),
        content_hash: r.get(at + 17)?,
    })
}

/// A level row as a candidate of `seed`, or `None` when a filter leaves it
/// out: no definition (an external, an undecided call), under `medium`, a
/// test, a `file` symbol, a seed (itself, or another one: it is in the
/// brief already).
fn candidate(
    r: &LevelRow,
    seed: u64,
    dir: Direction,
    hops: u8,
    seeds: &HashSet<u64>,
) -> Option<Candidate> {
    let id = r.id?;
    let confidence = confidence_rank(r.confidence.as_deref());
    if r.external
        || r.test
        || confidence < 2
        || seeds.contains(&id)
        || r.node_kind.as_deref() == Some("file")
    {
        return None;
    }
    let relation = match (dir, r.kind.as_str()) {
        (Direction::Upstream, "calls") => Relation::CalledBy,
        (Direction::Upstream, "instantiates") => Relation::InstantiatedBy,
        (Direction::Downstream, "calls") => Relation::Calls,
        (Direction::Downstream, "instantiates") => Relation::Instantiates,
        (Direction::Downstream, "inherits") => Relation::Extends,
        (Direction::Downstream, "implements") => Relation::Implements,
        _ => return None,
    };
    Some(Candidate {
        seed,
        id,
        qualified: r.symbol.clone(),
        file: r.file.clone()?,
        line: r.line?,
        relation,
        hops,
        confidence,
        rank: r.rank,
    })
}

/// Whether `a` is a better edge to its neighbor than `b`: fewer hops, surer,
/// then the relation's order, so the label does not hang on row order.
fn better(a: &Candidate, b: &Candidate) -> bool {
    (a.hops, std::cmp::Reverse(a.confidence), a.relation)
        < (b.hops, std::cmp::Reverse(b.confidence), b.relation)
}

/// The order within a seed: hop, rank (highest first, none last),
/// confidence, name, file, line, id.
fn order_within_seed(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    a.hops
        .cmp(&b.hops)
        .then_with(|| match (a.rank, b.rank) {
            (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(Ordering::Equal),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
        .then_with(|| b.confidence.cmp(&a.confidence))
        .then_with(|| a.qualified.cmp(&b.qualified))
        .then_with(|| a.file.cmp(&b.file))
        .then_with(|| a.line.cmp(&b.line))
        .then_with(|| a.id.cmp(&b.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::LEVEL_STATEMENTS;
    use crate::StoredSymbolEdge;
    use std::collections::BTreeMap;

    /// A symbol of a fixture: qualified name, kind, file, lines.
    #[derive(Clone)]
    struct S {
        q: &'static str,
        kind: &'static str,
        file: &'static str,
        lines: (i32, i32),
        parent: Option<&'static str>,
        rank: Option<f64>,
        test: bool,
    }

    fn s(q: &'static str, kind: &'static str, file: &'static str, lines: (i32, i32)) -> S {
        S {
            q,
            kind,
            file,
            lines,
            parent: None,
            rank: None,
            test: false,
        }
    }

    impl S {
        fn rank(mut self, r: f64) -> S {
            self.rank = Some(r);
            self
        }
        fn parent(mut self, p: &'static str) -> S {
            self.parent = Some(p);
            self
        }
        fn test(mut self) -> S {
            self.test = true;
            self
        }
    }

    /// An edge: source, destination (a symbol of the fixture, or a name
    /// with no definition), kind, confidence, external.
    type Edge = (&'static str, &'static str, &'static str, &'static str, bool);

    fn e(src: &'static str, dst: &'static str, kind: &'static str) -> Edge {
        (src, dst, kind, "high", false)
    }

    /// Writes the fixture on `repo`/`main`; ids from 100 in order.
    fn write(store: &Store, syms: &[S], edges: &[Edge]) -> BTreeMap<&'static str, u64> {
        let ids: BTreeMap<&'static str, u64> = syms
            .iter()
            .enumerate()
            .map(|(i, x)| (x.q, 100 + i as u64))
            .collect();
        let mut by_file: BTreeMap<&str, Vec<StoredSymbol>> = BTreeMap::new();
        for x in syms {
            let name = x.q.rsplit('.').next().unwrap();
            by_file.entry(x.file).or_default().push(StoredSymbol {
                id: ids[x.q],
                parent_id: x.parent.map(|p| ids[p]),
                file: x.file.into(),
                kind: x.kind.into(),
                name: name.into(),
                qualified: x.q.into(),
                signature: Some(format!("{} {name}(int a)", x.kind)),
                start_line: x.lines.0,
                end_line: x.lines.1,
                rank: x.rank,
                is_test: x.test,
                ..Default::default()
            });
        }
        for (file, rows) in &by_file {
            store
                .replace_file_symbols("repo", "main", file, rows)
                .unwrap();
        }
        let file_of = |q: &str| syms.iter().find(|x| x.q == q).unwrap().file;
        let mut edges_by_file: BTreeMap<&str, Vec<StoredSymbolEdge>> = BTreeMap::new();
        for (i, (src, dst, kind, conf, external)) in edges.iter().enumerate() {
            let file = file_of(src);
            edges_by_file
                .entry(file)
                .or_default()
                .push(StoredSymbolEdge {
                    kind: (*kind).into(),
                    src_id: ids[src],
                    dst_id: ids.get(dst).copied(),
                    dst_name: (*dst).into(),
                    file: file.into(),
                    line: 1000 + i as i32,
                    confidence: Some((*conf).into()),
                    resolution: Some("local".into()),
                    external: Some(*external),
                    from_test: syms.iter().any(|x| x.q == *src && x.test),
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

    fn seeds(store: &Store, ids: &BTreeMap<&str, u64>, names: &[&str]) -> Vec<StoredSymbol> {
        names
            .iter()
            .map(|n| store.symbol_by_id("repo", "main", ids[n]).unwrap().unwrap())
            .collect()
    }

    fn nothing_shown(_: &str, _: i32) -> bool {
        false
    }

    fn shown_as(v: &[RelatedNeighbor]) -> Vec<(String, &'static str, u8)> {
        v.iter()
            .map(|n| (n.symbol.qualified.clone(), n.relation.label(), n.hops))
            .collect()
    }

    fn own(v: &[(&str, &'static str, u8)]) -> Vec<(String, &'static str, u8)> {
        v.iter().map(|(a, b, c)| (a.to_string(), *b, *c)).collect()
    }

    /// A chunk maps to the innermost symbol holding it; a class chunk to the
    /// class; a doc chunk to the symbol it documents; a chunk of no symbol
    /// (and never the file symbol) to nothing. One statement.
    #[test]
    fn chunks_map_to_their_innermost_symbol() {
        let store = Store::open_in_memory(3).unwrap();
        write(
            &store,
            &[
                s("a.rs", "file", "a.rs", (1, 40)),
                s("Svc", "class", "a.rs", (3, 30)),
                s("Svc.run", "method", "a.rs", (8, 12)).parent("Svc"),
                s("Svc.stop", "method", "a.rs", (14, 20)).parent("Svc"),
            ],
            &[],
        );
        let chunk = |lo, hi, doc| ChunkRef {
            file: "a.rs",
            start_line: lo,
            end_line: hi,
            doc,
        };
        LEVEL_STATEMENTS.with(|c| c.set(0));
        let got = store
            .chunk_symbols(
                "repo",
                "main",
                &[
                    chunk(8, 12, false),
                    chunk(3, 30, false),
                    chunk(5, 8, true),
                    chunk(34, 38, false),
                    chunk(15, 16, false),
                ],
            )
            .unwrap();
        let names: Vec<Option<&str>> = got
            .iter()
            .map(|s| s.as_ref().map(|s| s.qualified.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                Some("Svc.run"),
                Some("Svc"),
                Some("Svc.run"),
                None,
                Some("Svc.stop")
            ]
        );
        assert_eq!(LEVEL_STATEMENTS.with(|c| c.get()), 1, "one statement");
        // Another branch, another file: nothing.
        let other = store
            .chunk_symbols("repo", "dev", &[chunk(8, 12, false)])
            .unwrap();
        assert_eq!(other, vec![None]);
    }

    /// Calls and instantiations both ways, supertypes up, each named from
    /// the seed.
    #[test]
    fn neighbors_follow_calls_both_ways_and_supertypes_up() {
        let store = Store::open_in_memory(3).unwrap();
        let ids = write(
            &store,
            &[
                s("Svc", "class", "svc.rs", (1, 50)),
                s("Svc.run", "method", "svc.rs", (5, 10)).parent("Svc"),
                s("Base", "class", "base.rs", (1, 9)),
                s("IRun", "interface", "irun.rs", (1, 5)),
                s("Main.main", "function", "main.rs", (1, 9)),
                s("Factory.make", "function", "factory.rs", (1, 9)),
                s("Dep.x", "function", "dep.rs", (1, 9)),
                s("Thing", "class", "thing.rs", (1, 9)),
            ],
            &[
                e("Main.main", "Svc.run", "calls"),
                e("Svc.run", "Dep.x", "calls"),
                e("Svc.run", "Thing", "instantiates"),
                e("Factory.make", "Svc", "instantiates"),
                e("Svc", "Base", "inherits"),
                e("Svc", "IRun", "implements"),
            ],
        );
        let got = store
            .related_neighbors(
                "repo",
                "main",
                &seeds(&store, &ids, &["Svc.run", "Svc"]),
                1,
                RELATED_PER_SEED,
                &nothing_shown,
            )
            .unwrap();
        let mut run: Vec<_> = shown_as(&got[..3.min(got.len())]);
        run.sort();
        assert_eq!(
            run,
            own(&[
                ("Dep.x", "calls", 1),
                ("Main.main", "called by", 1),
                ("Thing", "instantiates", 1),
            ]),
            "{got:?}"
        );
        let mut svc: Vec<_> = shown_as(&got[3.min(got.len())..]);
        svc.sort();
        assert_eq!(
            svc,
            own(&[
                ("Base", "extends", 1),
                ("Factory.make", "instantiated by", 1),
                ("IRun", "implements", 1),
            ]),
            "{got:?}"
        );
        assert!(got[..3].iter().all(|n| n.seed == ids["Svc.run"]));
        // The signature travels with the neighbor.
        assert_eq!(
            got[0].symbol.signature.as_deref().map(|s| s.contains('(')),
            Some(true)
        );
    }

    /// Only `high`/`medium`, no tests, no externals, no undecided calls, no
    /// file symbols, never the seed itself (recursion).
    #[test]
    fn neighbors_leave_out_what_the_filters_say() {
        let store = Store::open_in_memory(3).unwrap();
        let ids = write(
            &store,
            &[
                s("Svc.run", "method", "svc.rs", (5, 10)),
                s("Low.a", "function", "low.rs", (1, 9)),
                s("Med.b", "function", "med.rs", (1, 9)),
                s("RunTest.t", "function", "tests/run.rs", (1, 9)).test(),
                s("script.py", "file", "script.py", (1, 9)),
            ],
            &[
                ("Low.a", "Svc.run", "calls", "low", false),
                ("Med.b", "Svc.run", "calls", "medium", false),
                e("RunTest.t", "Svc.run", "calls"),
                e("script.py", "Svc.run", "calls"),
                ("Svc.run", "serde_json.from_str", "calls", "high", true),
                ("Svc.run", "unknown", "calls", "low", false),
                e("Svc.run", "Svc.run", "calls"),
            ],
        );
        let got = store
            .related_neighbors(
                "repo",
                "main",
                &seeds(&store, &ids, &["Svc.run"]),
                1,
                RELATED_PER_SEED,
                &nothing_shown,
            )
            .unwrap();
        assert_eq!(shown_as(&got), own(&[("Med.b", "called by", 1)]));
    }

    /// Ranked (highest first, a missing rank last) and capped per seed.
    #[test]
    fn neighbors_are_ranked_and_capped_per_seed() {
        let store = Store::open_in_memory(3).unwrap();
        let ids = write(
            &store,
            &[
                s("Svc.run", "method", "svc.rs", (5, 10)),
                s("C1.a", "function", "c1.rs", (1, 9)).rank(0.1),
                s("C2.a", "function", "c2.rs", (1, 9)).rank(0.5),
                s("C3.a", "function", "c3.rs", (1, 9)),
                s("C4.a", "function", "c4.rs", (1, 9)).rank(0.3),
                s("C5.a", "function", "c5.rs", (1, 9)).rank(0.9),
                s("C6.a", "function", "c6.rs", (1, 9)).rank(0.2),
            ],
            &[
                e("C1.a", "Svc.run", "calls"),
                e("C2.a", "Svc.run", "calls"),
                e("C3.a", "Svc.run", "calls"),
                e("C4.a", "Svc.run", "calls"),
                e("C5.a", "Svc.run", "calls"),
                e("C6.a", "Svc.run", "calls"),
            ],
        );
        let got = store
            .related_neighbors(
                "repo",
                "main",
                &seeds(&store, &ids, &["Svc.run"]),
                1,
                RELATED_PER_SEED,
                &nothing_shown,
            )
            .unwrap();
        let names: Vec<&str> = got.iter().map(|n| n.symbol.qualified.as_str()).collect();
        assert_eq!(names, vec!["C5.a", "C2.a", "C4.a", "C6.a"]);
    }

    /// Equal ranks: the order is the names', whatever the order the rows
    /// were written in.
    #[test]
    fn ties_are_broken_the_same_way_whatever_the_write_order() {
        let syms = vec![
            s("Svc.run", "method", "svc.rs", (5, 10)),
            s("Zed.a", "function", "a.rs", (1, 9)).rank(0.5),
            s("Alpha.a", "function", "z.rs", (1, 9)).rank(0.5),
            s("Mid.a", "function", "m.rs", (1, 9)).rank(0.5),
        ];
        let edges = vec![
            e("Zed.a", "Svc.run", "calls"),
            e("Alpha.a", "Svc.run", "calls"),
            e("Mid.a", "Svc.run", "calls"),
        ];
        let run = |syms: &[S], edges: &[Edge]| {
            let store = Store::open_in_memory(3).unwrap();
            let ids = write(&store, syms, edges);
            let got = store
                .related_neighbors(
                    "repo",
                    "main",
                    &seeds(&store, &ids, &["Svc.run"]),
                    1,
                    2,
                    &nothing_shown,
                )
                .unwrap();
            got.iter()
                .map(|n| n.symbol.qualified.clone())
                .collect::<Vec<_>>()
        };
        let forward = run(&syms, &edges);
        let mut rsyms = syms.clone();
        rsyms[1..].reverse();
        let mut redges = edges.clone();
        redges.reverse();
        assert_eq!(forward, vec!["Alpha.a", "Mid.a"]);
        assert_eq!(run(&rsyms, &redges), forward);
    }

    /// What the brief already shows, and the other seeds, are not repeated;
    /// a neighbor of two seeds goes with the first, and the cap is applied
    /// after that (an excluded neighbor does not take a place).
    #[test]
    fn what_is_shown_is_not_repeated() {
        let store = Store::open_in_memory(3).unwrap();
        let ids = write(
            &store,
            &[
                s("A.run", "method", "a.rs", (5, 10)),
                s("B.run", "method", "b.rs", (5, 10)),
                s("Shared.x", "function", "shared.rs", (1, 9)).rank(0.9),
                s("InCode.y", "function", "incode.rs", (20, 29)).rank(0.8),
                s("Other.z", "function", "other.rs", (1, 9)).rank(0.1),
                s("BOnly.w", "function", "bonly.rs", (1, 9)).rank(0.5),
                s("BLow.v", "function", "blow.rs", (1, 9)).rank(0.05),
            ],
            &[
                e("A.run", "Shared.x", "calls"),
                e("B.run", "Shared.x", "calls"),
                e("B.run", "BOnly.w", "calls"),
                e("B.run", "BLow.v", "calls"),
                e("A.run", "InCode.y", "calls"),
                e("A.run", "B.run", "calls"),
                e("A.run", "Other.z", "calls"),
            ],
        );
        let shown = |file: &str, line: i32| file == "incode.rs" && (18..=30).contains(&line);
        let got = store
            .related_neighbors(
                "repo",
                "main",
                &seeds(&store, &ids, &["A.run", "B.run"]),
                1,
                2,
                &shown,
            )
            .unwrap();
        let pairs: Vec<(u64, &str)> = got
            .iter()
            .map(|n| (n.seed, n.symbol.qualified.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (ids["A.run"], "Shared.x"),
                (ids["A.run"], "Other.z"),
                (ids["B.run"], "BOnly.w"),
                (ids["B.run"], "BLow.v"),
            ],
            "{got:?}"
        );
    }

    /// One batch is a fixed number of statements, whatever the number of
    /// seeds and of neighbors: one seed or five, fifty callers each.
    #[test]
    fn a_batch_is_a_fixed_number_of_statements() {
        let store = Store::open_in_memory(3).unwrap();
        let mut syms: Vec<S> = Vec::new();
        let names: Vec<&'static str> = (0..5)
            .map(|i| &*Box::leak(format!("Seed{i}.run").into_boxed_str()))
            .collect();
        for n in &names {
            syms.push(s(n, "method", "seed.rs", (5, 10)));
        }
        let mut edges: Vec<Edge> = Vec::new();
        for (k, n) in names.iter().enumerate() {
            for j in 0..50 {
                let c: &'static str = Box::leak(format!("C{k}_{j}.a").into_boxed_str());
                syms.push(s(c, "function", "callers.rs", (1, 9)));
                edges.push(e(c, n, "calls"));
            }
        }
        let ids = write(&store, &syms, &edges);
        let count = |seed_names: &[&str], hops: u8| {
            LEVEL_STATEMENTS.with(|c| c.set(0));
            let got = store
                .related_neighbors(
                    "repo",
                    "main",
                    &seeds(&store, &ids, seed_names),
                    hops,
                    RELATED_PER_SEED,
                    &nothing_shown,
                )
                .unwrap();
            assert_eq!(got.len(), RELATED_PER_SEED * seed_names.len());
            LEVEL_STATEMENTS.with(|c| c.get())
        };
        let one = count(&names[..1], 1);
        let five = count(&names, 1);
        assert_eq!(one, five, "per batch, not per seed");
        assert!(five <= 5, "{five} statements");
        let two_hops = count(&names, 2);
        assert!(two_hops <= five + 2, "{two_hops} statements");
    }

    /// Two hops reach callers of callers and callees of callees, after the
    /// direct neighbors; one hop does not.
    #[test]
    fn two_hops_reach_the_neighbors_of_neighbors() {
        let store = Store::open_in_memory(3).unwrap();
        let ids = write(
            &store,
            &[
                s("Svc.run", "method", "svc.rs", (5, 10)),
                s("Mid.b", "function", "mid.rs", (1, 9)),
                s("Top.a", "function", "top.rs", (1, 9)).rank(0.9),
                s("Dep.x", "function", "dep.rs", (1, 9)),
                s("Leaf.y", "function", "leaf.rs", (1, 9)).rank(0.9),
            ],
            &[
                e("Top.a", "Mid.b", "calls"),
                e("Mid.b", "Svc.run", "calls"),
                e("Svc.run", "Dep.x", "calls"),
                e("Dep.x", "Leaf.y", "calls"),
            ],
        );
        let walk = |hops| {
            let got = store
                .related_neighbors(
                    "repo",
                    "main",
                    &seeds(&store, &ids, &["Svc.run"]),
                    hops,
                    RELATED_PER_SEED,
                    &nothing_shown,
                )
                .unwrap();
            shown_as(&got)
        };
        let mut one = walk(1);
        one.sort();
        assert_eq!(
            one,
            own(&[("Dep.x", "calls", 1), ("Mid.b", "called by", 1)])
        );
        let two = walk(2);
        assert_eq!(two.len(), 4, "{two:?}");
        assert!(
            two[..2].iter().all(|n| n.2 == 1),
            "direct ones first: {two:?}"
        );
        let mut far: Vec<_> = two[2..].to_vec();
        far.sort();
        assert_eq!(
            far,
            own(&[("Leaf.y", "calls", 2), ("Top.a", "called by", 2)])
        );
    }

    /// A method that implements an interface method shows it: the
    /// override-equivalent in the supertype (TASK-017's index), `medium` at
    /// most and only with a `high`/`medium` chain.
    #[test]
    fn a_method_shows_the_supertype_method_it_overrides() {
        let store = Store::open_in_memory(3).unwrap();
        let ids = write(
            &store,
            &[
                s("Impl", "class", "impl.java", (1, 40)),
                s("Impl.save", "method", "impl.java", (5, 10)).parent("Impl"),
                s("IRepo", "interface", "irepo.java", (1, 9)),
                s("IRepo.save", "method", "irepo.java", (3, 3)).parent("IRepo"),
                s("Weak", "class", "weak.java", (1, 40)),
                s("Weak.save", "method", "weak.java", (5, 10)).parent("Weak"),
                s("IOther", "interface", "iother.java", (1, 9)),
                s("IOther.save", "method", "iother.java", (3, 3)).parent("IOther"),
            ],
            &[
                e("Impl", "IRepo", "implements"),
                ("Weak", "IOther", "implements", "low", false),
            ],
        );
        let got = store
            .related_neighbors(
                "repo",
                "main",
                &seeds(&store, &ids, &["Impl.save", "Weak.save"]),
                1,
                RELATED_PER_SEED,
                &nothing_shown,
            )
            .unwrap();
        assert_eq!(shown_as(&got), own(&[("IRepo.save", "overrides", 1)]));
    }
}
