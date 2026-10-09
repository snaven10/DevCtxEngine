//! Dispatch through a supertype in `impact_analysis` (PLAN-009 TASK-017): a
//! method is also reached through the methods it overrides, or that override
//! it, along the `inherits`/`implements` edges the link pass resolved.
//!
//! - **Upstream** (callers): `Impl.m` → the override-equivalent methods of its
//!   supertypes (`IService.m`, `Base.m`), whose callers may run `Impl.m`.
//! - **Downstream** (callees): an interface or abstract method → the methods
//!   of its subtypes that implement it.
//!
//! Override-equivalent = the same name, a callable kind, and an arity that can
//! match (the parameter lists of both signatures, when they can be read; a
//! default, an optional or a variadic parameter widens the range). The chain
//! of supertypes is transitive up to [`DISPATCH_DEPTH`] levels. A private or
//! static method overrides nothing. Go is left out: embedding promotes
//! methods, it does not override them, and an interface is satisfied without
//! an edge the index could follow.
//!
//! An edge reached through dispatch is never surer than `medium` and never
//! surer than the edge it stands for: `min(medium, original)`, with the
//! weakest `inherits`/`implements` edge of the chain counted too.

use std::collections::HashMap;

use duckdb::params_from_iter;

use crate::error::Result;
use crate::impact::{count_statement, Direction, FrontierSql};
use crate::store::Store;

/// Supertype levels a dispatch climbs (upstream) or descends (downstream):
/// `Impl` → `Base` → `IService` → its own super-interface.
pub const DISPATCH_DEPTH: usize = 4;

/// Override-equivalent methods one node of a level is expanded into, at
/// most; the rest are counted (`omitted_by_limit.dispatch`), not followed.
pub const DISPATCH_MAX_PER_NODE: usize = 16;

/// The relations a dispatch follows between types.
const SUPER_KINDS: &str = "('inherits', 'implements')";

/// The kinds a method that can be overridden has.
const CALLABLE_KINDS: &str = "('method', 'function')";

/// One override-equivalent method of a frontier node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Equivalent {
    /// The frontier node it stands for.
    pub origin: u64,
    /// Qualified name of the frontier node.
    pub origin_symbol: String,
    /// The equivalent method.
    pub id: u64,
    /// Its type.
    pub parent: u64,
    /// Its qualified name.
    pub symbol: String,
    /// File of its definition.
    pub file: Option<String>,
    /// First line of its definition.
    pub line: Option<i32>,
    /// Its rank (PageRank).
    pub rank: Option<f64>,
    /// A symbol of a test file.
    pub test: bool,
    /// The weakest `inherits`/`implements` edge of the chain: 3 high, 2
    /// medium, 1 anything else.
    pub chain: u8,
}

/// The override-equivalents of a frontier, capped per node.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Equivalents {
    /// The ones followed, by origin and then by rank and name.
    pub kept: Vec<Equivalent>,
    /// Ids of the ones past [`DISPATCH_MAX_PER_NODE`] for some origin.
    pub cut: Vec<u64>,
}

/// `min(medium, c)`, and no surer than the chain: a dispatch is never
/// `high`, and a `low` edge stays `low`. An undecided edge stays undecided.
pub(crate) fn dispatch_confidence(c: Option<&str>, chain: u8) -> Option<String> {
    let rank = match c? {
        "high" | "medium" => 2,
        _ => 1,
    }
    .min(chain.max(1));
    Some(if rank >= 2 { "medium" } else { "low" }.to_string())
}

/// The range of arities a signature accepts: `(min, max)`, `max` `None` for
/// a variadic list. `None` when no parameter list follows `name`.
pub(crate) fn arity(signature: &str, name: &str) -> Option<(usize, Option<usize>)> {
    let params = parameter_list(signature, name)?;
    let parts = split_top_level(params);
    let (mut min, mut variadic) = (0, false);
    let mut count = 0;
    for p in &parts {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        count += 1;
        let head = p.trim_start_matches(|c: char| c == '@' || c.is_whitespace());
        if p.contains("...") || head.starts_with('*') {
            variadic = true;
            count -= 1;
            continue;
        }
        // `x = 1` (Python, TS), `x?: T` (TS): optional.
        let optional = top_level_contains(p, '=') || p.contains("?:") || p.ends_with('?');
        if !optional {
            min += 1;
        }
    }
    Some((min, (!variadic).then_some(count)))
}

