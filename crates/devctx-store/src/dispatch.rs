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

impl Store {
    /// Whether `branch` has any resolved `inherits`/`implements` edge: when it
    /// has none, no level pays for the dispatch query. One statement per
    /// walk, not per level.
    pub(crate) fn has_supertype_edges(&self, repo: &str, branch: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT EXISTS (SELECT 1 FROM live_edges
                             WHERE repo = ? AND branch = ? AND kind IN {SUPER_KINDS}
                               AND dst_id IS NOT NULL)"
        ))?;
        Ok(stmt.query_row(duckdb::params![repo, branch], |r| r.get(0))?)
    }

    /// The override-equivalent methods of the methods of `frontier` (one
    /// statement): of their supertypes upstream, of their subtypes
    /// downstream, transitively up to [`DISPATCH_DEPTH`] levels, over
    /// `live_edges`. At most [`DISPATCH_MAX_PER_NODE`] per node, by rank and
    /// name; the rest are returned as `cut`.
    pub(crate) fn override_equivalents(
        &self,
        repo: &str,
        branch: &str,
        frontier: &[u64],
        dir: Direction,
        mode: FrontierSql,
    ) -> Result<Equivalents> {
        if frontier.is_empty() {
            return Ok(Equivalents::default());
        }
        let mut args: Vec<duckdb::types::Value> =
            vec![repo.to_string().into(), branch.to_string().into()];
        let set = self.frontier_set(frontier, mode, &mut args)?;
        args.extend([
            repo.to_string().into(),
            branch.to_string().into(),
            repo.to_string().into(),
            branch.to_string().into(),
        ]);
        // Upstream climbs from a type to its supertypes (`src` → `dst`),
        // downstream descends from a type to its subtypes.
        let (this, next) = match dir {
            Direction::Upstream => ("src_id", "dst_id"),
            Direction::Downstream => ("dst_id", "src_id"),
        };
        let sql = format!(
            "WITH RECURSIVE
             f AS (
               SELECT s.id, s.name, s.parent_id, s.qualified, s.signature FROM symbols s
                WHERE s.repo = ? AND s.branch = ? AND s.id IN {set}
                  AND s.parent_id IS NOT NULL AND s.kind IN {CALLABLE_KINDS}
                  AND NOT ends_with(s.file, '.go')
             ),
             chain(origin, name, t, d, conf) AS (
               SELECT id, name, parent_id, 0, 3 FROM f
               UNION ALL
               SELECT c.origin, c.name, e.{next}, c.d + 1,
                      least(c.conf, CASE e.confidence WHEN 'high' THEN 3
                                                      WHEN 'medium' THEN 2 ELSE 1 END)
                 FROM chain c
                 JOIN live_edges e ON e.repo = ? AND e.branch = ? AND e.{this} = c.t
                                  AND e.kind IN {SUPER_KINDS} AND e.dst_id IS NOT NULL
                WHERE c.d < {DISPATCH_DEPTH}
             )
             SELECT c.origin, f.qualified, f.signature, m.id, m.qualified, m.file,
                    m.start_line, m.signature, m.rank, coalesce(m.is_test, false), max(c.conf)
               FROM chain c
               JOIN f ON f.id = c.origin
               JOIN symbols m ON m.repo = ? AND m.branch = ? AND m.parent_id = c.t
                             AND m.name = c.name AND m.kind IN {CALLABLE_KINDS}
              WHERE c.d > 0 AND m.id <> c.origin
              GROUP BY ALL"
        );
        count_statement();
        let mut stmt = self.conn.prepare(&sql)?;
        type Row = (
            u64,
            String,
            Option<String>,
            u64,
            String,
            Option<String>,
            Option<i32>,
            Option<String>,
            Option<f64>,
            bool,
            i32,
        );
        let rows = stmt.query_map(params_from_iter(args), |r| -> duckdb::Result<Row> {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
            ))
        })?;
        let mut by_origin: HashMap<u64, Vec<Equivalent>> = HashMap::new();
        for row in rows {
            let (origin, oq, osig, id, q, file, line, sig, rank, test, conf) = row?;
            let name = q.rsplit(['.', ':']).next().unwrap_or(&q).to_string();
            let (osig, sig) = (osig.unwrap_or_default(), sig.unwrap_or_default());
            if never_overrides(&osig) || never_overrides(&sig) {
                continue;
            }
            if !arities_meet(arity(&osig, &name), arity(&sig, &name)) {
                continue;
            }
            let list = by_origin.entry(origin).or_default();
            // Two chains to the same method (a diamond): the surer one.
            match list.iter_mut().find(|e| e.id == id) {
                Some(e) => e.chain = e.chain.max(conf.clamp(1, 3) as u8),
                None => list.push(Equivalent {
                    origin,
                    origin_symbol: oq,
                    id,
                    symbol: q,
                    file,
                    line,
                    rank,
                    test,
                    chain: conf.clamp(1, 3) as u8,
                }),
            }
        }
        let mut origins: Vec<u64> = by_origin.keys().copied().collect();
        origins.sort_unstable();
        let mut out = Equivalents::default();
        for o in origins {
            let mut list = by_origin.remove(&o).unwrap_or_default();
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
        assert!(never_overrides("private void update(String id)"));
        assert!(never_overrides("public static Foo of(String s)"));
        assert!(!never_overrides("public void update(String staticId)"));
    }
}
