//! Lookups by symbol over the symbol graph (PLAN-009 DD-10, TASK-008): what
//! `read_symbol`, the identifier anchoring of `search`, `get_references`,
//! `external`/`suggestions` and the web graph view read once a branch has rows
//! in `symbols`.
//!
//! Every edge read goes through the `live_edges` view: a call the link pass
//! discarded (`resolution = 'discarded'`) stays in `edges` only to be reopened
//! by name, and must never reach an answer.
//!
//! The 0.9.0 readers (`symbol_definitions`, `symbol_matches`,
//! `symbol_suggestions`, `resolve_symbol`, `find_references`,
//! `external_call_sites`, `graph_edges`) stay as they are: they are the old
//! path for a branch the current extractor has not indexed (DD-19), and
//! `impact_analysis` still walks `graph_edges` until TASK-009.

use std::collections::{BTreeSet, HashMap};

use devctx_core::{SearchFilter, VectorPoint};
use duckdb::{params, params_from_iter};

use crate::error::Result;
use crate::store::{row_to_point, Store, COLS};
use crate::symbols::{row_to_symbol, StoredSymbol, SYMBOL_SELECT};

/// Kinds that hold other definitions: their code is the `class` chunk (the
/// summary of their members), never every method chunk inside their range.
const CONTAINER_KINDS: &[&str] = &[
    "class",
    "interface",
    "enum",
    "record",
    "struct",
    "trait",
    "type",
    "module",
];

/// A symbol and the chunks that hold its code (DD-4: by line range, no link
/// table). `chunks` is empty for a symbol the chunker gives no chunk of its
/// own (a field, a constant, a Rust `impl`); its `signature` stands in.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolDefinition {
    /// The definition.
    pub symbol: StoredSymbol,
    /// Its code chunks, in line order.
    pub chunks: Vec<VectorPoint>,
}

/// One occurrence of a reference to a symbol (`get_references`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolReference {
    /// File of the occurrence.
    pub file: String,
    /// 1-based line.
    pub line: i32,
    /// The enclosing symbol's qualified name (the file for module level).
    pub source: String,
    /// Edge kind: `calls`, `instantiates`, `references`, `imports`, …
    pub kind: String,
    /// The destination's id, when it resolved to a definition of the repo.
    pub dst_id: Option<u64>,
    /// The destination as the file wrote it.
    pub dst_name: String,
    /// `high`/`medium`/`low`; `None` for a row the link pass left undecided.
    pub confidence: Option<String>,
    /// Defined outside the repository, with evidence (DD-9).
    pub external: bool,
    /// The occurrence is in a test file.
    pub from_test: bool,
}

impl SymbolReference {
    /// Neither resolved nor external: the link pass could not decide.
    pub fn undecided(&self) -> bool {
        self.dst_id.is_none() && !self.external
    }
}

/// What the graph knows about a name it has no definition of (`read_symbol`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalSites {
    /// Call sites (`calls` and `instantiates`) marked external.
    pub count: usize,
    /// The distinct destination names those sites wrote, sorted.
    pub names: Vec<String>,
}

/// An edge of the web graph view, with both ends named the way `graph_edges`
/// names them (qualified source; qualified destination when resolved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewEdge {
    /// Source symbol's qualified name.
    pub source: String,
    /// Destination's qualified name, or the name as written when unresolved.
    pub target: String,
    /// Edge kind.
    pub kind: String,
    /// File of the occurrence.
    pub file: String,
    /// 1-based line.
    pub line: i32,
    /// `high`/`medium`/`low`, if decided.
    pub confidence: Option<String>,
    /// The destination is outside the repository (DD-9), not "never a source".
    pub external: bool,
    /// The occurrence is in a test file.
    pub from_test: bool,
}

/// Minimum confidence of a reference to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MinConfidence {
    /// Everything, undecided rows (name only) included.
    Low,
    /// `high` and `medium` (the default of `get_references`).
    Medium,
    /// `high` only.
    High,
}

impl MinConfidence {
    /// Parse `high`/`medium`/`low` (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    /// Does a row with this confidence pass?
    pub fn admits(self, confidence: Option<&str>) -> bool {
        let rank = match confidence {
            Some("high") => 3,
            Some("medium") => 2,
            _ => 1,
        };
        let needed = match self {
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
        };
        rank >= needed
    }
}

/// `Foo::bar` and `Foo.bar` are one name: the extractor writes Rust paths
/// with `::` in `dst_name` and every `qualified` with `.`.
fn dotted(name: &str) -> String {
    name.replace("::", ".")
}