/// Whether two arity ranges can meet; an unreadable one meets anything.
fn arities_meet(a: Option<(usize, Option<usize>)>, b: Option<(usize, Option<usize>)>) -> bool {
    let (Some((amin, amax)), Some((bmin, bmax))) = (a, b) else {
        return true;
    };
    amin <= bmax.unwrap_or(usize::MAX) && bmin <= amax.unwrap_or(usize::MAX)
}

/// A method that overrides nothing and nothing overrides: private or static.
/// Only the modifiers before the name count, and a lifetime is none: a Rust
/// `-> &'static str` or `T: 'static` is no `static` method (review MAJOR 1).
fn never_overrides(signature: &str, name: &str) -> bool {
    let head = name_in(signature, name).map_or(signature, |(start, _)| &signature[..start]);
    let bytes = head.as_bytes();
    let mut start = 0;
    let mut words = Vec::new();
    for (i, &b) in bytes
        .iter()
        .enumerate()
        .chain(std::iter::once((bytes.len(), &b' ')))
    {
        if !(b.is_ascii_alphanumeric() || b == b'_') {
            if i > start && (start == 0 || bytes[start - 1] != b'\'') {
                words.push(&head[start..i]);
            }
            start = i + 1;
        }
    }
    words.iter().any(|w| *w == "private" || *w == "static")
}

/// The text between the parentheses that follow `name` in `signature` (as a
/// whole word, past generic parameters): `fn m<T>(a: T, b: u8)` → `a: T, b: u8`.
fn parameter_list<'a>(signature: &'a str, name: &str) -> Option<&'a str> {
    name_in(signature, name).map(|(_, params)| params)
}

