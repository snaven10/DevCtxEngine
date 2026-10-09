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

use crate::error::Result;
use crate::impact::Direction;
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
fn never_overrides(signature: &str) -> bool {
    signature
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|w| w == "private" || w == "static")
}

/// The text between the parentheses that follow `name` in `signature` (as a
/// whole word, past generic parameters): `fn m<T>(a: T, b: u8)` → `a: T, b: u8`.
fn parameter_list<'a>(signature: &'a str, name: &str) -> Option<&'a str> {
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
            return Some(&signature[i + 1..close]);
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

/// A method of a type that has a supertype or a subtype on the branch: what
/// a dispatch can start from or arrive at.
#[derive(Debug, Clone, PartialEq)]
struct Method {
    parent: u64,
    name: String,
    symbol: String,
    file: Option<String>,
    line: Option<i32>,
    signature: String,
    rank: Option<f64>,
    test: bool,
}

/// The supertype graph of a branch, read once per walk (two statements,
/// whatever the depth and the frontier): every resolved `inherits`/
/// `implements` edge of `live_edges`, and the methods of the types at
/// either end. Each level is then expanded in memory.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DispatchIndex {
    /// Type → its supertypes, with the edge's confidence (3 high, 2
    /// medium, 1 anything else).
    supers: HashMap<u64, Vec<(u64, u8)>>,
    /// Type → its subtypes.
    subs: HashMap<u64, Vec<(u64, u8)>>,
    /// Method id → the method.
    methods: HashMap<u64, Method>,
    /// Type → its methods.
    by_type: HashMap<u64, Vec<u64>>,
}

impl DispatchIndex {
    /// The override-equivalent methods of the methods of `frontier`: of
    /// their supertypes upstream, of their subtypes downstream, transitively
    /// up to [`DISPATCH_DEPTH`] levels. At most [`DISPATCH_MAX_PER_NODE`]
    /// per node, by rank and name; the rest are returned as `cut`.
    pub(crate) fn equivalents(&self, frontier: &[u64], dir: Direction) -> Equivalents {
        let graph = match dir {
            Direction::Upstream => &self.supers,
            Direction::Downstream => &self.subs,
        };
        let mut out = Equivalents::default();
        for &origin in frontier {
            let Some(m) = self.methods.get(&origin) else {
                continue;
            };
            if never_overrides(&m.signature) {
                continue;
            }
            // The surest chain to each type within the depth.
            let mut best: HashMap<u64, u8> = HashMap::new();
            let mut layer: Vec<(u64, u8)> = vec![(m.parent, 3)];
            for _ in 0..DISPATCH_DEPTH {
                let mut next = Vec::new();
                for (t, conf) in layer {
                    for &(s, c) in graph.get(&t).into_iter().flatten() {
                        let c = conf.min(c);
                        if s != m.parent && best.get(&s).is_none_or(|&b| c > b) {
                            best.insert(s, c);
                            next.push((s, c));
                        }
                    }
                }
                layer = next;
            }
            let mine = arity(&m.signature, &m.name);
            let mut list: Vec<Equivalent> = Vec::new();
            for (t, chain) in best {
                for id in self.by_type.get(&t).into_iter().flatten() {
                    let e = &self.methods[id];
                    if *id == origin
                        || e.name != m.name
                        || never_overrides(&e.signature)
                        || !arities_meet(mine, arity(&e.signature, &e.name))
                    {
                        continue;
                    }
                    list.push(Equivalent {
                        origin,
                        origin_symbol: m.symbol.clone(),
                        id: *id,
                        symbol: e.symbol.clone(),
                        file: e.file.clone(),
                        line: e.line,
                        rank: e.rank,
                        test: e.test,
                        chain,
                    });
                }
            }
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
        out
    }
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

impl Store {
    /// The supertype graph of `branch` ([`DispatchIndex`]); `None` when the
    /// branch has no resolved supertype edge (then no level pays for
    /// dispatch). Two statements, once per walk; Go's files are left out
    /// (embedding promotes, it does not override).
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
        }
        let mut stmt = self.conn.prepare(&format!(
            "SELECT m.id, m.parent_id, m.name, m.qualified, m.file, m.start_line,
                    m.signature, m.rank, coalesce(m.is_test, false)
               FROM symbols m
              WHERE m.repo = ? AND m.branch = ? AND m.kind IN {CALLABLE_KINDS}
                AND NOT ends_with(m.file, '.go')
                AND m.parent_id IN (
                    SELECT src_id FROM live_edges
                     WHERE repo = ? AND branch = ? AND kind IN {SUPER_KINDS}
                       AND dst_id IS NOT NULL
                    UNION
                    SELECT dst_id FROM live_edges
                     WHERE repo = ? AND branch = ? AND kind IN {SUPER_KINDS}
                       AND dst_id IS NOT NULL)"
        ))?;
        let rows = stmt.query_map(
            duckdb::params![repo, branch, repo, branch, repo, branch],
            |r| {
                Ok((
                    r.get::<_, u64>(0)?,
                    Method {
                        parent: r.get(1)?,
                        name: r.get(2)?,
                        symbol: r.get(3)?,
                        file: r.get(4)?,
                        line: r.get(5)?,
                        signature: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                        rank: r.get(7)?,
                        test: r.get(8)?,
                    },
                ))
            },
        )?;
        for row in rows {
            let (id, m) = row?;
            idx.by_type.entry(m.parent).or_default().push(id);
            idx.methods.insert(id, m);
        }
        Ok(Some(idx))
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
        assert!(never_overrides("private void update(String id)"));
        assert!(never_overrides("public static Foo of(String s)"));
        assert!(!never_overrides("public void update(String staticId)"));
    }
}