/// The last segment of a dotted name.
fn last_segment(name: &str) -> &str {
    name.rsplit(['.', ':']).next().unwrap_or(name)
}

/// SQL expression: `col` with `::` folded to `.`.
fn dotted_sql(col: &str) -> String {
    format!("replace({col}, '::', '.')")
}

impl Store {
    /// Whether `branch` has rows in `symbols` (an index made by an extractor
    /// that writes the symbol graph). Without them every lookup takes the
    /// 0.9.0 path (DD-19).
    pub fn has_symbol_graph(&self, repo: &str, branch: &str) -> Result<bool> {
        let found: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM symbols WHERE repo = ? AND branch = ?)",
            params![repo, branch],
            |r| r.get(0),
        )?;
        Ok(found)
    }

    /// The symbols `name` designates on a branch, by `(file, line)` — fields
    /// and Rust `impl` blocks after the rest (a field is rarely the code
    /// someone reading a symbol wants, and an `impl` follows its type):
    ///
    /// - a bare name (`charge`) matches `symbols.name`;
    /// - a qualified one (`Card.charge`, `Card::charge`) matches `qualified`
    ///   exactly, as a suffix (`Inner.m` for `Outer.Inner.m`), or with its
    ///   package in front (`com.x.Card.charge`, `crate::m::Card::charge`);
    ///   failing those, a longer path that ends in a `Type.member`
    ///   (`devctx_store::Store::open` for `Store.open`);
    /// - `file::name` (`src/pay.rs::charge`, `pay.rs::charge`) restricts the
    ///   bare or qualified name to that file.
    ///
    /// The file symbol (`kind = 'file'`) is never a definition here.
    pub fn lookup_symbols(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
        file: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredSymbol>> {
        let q = dotted(name.trim());
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let order =
            format!("ORDER BY (kind IN ('impl', 'field')), file, start_line, id LIMIT {limit}");
        let (file_clause, file_args): (&str, Vec<String>) = match file {
            Some(f) => (
                " AND (file = ? OR ends_with(file, '/' || ?))",
                vec![f.to_string(), f.to_string()],
            ),
            None => ("", Vec::new()),
        };
        let run = |cond: &str, args: Vec<String>| -> Result<Vec<StoredSymbol>> {
            let sql = format!(
                "{SYMBOL_SELECT} WHERE repo = ? AND branch = ? AND kind <> 'file'
                   AND ({cond}){file_clause} {order}"
            );
            let mut all = vec![repo.to_string(), branch.to_string()];
            all.extend(args);
            all.extend(file_args.iter().cloned());
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(all), row_to_symbol)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        };
        if !q.contains('.') {
            return run("name = ?", vec![q]);
        }
        let found = run(
            &format!(
                "qualified = ? OR ends_with(qualified, '.' || ?)
                 OR (package IS NOT NULL AND package <> ''
                     AND {} || '.' || qualified = ?)",
                dotted_sql("package")
            ),
            vec![q.clone(), q.clone(), q.clone()],
        )?;
        if !found.is_empty() {
            return Ok(found);
        }
        // A longer path than the package column holds (a crate name, a
        // re-export): accept it when it ends in a `Type.member` of the repo.
        run(
            "contains(qualified, '.') AND ends_with(?, '.' || qualified)",
            vec![q],
        )
    }

    /// The chunks holding `sym`'s code (DD-4): by line overlap within its
    /// file, never a summary (`file`/`doc`), and
    ///
    /// - for a container, its `class` chunk;
    /// - for anything else, the `function`/`block` chunks named after it
    ///   (a large function is several blocks), else the `grouped` chunk of
    ///   small functions that holds it (by its list of names or its
    ///   per-function header, as the 0.9.0 lookup did).
    pub fn symbol_code_chunks(
        &self,
        repo: &str,
        branch: &str,
        sym: &StoredSymbol,
    ) -> Result<Vec<VectorPoint>> {
        let sql = format!(
            "SELECT {COLS} FROM vectors
             WHERE repo = ? AND branch = ? AND file = ? AND NOT is_deletion
               AND chunk_level NOT IN ('memory', 'memory_chunk', 'file', 'doc')
               AND start_line <= ? AND end_line >= ?
             ORDER BY start_line, end_line"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![repo, branch, sym.file, sym.end_line, sym.start_line],
            row_to_point,
        )?;
        let near = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(pick_code_chunks(sym, near))
    }

    /// [`symbol_code_chunks`](Self::symbol_code_chunks) of every symbol of
    /// `syms` in **one** query (`symbols` joined to `vectors` by file and
    /// line overlap), by symbol id: what anchoring and `read_symbol` ask for
    /// on every call, so it must not cost a scan of `vectors` per definition
    /// (review of TASK-008, M3).
    pub fn code_chunks_of(
        &self,
        repo: &str,
        branch: &str,
        syms: &[StoredSymbol],
    ) -> Result<HashMap<u64, Vec<VectorPoint>>> {
        let mut out: HashMap<u64, Vec<VectorPoint>> = HashMap::new();
        if syms.is_empty() {
            return Ok(out);
        }
        let cols = COLS
            .split(',')
            .map(|c| format!("v.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let holes = vec!["?"; syms.len()].join(", ");
        let sql = format!(
            "SELECT {cols}, s.id FROM symbols s
               JOIN vectors v ON v.repo = s.repo AND v.branch = s.branch AND v.file = s.file
                AND v.start_line <= s.end_line AND v.end_line >= s.start_line
              WHERE s.repo = ? AND s.branch = ? AND s.id IN ({holes})
                AND NOT v.is_deletion
                AND v.chunk_level NOT IN ('memory', 'memory_chunk', 'file', 'doc')
              ORDER BY s.id, v.start_line, v.end_line"
        );
        let mut args: Vec<duckdb::types::Value> =
            vec![repo.to_string().into(), branch.to_string().into()];
        args.extend(syms.iter().map(|s| duckdb::types::Value::UBigInt(s.id)));
        let mut stmt = self.conn.prepare(&sql)?;
        // `s.id` follows the `vectors` columns that `row_to_point` reads.
        let id_col = COLS.split(',').count();
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok((r.get::<_, u64>(id_col)?, row_to_point(r)?))
        })?;
        let mut near: HashMap<u64, Vec<VectorPoint>> = HashMap::new();
        for r in rows {
            let (id, p) = r?;
            near.entry(id).or_default().push(p);
        }
        for sym in syms {
            let chunks = pick_code_chunks(sym, near.remove(&sym.id).unwrap_or_default());
            out.insert(sym.id, chunks);
        }
        Ok(out)
    }

    /// `read_symbol` over the symbol graph: the definitions of `name` with
    /// their code, by `(file, line)`. `file` restricts to `file::name`.
    pub fn lookup_definitions(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
        file: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SymbolDefinition>> {
        let syms = self.lookup_symbols(repo, branch, name, file, limit)?;
        let mut chunks = self.code_chunks_of(repo, branch, &syms)?;
        Ok(syms
            .into_iter()
            .map(|symbol| SymbolDefinition {
                chunks: chunks.remove(&symbol.id).unwrap_or_default(),
                symbol,
            })
            .collect())
    }

    /// Definitions of `name` for anchoring a search (the symbol-graph
    /// counterpart of [`symbol_matches`](Self::symbol_matches)): each
    /// definition's code chunks, tagged with its id, among the rows `filter`
    /// admits. Needs the filter's repo and branch; without them it returns
    /// nothing (the caller uses the 0.9.0 path).
    pub fn lookup_anchors(
        &self,
        filter: &SearchFilter,
        name: &str,
        limit: usize,
    ) -> Result<Vec<(u64, VectorPoint)>> {
        let (Some(repo), Some(branch)) = (filter.repo.as_deref(), filter.branch.as_deref()) else {
            return Ok(Vec::new());
        };
        let syms = self.lookup_symbols(repo, branch, name, None, limit)?;
        let mut chunks = self.code_chunks_of(repo, branch, &syms)?;
        let mut out = Vec::new();
        for sym in &syms {
            for p in chunks.remove(&sym.id).unwrap_or_default() {
                if !filter.languages.is_empty() && !filter.languages.contains(&p.metadata.language)
                {
                    continue;
                }
                out.push((sym.id, p));
            }
        }
        Ok(out)
    }

    /// Up to `n` names of this branch that `name` could be a slip for, from
    /// `symbols` (`qualified`, which `read_symbol` takes back as is): the
    /// same candidate tests and ranking as
    /// [`symbol_suggestions`](Self::symbol_suggestions).
    pub fn lookup_suggestions(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
        n: usize,
    ) -> Result<Vec<String>> {
        let lname = last_segment(&dotted(name)).to_lowercase();
        if lname.chars().count() < 2 || n == 0 {
            return Ok(Vec::new());
        }
        let head: String = lname.chars().take(3).collect();
        let tail: String = {
            let c: Vec<char> = lname.chars().collect();
            c[c.len().saturating_sub(3)..].iter().collect()
        };
        let mut stmt = self.conn.prepare(
            "SELECT qualified FROM (
               SELECT DISTINCT qualified, lower(name) AS last
                 FROM symbols
                WHERE repo = ? AND branch = ? AND kind NOT IN ('file', 'impl')
                  AND (starts_with(lower(name), ?) OR ends_with(lower(name), ?)
                       OR contains(lower(name), ?)
                       OR (length(name) >= 3 AND contains(?, lower(name))))
             )
             ORDER BY levenshtein(last, ?), jaro_winkler_similarity(last, ?) DESC, qualified
             LIMIT 200",
        )?;
        let rows = stmt.query_map(
            params![repo, branch, head, tail, lname, lname, lname, lname],
            |r| r.get::<_, String>(0),
        )?;
        let cands = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(crate::store::rank_suggestions(name, cands, n))
    }

    /// Qualified names of the definitions whose bare name is `name`'s last
    /// segment: what a qualified name that matched nothing could have meant
    /// (`Foo.new` → `Thing.new`).
    pub fn lookup_same_name(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
        n: usize,
    ) -> Result<Vec<String>> {
        let last = last_segment(&dotted(name)).to_string();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT DISTINCT qualified FROM symbols
              WHERE repo = ? AND branch = ? AND kind NOT IN ('file', 'impl') AND name = ?
              ORDER BY qualified LIMIT {n}"
        ))?;
        let rows = stmt.query_map(params![repo, branch, last], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Whether some definition of the branch has this bare name.
    pub fn defines_name(&self, repo: &str, branch: &str, name: &str) -> Result<bool> {
        let found: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM symbols
                WHERE repo = ? AND branch = ? AND kind <> 'file' AND name = ?)",
            params![repo, branch, name],
            |r| r.get(0),
        )?;
        Ok(found)
    }

    /// Call sites of a name the repo does not define, marked external by the
    /// link pass (DD-9): `Some` when at least one `calls`/`instantiates` row
    /// is `external` and its destination is `name` or ends in `.name`
    /// (`::` and `.` alike, so `serde_json::from_str` and
    /// `Panache.withTransaction` are found as written).
    ///
    /// A qualified name with no such site falls back to its last segment
    /// only when no definition of the branch has that bare name (pending 2
    /// of PLAN-008): `Foo::new` with a `Thing::new` in the repo is not a
    /// library function, it is a name that does not exist.
    pub fn lookup_external(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
    ) -> Result<Option<ExternalSites>> {
        let q = dotted(name.trim());
        if let Some(found) = self.external_sites(repo, branch, &q)? {
            return Ok(Some(found));
        }
        let last = last_segment(&q);
        if last.is_empty() || last == q || self.defines_name(repo, branch, last)? {
            return Ok(None);
        }
        self.external_sites(repo, branch, last)
    }

    fn external_sites(&self, repo: &str, branch: &str, q: &str) -> Result<Option<ExternalSites>> {
        let d = dotted_sql("dst_name");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT dst_name, count(*) FROM live_edges
              WHERE repo = ? AND branch = ? AND kind IN ('calls', 'instantiates')
                AND coalesce(external, false) AND dst_id IS NULL
                AND ({d} = ? OR ends_with({d}, '.' || ?))
              GROUP BY dst_name ORDER BY dst_name"
        ))?;
        let rows = stmt.query_map(params![repo, branch, q, q], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut out = ExternalSites::default();
        for r in rows {
            let (n, c) = r?;
            out.count += c as usize;
            out.names.push(n);
        }
        Ok((out.count > 0).then_some(out))
    }

    /// Every reference to the symbols `ids` (one row per occurrence, by
    /// file and line), over `live_edges`, of the relations `kinds` (empty:
    /// every kind but `contains`).
    ///
    /// With `ids` empty, the external sites of `name` instead (the same
    /// match as [`lookup_external`](Self::lookup_external)); `external`
    /// false skips them (a `file::name` lookup is never a library's). With
    /// `undecided`, also the rows the link pass could not decide whose
    /// destination is `name` as written or ends in it
    /// ([`count_undecided_references`](Self::count_undecided_references)
    /// counts the same rows), so the caller can show them marked.
    #[allow(clippy::too_many_arguments)]
    pub fn lookup_references(
        &self,
        repo: &str,
        branch: &str,
        ids: &[u64],
        name: &str,
        kinds: &[&str],
        external: bool,
        undecided: bool,
    ) -> Result<Vec<SymbolReference>> {
        let q = dotted(name.trim());
        let d = dotted_sql("e.dst_name");
        let mut conds: Vec<String> = Vec::new();
        let mut args: Vec<duckdb::types::Value> =
            vec![repo.to_string().into(), branch.to_string().into()];
        let kind_clause = kinds_clause(kinds, &mut args);
        if !ids.is_empty() {
            conds.push(format!(
                "e.dst_id IN ({})",
                ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ")
            ));
            args.extend(ids.iter().map(|&i| duckdb::types::Value::UBigInt(i)));
        } else if external {
            let target = if self.external_sites(repo, branch, &q)?.is_some() {
                Some(q.clone())
            } else {
                let last = last_segment(&q);
                (!last.is_empty() && last != q && !self.defines_name(repo, branch, last)?)
                    .then(|| last.to_string())
            };
            if let Some(t) = target {
                conds.push(format!(
                    "(e.dst_id IS NULL AND coalesce(e.external, false)
                      AND ({d} = ? OR ends_with({d}, '.' || ?)))"
                ));
                args.push(t.clone().into());
                args.push(t.into());
            }
        }
        if undecided {
            conds.push(format!(
                "(e.dst_id IS NULL AND NOT coalesce(e.external, false)
                  AND ({d} = ? OR ends_with({d}, '.' || ?)))"
            ));
            args.push(q.clone().into());
            args.push(q.into());
        }
        if conds.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT e.file, e.line, coalesce(s.qualified, e.file) AS source, e.kind,
                    e.dst_id, e.dst_name, e.confidence, coalesce(e.external, false),
                    coalesce(e.from_test, false)
               FROM live_edges e
               LEFT JOIN symbols s ON s.repo = e.repo AND s.branch = e.branch AND s.id = e.src_id
              WHERE e.repo = ? AND e.branch = ? AND {kind_clause} AND ({})
              ORDER BY e.file, e.line, source, e.kind",
            conds.join(" OR ")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok(SymbolReference {
                file: r.get(0)?,
                line: r.get::<_, Option<i32>>(1)?.unwrap_or(0),
                source: r.get(2)?,
                kind: r.get(3)?,
                dst_id: r.get(4)?,
                dst_name: r.get(5)?,
                confidence: r.get(6)?,
                external: r.get(7)?,
                from_test: r.get(8)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// How many rows of the relations `kinds` the link pass left undecided
    /// (no destination, not external) whose destination is `name` as
    /// written (`::` and `.` alike) or ends in `.name`: a `count(*)`, for an
    /// answer that does not list them (review of TASK-008, m3). A bare name
    /// counts every undecided call of that name; a qualified one only those
    /// written that way.
    pub fn count_undecided_references(
        &self,
        repo: &str,
        branch: &str,
        name: &str,
        kinds: &[&str],
    ) -> Result<usize> {
        let q = dotted(name.trim());
        let d = dotted_sql("dst_name");
        let mut args: Vec<duckdb::types::Value> =
            vec![repo.to_string().into(), branch.to_string().into()];
        let kind_clause = kinds_clause(kinds, &mut args).replace("e.kind", "kind");
        args.push(q.clone().into());
        args.push(q.into());
        let n: i64 = self.conn.query_row(
            &format!(
                "SELECT count(*) FROM live_edges
                  WHERE repo = ? AND branch = ? AND {kind_clause}
                    AND dst_id IS NULL AND NOT coalesce(external, false)
                    AND ({d} = ? OR ends_with({d}, '.' || ?))"
            ),
            params_from_iter(args),
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// The web graph view over the symbol graph: `live_edges` but
    /// `contains` (or only `kind`), optionally from one file, capped at
    /// `limit` (0 = 2000), by source then target. `external` and `from_test`
    /// are the link pass's marks.
    pub fn lookup_graph_view(
        &self,
        repo: &str,
        branch: &str,
        kind: Option<&str>,
        file: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ViewEdge>> {
        let mut sql = String::from(
            "SELECT * FROM (
               SELECT coalesce(s.qualified, e.file) AS source,
                      coalesce(t.qualified, e.dst_name) AS target,
                      e.kind, e.file, coalesce(e.line, 0) AS line, e.confidence,
                      coalesce(e.external, false) AS external,
                      coalesce(e.from_test, false) AS from_test
                 FROM live_edges e
                 LEFT JOIN symbols s ON s.repo = e.repo AND s.branch = e.branch AND s.id = e.src_id
                 LEFT JOIN symbols t ON t.repo = e.repo AND t.branch = e.branch AND t.id = e.dst_id
                WHERE e.repo = ? AND e.branch = ?",
        );
        let mut args: Vec<String> = vec![repo.to_string(), branch.to_string()];
        match kind {
            Some(k) => {
                sql.push_str(" AND e.kind = ?");
                args.push(k.to_string());
            }
            None => sql.push_str(" AND e.kind <> 'contains'"),
        }
        if let Some(f) = file {
            sql.push_str(" AND e.file = ?");
            args.push(f.to_string());
        }
        let cap = if limit == 0 { 2000 } else { limit };
        sql.push_str(&format!(
            ") ORDER BY source, target, file, line LIMIT {cap}"
        ));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok(ViewEdge {
                source: r.get(0)?,
                target: r.get(1)?,
                kind: r.get(2)?,
                file: r.get(3)?,
                line: r.get(4)?,
                confidence: r.get(5)?,
                external: r.get(6)?,
                from_test: r.get(7)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Distinct qualified names of `syms`, sorted (`resolved_symbols`).
    pub fn qualified_names(syms: &[StoredSymbol]) -> Vec<String> {
        syms.iter()
            .map(|s| s.qualified.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Of the chunks overlapping `sym`'s lines (in line order), the ones that
/// hold its code: see [`Store::symbol_code_chunks`].
fn pick_code_chunks(sym: &StoredSymbol, near: Vec<VectorPoint>) -> Vec<VectorPoint> {
    let named = |p: &VectorPoint| p.metadata.symbol == sym.name;
    if CONTAINER_KINDS.contains(&sym.kind.as_str()) {
        return near
            .into_iter()
            .filter(|p| p.metadata.chunk_level == "class" && named(p))
            .collect();
    }
    let own: Vec<VectorPoint> = near
        .iter()
        .filter(|p| {
            p.metadata.chunk_level != "class" && p.metadata.symbol_type != "grouped" && named(p)
        })
        .cloned()
        .collect();
    if !own.is_empty() {
        return own;
    }
    let header = format!("> {}", sym.name);
    near.into_iter()
        .filter(|p| {
            p.metadata.symbol_type == "grouped"
                && (grouped_names(&p.metadata.symbol).any(|n| n == sym.name)
                    || p.text
                        .lines()
                        .any(|l| l.starts_with("# ") && l.ends_with(&header)))
        })
        .take(1)
        .collect()
}

/// SQL condition on `e.kind` for `kinds` (empty: every kind but
/// `contains`), pushing its parameters.
fn kinds_clause(kinds: &[&str], args: &mut Vec<duckdb::types::Value>) -> String {
    if kinds.is_empty() {
        return "e.kind <> 'contains'".to_string();
    }
    args.extend(
        kinds
            .iter()
            .map(|k| duckdb::types::Value::from(k.to_string())),
    );
    format!("e.kind IN ({})", vec!["?"; kinds.len()].join(", "))
}

/// The names a `grouped` chunk lists (`a, b, c` or `a, b, c, d +2`).
fn grouped_names(symbol: &str) -> impl Iterator<Item = &str> {
    let list = match symbol.rsplit_once(" +") {
        Some((head, n)) if n.chars().all(|c| c.is_ascii_digit()) => head,
        _ => symbol,
    };
    list.split(", ")
}

/// The id as the JSON shows it: 16 lowercase hex digits (DD-3).
pub fn sym_hex(id: u64) -> String {
    format!("{id:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_confidence_admits_by_rank() {
        let m = MinConfidence::Medium;
        assert!(m.admits(Some("high")) && m.admits(Some("medium")));
        assert!(!m.admits(Some("low")) && !m.admits(None));
        assert!(MinConfidence::Low.admits(None));
        assert!(!MinConfidence::High.admits(Some("medium")));
        assert_eq!(MinConfidence::parse(" LOW "), Some(MinConfidence::Low));
        assert_eq!(MinConfidence::parse("x"), None);
    }

    #[test]
    fn sym_is_sixteen_hex_digits() {
        assert_eq!(sym_hex(0xab), "00000000000000ab");
        assert_eq!(sym_hex(u64::MAX).len(), 16);
    }

    #[test]
    fn grouped_names_drop_the_overflow_count() {
        assert_eq!(grouped_names("a, b +2").collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(grouped_names("a").collect::<Vec<_>>(), ["a"]);
    }
}