/// Where `name` starts in `signature` as the name of what the parameter list
/// after it belongs to, and that list's text.
fn name_in<'a>(signature: &'a str, name: &str) -> Option<(usize, &'a str)> {
    let bytes = signature.as_bytes();
    let mut from = 0;
    while let Some(at) = signature[from..].find(name) {
        let start = from + at;
        let end = start + name.len();
        from = end;
        let word_before = start > 0 && is_word(bytes[start - 1]);
        let word_after = end < bytes.len() && is_word(bytes[end]);
        if word_before || word_after {
            continue;
        }
        let mut i = end;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'<' {
            i = close_of(bytes, i, b'<', b'>')? + 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
        }
        if i < bytes.len() && bytes[i] == b'(' {
            let close = close_of(bytes, i, b'(', b')')?;
            return Some((start, &signature[i + 1..close]));
        }
    }
    None
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The index of the bracket closing the one at `open`; a `>` of `->`/`=>`
/// closes nothing.
fn close_of(bytes: &[u8], open: usize, o: u8, c: u8) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        if b == b'>' && i > 0 && matches!(bytes[i - 1], b'-' | b'=') {
            continue;
        }
        if b == o {
            depth += 1;
        } else if b == c {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// `params` split at the commas outside any bracket or string.
fn split_top_level(params: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    let mut quote: Option<u8> = None;
    let bytes = params.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = quote {
            if b == q && (i == 0 || bytes[i - 1] != b'\\') {
                quote = None;
            }
            continue;
        }
        match b {
            b'"' | b'\'' | b'`' => quote = Some(b),
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b'>' if i > 0 && matches!(bytes[i - 1], b'-' | b'=') => {}
            b')' | b']' | b'}' | b'>' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&params[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&params[start..]);
    out
}

/// Whether `ch` appears in `p` outside any bracket (a default value, not a
/// generic default or a lambda inside a type).
fn top_level_contains(p: &str, ch: char) -> bool {
    let mut depth = 0i32;
    let bytes = p.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'>' if i > 0 && bytes[i - 1] == b'=' => {}
            b'>' => depth -= 1,
            _ if depth == 0 && b as char == ch => {
                // `=>`, `==`, `>=`, `<=`, `!=` are no default.
                let next = bytes.get(i + 1).copied();
                let prev = i.checked_sub(1).map(|j| bytes[j]);
                if next != Some(b'>')
                    && next != Some(b'=')
                    && !matches!(prev, Some(b'=' | b'!' | b'<' | b'>'))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The supertype graph of a branch, read once per walk as id pairs (one
/// statement): every resolved `inherits`/`implements` edge of `live_edges`
/// outside Go. The methods are read per level, only those with the name of a
/// frontier method in a type of this graph (review of TASK-017, MAJOR 2).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DispatchIndex {
    /// Type → its supertypes, with the edge's confidence (3 high, 2
    /// medium, 1 anything else).
    supers: HashMap<u64, Vec<(u64, u8)>>,
    /// Type → its subtypes.
    subs: HashMap<u64, Vec<(u64, u8)>>,
    /// Every type at either end, sorted.
    types: Vec<u64>,
}

impl DispatchIndex {
    /// Whether `t` has a supertype or a subtype here.
    pub(crate) fn has_type(&self, t: u64) -> bool {
        self.types.binary_search(&t).is_ok()
    }

    /// The types reachable from `from` in direction `dir` within
    /// [`DISPATCH_DEPTH`] levels, each with its surest chain.
    fn reach(&self, from: u64, dir: Direction) -> HashMap<u64, u8> {
        let graph = match dir {
            Direction::Upstream => &self.supers,
            Direction::Downstream => &self.subs,
        };
        let mut best: HashMap<u64, u8> = HashMap::new();
        let mut layer: Vec<(u64, u8)> = vec![(from, 3)];
        for _ in 0..DISPATCH_DEPTH {
            let mut next = Vec::new();
            for (t, conf) in layer {
                for &(s, c) in graph.get(&t).into_iter().flatten() {
                    let c = conf.min(c);
                    if s != from && best.get(&s).is_none_or(|&b| c > b) {
                        best.insert(s, c);
                        next.push((s, c));
                    }
                }
            }
            layer = next;
        }
        best
    }
}

/// A frontier method, as the dispatch query reads it.
struct Origin {
    parent: u64,
    name: String,
    symbol: String,
    signature: String,
}

/// An origin, the types it reaches (with their chain) and its candidates there.
type OriginFound = (Origin, HashMap<u64, u8>, Vec<(Candidate, u8)>);

/// A method of the same name in a type of the supertype graph.
struct Candidate {
    id: u64,
    parent: u64,
    symbol: String,
    file: Option<String>,
    line: Option<i32>,
    signature: String,
    rank: Option<f64>,
    test: bool,
}

#[cfg(test)]
thread_local! {
    /// Times the supertype graph was read on this thread (once per walk).
    pub(crate) static INDEX_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 3 high, 2 medium, 1 anything else.
fn confidence_rank(c: Option<&str>) -> u8 {
    match c {
        Some("high") => 3,
        Some("medium") => 2,
        _ => 1,
    }
}

/// The equivalents of one origin among its candidates (already reachable,
/// with their chain): override-equivalent ones, ordered and capped.
fn select(origin: u64, o: &Origin, found: Vec<(Candidate, u8)>, out: &mut Equivalents) {
    if never_overrides(&o.signature, &o.name) {
        return;
    }
    let mine = arity(&o.signature, &o.name);
    let mut list: Vec<Equivalent> = found
        .into_iter()
        .filter(|(c, _)| {
            !never_overrides(&c.signature, &o.name)
                && arities_meet(mine, arity(&c.signature, &o.name))
        })
        .map(|(c, chain)| Equivalent {
            origin,
            origin_symbol: o.symbol.clone(),
            id: c.id,
            parent: c.parent,
            symbol: c.symbol,
            file: c.file,
            line: c.line,
            rank: c.rank,
            test: c.test,
            chain,
        })
        .collect();
    list.sort_by(|a, b| {
        b.rank
            .unwrap_or(f64::NEG_INFINITY)
            .partial_cmp(&a.rank.unwrap_or(f64::NEG_INFINITY))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.symbol.cmp(&b.symbol))
            .then_with(|| a.id.cmp(&b.id))
    });
    if list.len() > DISPATCH_MAX_PER_NODE {
        out.cut
            .extend(list.drain(DISPATCH_MAX_PER_NODE..).map(|e| e.id));
    }
    out.kept.extend(list);
}

impl Store {
    /// The supertype graph of `branch` ([`DispatchIndex`]) as id pairs, one
    /// statement; `None` when it has no resolved supertype edge (then no
    /// level pays for dispatch). Go's files are left out (embedding promotes,
    /// it does not override).
    pub(crate) fn dispatch_index(&self, repo: &str, branch: &str) -> Result<Option<DispatchIndex>> {
        #[cfg(test)]
        INDEX_READS.with(|c| c.set(c.get() + 1));
        let mut stmt = self.conn.prepare(&format!(
            "SELECT e.src_id, e.dst_id, e.confidence FROM live_edges e
              WHERE e.repo = ? AND e.branch = ? AND e.kind IN {SUPER_KINDS}
                AND e.dst_id IS NOT NULL AND NOT ends_with(e.file, '.go')"
        ))?;
        let edges = stmt
            .query_map(duckdb::params![repo, branch], |r| {
                Ok((
                    r.get::<_, u64>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if edges.is_empty() {
            return Ok(None);
        }
        let mut idx = DispatchIndex::default();
        for (src, dst, conf) in edges {
            let c = confidence_rank(conf.as_deref());
            idx.supers.entry(src).or_default().push((dst, c));
            idx.subs.entry(dst).or_default().push((src, c));
            idx.types.extend([src, dst]);
        }
        idx.types.sort_unstable();
        idx.types.dedup();
        Ok(Some(idx))
    }

    /// The override-equivalent methods of the methods of `frontier` (one
    /// statement per level): the frontier methods whose type is in the
    /// supertype graph, and the methods with their name in another type of
    /// it; then, in memory, the ones whose type is reachable (supertypes
    /// upstream, subtypes downstream, up to [`DISPATCH_DEPTH`] levels),
    /// override-equivalent, at most [`DISPATCH_MAX_PER_NODE`] per node.
    pub(crate) fn override_equivalents(
        &self,
        repo: &str,
        branch: &str,
        frontier: &[u64],
        dir: Direction,
        mode: FrontierSql,
        index: &DispatchIndex,
    ) -> Result<Equivalents> {
        let mut out = Equivalents::default();
        if frontier.is_empty() {
            return Ok(out);
        }
        let types = format!(
            "[{}]",
            index
                .types
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut args: Vec<duckdb::types::Value> = vec![
            types.into(),
            repo.to_string().into(),
            branch.to_string().into(),
        ];
        let set = self.frontier_set(frontier, mode, &mut args)?;
        args.extend([repo.to_string().into(), branch.to_string().into()]);
        let sql = format!(
            "WITH t AS (SELECT unnest(CAST(? AS UBIGINT[])) AS id),
             f AS (
               SELECT s.id, s.parent_id, s.name, s.qualified, s.signature FROM symbols s
                WHERE s.repo = ? AND s.branch = ? AND s.id IN {set}
                  AND s.kind IN {CALLABLE_KINDS} AND NOT ends_with(s.file, '.go')
                  AND s.parent_id IN (SELECT id FROM t)
             )
             SELECT f.id, f.parent_id, f.name, f.qualified, f.signature,
                    m.id, m.parent_id, m.qualified, m.file, m.start_line, m.signature,
                    m.rank, coalesce(m.is_test, false)
               FROM f
               JOIN symbols m ON m.repo = ? AND m.branch = ? AND m.name = f.name
                             AND m.id <> f.id AND m.kind IN {CALLABLE_KINDS}
                             AND NOT ends_with(m.file, '.go')
                             AND m.parent_id IN (SELECT id FROM t)"
        );
        count_statement();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok((
                r.get::<_, u64>(0)?,
                Origin {
                    parent: r.get(1)?,
                    name: r.get(2)?,
                    symbol: r.get(3)?,
                    signature: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                },
                Candidate {
                    id: r.get(5)?,
                    parent: r.get(6)?,
                    symbol: r.get(7)?,
                    file: r.get(8)?,
                    line: r.get(9)?,
                    signature: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                    rank: r.get(11)?,
                    test: r.get(12)?,
                },
            ))
        })?;
        let mut origins: HashMap<u64, OriginFound> = HashMap::new();
        for row in rows {
            let (id, o, c) = row?;
            let entry = origins.entry(id).or_insert_with(|| {
                let reach = index.reach(o.parent, dir);
                (o, reach, Vec::new())
            });
            if let Some(&chain) = entry.1.get(&c.parent) {
                entry.2.push((c, chain));
            }
        }
        let mut ids: Vec<u64> = origins.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let (o, _, found) = origins.remove(&id).expect("listed");
            select(id, &o, found, &mut out);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arity_reads_each_language_signature() {
        assert_eq!(
            arity("@Override public void update(String id)", "update"),
            Some((1, Some(1)))
        );
        assert_eq!(
            arity("void update(String id);", "update"),
            Some((1, Some(1)))
        );
        assert_eq!(
            arity(
                "@Path(\"x\") public R get(@PathParam(\"id\") Long id, Map<String, List<X>> m)",
                "get"
            ),
            Some((2, Some(2)))
        );
        assert_eq!(
            arity("public void log(String fmt, Object... args)", "log"),
            Some((1, None))
        );
        assert_eq!(
            arity("update(id: string, opts?: Opts): void", "update"),
            Some((1, Some(2)))
        );
        assert_eq!(
            arity("handle(cb: (x: number) => void, n = 2)", "handle"),
            Some((1, Some(2)))
        );
        assert_eq!(
            arity("fn update(&self, id: &str);", "update"),
            Some((2, Some(2)))
        );
        assert_eq!(
            arity("pub fn map<T, F: Fn(T) -> T>(&self, f: F) -> T", "map"),
            Some((2, Some(2)))
        );
        assert_eq!(
            arity("def run(self, x, y=None, *args, **kw):", "run"),
            Some((2, None))
        );
        assert_eq!(arity("handle(): void", "handle"), Some((0, Some(0))));
        assert_eq!(arity("handle = (e: Event) =>", "handle"), None);
    }

    #[test]
    fn arities_meet_when_their_ranges_overlap() {
        assert!(arities_meet(Some((1, Some(1))), Some((1, Some(2)))));
        assert!(!arities_meet(Some((1, Some(1))), Some((2, Some(2)))));
        assert!(arities_meet(Some((1, None)), Some((3, Some(3)))));
        assert!(arities_meet(None, Some((3, Some(3)))));
    }

    #[test]
    fn dispatch_is_never_surer_than_medium_nor_than_its_edge() {
        assert_eq!(
            dispatch_confidence(Some("high"), 3).as_deref(),
            Some("medium")
        );
        assert_eq!(
            dispatch_confidence(Some("medium"), 3).as_deref(),
            Some("medium")
        );
        assert_eq!(dispatch_confidence(Some("low"), 3).as_deref(), Some("low"));
        assert_eq!(dispatch_confidence(Some("high"), 1).as_deref(), Some("low"));
        assert_eq!(dispatch_confidence(None, 3), None);
    }

    #[test]
    fn private_and_static_methods_override_nothing() {
        assert!(never_overrides("private void update(String id)", "update"));
        assert!(never_overrides("public static Foo of(String s)", "of"));
        assert!(!never_overrides(
            "public void update(String staticId)",
            "update"
        ));
        assert!(!never_overrides("fn name(&self) -> &'static str", "name"));
        assert!(!never_overrides(
            "fn spawn<T: 'static>(&self, t: T)",
            "spawn"
        ));
        assert!(!never_overrides(
            "public void run(@Named(\"static\") String s)",
            "run"
        ));
    }
}
