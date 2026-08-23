//! Call-graph (`graph_edges`) operations: edge upsert, callers/callees,
//! references, and blast-radius impact analysis.

use std::collections::{HashSet, VecDeque};

use duckdb::{params, params_from_iter};

use crate::error::Result;
use crate::store::Store;

/// A call-graph edge to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEdge {
    /// Enclosing symbol making the call.
    pub source: String,
    /// Called symbol.
    pub target: String,
    /// Edge kind (`calls`).
    pub kind: String,
    /// File the call occurs in.
    pub source_file: String,
    /// 1-based line.
    pub line: i32,
}

/// A raw call-graph edge, for bulk export (graph visualization).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdge {
    /// Enclosing (calling) symbol.
    pub source: String,
    /// Called symbol.
    pub target: String,
    /// Edge kind (`calls`).
    pub kind: String,
    /// File the call occurs in.
    pub source_file: String,
    /// 1-based line.
    pub line: i32,
}

/// A reference to a symbol (a call site).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// File containing the reference.
    pub file: String,
    /// 1-based line.
    pub line: i32,
    /// Symbol making the reference.
    pub source: String,
}

/// Transitive callers (upstream) and callees (downstream) of a symbol, with depth.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImpactResult {
    /// Transitive callers (who is affected if the symbol changes), with depth.
    pub upstream: Vec<(String, usize)>,
    /// Transitive callees (what the symbol depends on), with depth.
    pub downstream: Vec<(String, usize)>,
}

impl Store {
    /// Replace all edges originating in `source_file` with `edges` (deduped by
    /// source/target/kind to satisfy the uniqueness constraint).
    pub fn replace_file_edges(
        &self,
        repo: &str,
        branch: &str,
        source_file: &str,
        edges: &[StoredEdge],
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM graph_edges WHERE repo = ? AND branch = ? AND source_file = ?",
            params![repo, branch, source_file],
        )?;
        let mut seen = HashSet::new();
        for e in edges {
            if !seen.insert((&e.source, &e.target, &e.kind)) {
                continue;
            }
            self.conn.execute(
                "INSERT INTO graph_edges
                    (source, target, kind, source_file, target_file, line, repo, branch, metadata)
                 VALUES (?, ?, ?, ?, '', ?, ?, ?, '')",
                params![
                    e.source,
                    e.target,
                    e.kind,
                    e.source_file,
                    e.line,
                    repo,
                    branch
                ],
            )?;
        }
        Ok(())
    }

    /// Delete all edges originating in a file.
    pub fn delete_file_edges(&self, repo: &str, branch: &str, source_file: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM graph_edges WHERE repo = ? AND branch = ? AND source_file = ?",
            params![repo, branch, source_file],
        )?;
        Ok(())
    }

    /// Bulk-export call-graph edges for a repo/branch, for visualization.
    ///
    /// Optionally filtered to a single edge `kind` or `source_file`, and capped
    /// at `limit` edges (0 = a generous default). Ordered by source/target so
    /// the result is deterministic.
    pub fn graph_edges(
        &self,
        repo: &str,
        branch: &str,
        kind: Option<&str>,
        source_file: Option<&str>,
        limit: usize,
    ) -> Result<Vec<GraphEdge>> {
        let mut sql = String::from(
            "SELECT source, target, kind, source_file, line FROM graph_edges
             WHERE repo = ? AND branch = ?",
        );
        let mut args: Vec<String> = vec![repo.to_string(), branch.to_string()];
        if let Some(k) = kind {
            sql.push_str(" AND kind = ?");
            args.push(k.to_string());
        }
        if let Some(f) = source_file {
            sql.push_str(" AND source_file = ?");
            args.push(f.to_string());
        }
        let cap = if limit == 0 { 2000 } else { limit };
        sql.push_str(&format!(" ORDER BY source, target LIMIT {cap}"));

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(duckdb::params_from_iter(args.iter()), |r| {
            Ok(GraphEdge {
                source: r.get(0)?,
                target: r.get(1)?,
                kind: r.get(2)?,
                source_file: r.get(3)?,
                line: r.get(4)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Distinct locally-defined symbols (every symbol that makes a call) for a
    /// repo/branch. Used to classify call targets as internal vs external for
    /// graph visualization, independent of any display limit.
    pub fn graph_defined_symbols(&self, repo: &str, branch: &str) -> Result<HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT source FROM graph_edges WHERE repo = ? AND branch = ?")?;
        let rows = stmt.query_map(params![repo, branch], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<HashSet<_>, _>>()
            .map_err(Into::into)
    }

    /// Every name in the graph that `symbol` could mean.
    ///
    /// An edge's `source` is written qualified (`Class.method`) whenever the
    /// calling function sits inside a container, but its `target` is qualified
    /// only when the call site had a receiver whose type could be resolved. In
    /// a language where every method lives in a class — Java — that leaves a
    /// bare name unable to match any `source` at all, and able to match a
    /// `target` only by luck of how the call happened to be written. Measured
    /// on a Java/Quarkus repository: `actualizar` returned nothing while
    /// `OficinaService.actualizar` returned one caller and twenty-three
    /// callees. The edges were never missing — the key was.
    ///
    /// So a bare name expands to every qualified form carrying it. A name that
    /// is already qualified is returned untouched: `OficinaService.actualizar`
    /// has to keep meaning exactly one thing. A name nothing matches returns as
    /// itself, so an absent symbol still yields an empty result rather than an
    /// error.
    ///
    /// Callers are expected to report an expansion wider than one. Folding
    /// seven distinct `actualizar` methods into a single blast radius without
    /// saying so is worse than returning nothing.
    pub fn resolve_symbol(&self, repo: &str, branch: &str, symbol: &str) -> Result<Vec<String>> {
        if symbol.contains('.') {
            return Ok(vec![symbol.to_string()]);
        }
        // `ends_with` rather than `LIKE '%.' || ?`: in LIKE an underscore is a
        // single-character wildcard, and identifiers are full of underscores —
        // `find_by_id` would also match `findXbyXid`.
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT source AS name FROM graph_edges
              WHERE repo = ? AND branch = ?
                AND (source = ? OR ends_with(source, '.' || ?))
             UNION
             SELECT DISTINCT target AS name FROM graph_edges
              WHERE repo = ? AND branch = ?
                AND (target = ? OR ends_with(target, '.' || ?))
             ORDER BY name",
        )?;
        let rows = stmt.query_map(
            params![repo, branch, symbol, symbol, repo, branch, symbol, symbol],
            |r| r.get::<_, String>(0),
        )?;
        let found: Vec<String> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(if found.is_empty() {
            vec![symbol.to_string()]
        } else {
            found
        })
    }

    /// Direct callers of `symbol` (sources of edges targeting it).
    pub fn get_callers(&self, repo: &str, branch: &str, symbol: &str) -> Result<Vec<String>> {
        let names = self.resolve_symbol(repo, branch, symbol)?;
        self.neighbours_of(repo, branch, &names, true)
    }

    /// Direct callees of `symbol` (targets of edges from it).
    pub fn get_callees(&self, repo: &str, branch: &str, symbol: &str) -> Result<Vec<String>> {
        let names = self.resolve_symbol(repo, branch, symbol)?;
        self.neighbours_of(repo, branch, &names, false)
    }

    /// Neighbours of an exact set of names — no expansion. Traversal uses this:
    /// the names it walks came out of the graph already, so re-expanding them
    /// would pull in unrelated homonyms at every hop.
    fn neighbours_of(
        &self,
        repo: &str,
        branch: &str,
        names: &[String],
        callers: bool,
    ) -> Result<Vec<String>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let (select, matched) = if callers {
            ("source", "target")
        } else {
            ("target", "source")
        };
        let holes = vec!["?"; names.len()].join(", ");
        let sql = format!(
            "SELECT DISTINCT {select} FROM graph_edges
              WHERE repo = ? AND branch = ? AND {matched} IN ({holes})"
        );
        let mut args: Vec<String> = vec![repo.to_string(), branch.to_string()];
        args.extend(names.iter().cloned());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// All references (call sites) of `symbol`.
    pub fn find_references(
        &self,
        repo: &str,
        branch: &str,
        symbol: &str,
    ) -> Result<Vec<Reference>> {
        let names = self.resolve_symbol(repo, branch, symbol)?;
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let holes = vec!["?"; names.len()].join(", ");
        let sql = format!(
            "SELECT source_file, line, source FROM graph_edges
              WHERE repo = ? AND branch = ? AND target IN ({holes})
             ORDER BY source_file, line"
        );
        let mut args: Vec<String> = vec![repo.to_string(), branch.to_string()];
        args.extend(names);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok(Reference {
                file: r.get(0)?,
                line: r.get(1)?,
                source: r.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Blast radius: transitive callers + callees up to `max_depth`.
    pub fn impact_analysis(
        &self,
        repo: &str,
        branch: &str,
        symbol: &str,
        max_depth: usize,
    ) -> Result<ImpactResult> {
        Ok(ImpactResult {
            upstream: self.bfs(repo, branch, symbol, max_depth, true)?,
            downstream: self.bfs(repo, branch, symbol, max_depth, false)?,
        })
    }

    fn bfs(
        &self,
        repo: &str,
        branch: &str,
        start: &str,
        max_depth: usize,
        callers: bool,
    ) -> Result<Vec<(String, usize)>> {
        // Only the starting point expands. Every name reached from here came
        // out of the graph as written, so it is walked exactly as it is.
        let seeds = self.resolve_symbol(repo, branch, start)?;

        // Two sets, because a seed is both a starting point and a reachable
        // node. In `OficinaResource.actualizar -> OficinaService.actualizar`
        // — the dominant shape in a Quarkus codebase — both ends answer to the
        // bare name `actualizar`, so both are seeds *and* one is genuinely the
        // caller of the other. Suppressing a seed from the output would drop
        // exactly the edge the question was about.
        let mut walked: HashSet<String> = seeds.iter().cloned().collect();
        let mut reported: HashSet<String> = HashSet::from([start.to_string()]);
        let mut queue: VecDeque<(String, usize)> = seeds.into_iter().map(|s| (s, 0)).collect();
        let mut out = Vec::new();
        while let Some((node, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let neighbours =
                self.neighbours_of(repo, branch, std::slice::from_ref(&node), callers)?;
            for n in neighbours {
                if reported.insert(n.clone()) {
                    out.push((n.clone(), depth + 1));
                    // A seed reached as a neighbour is reported, but not walked
                    // a second time.
                    if walked.insert(n.clone()) {
                        queue.push_back((n, depth + 1));
                    }
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(source: &str, target: &str, file: &str, line: i32) -> StoredEdge {
        StoredEdge {
            source: source.into(),
            target: target.into(),
            kind: "calls".into(),
            source_file: file.into(),
            line,
        }
    }

    fn seeded() -> Store {
        let store = Store::open_in_memory(3).unwrap();
        // a -> b -> c ; a -> d
        store
            .replace_file_edges(
                "repo",
                "main",
                "a.rs",
                &[edge("a", "b", "a.rs", 2), edge("a", "d", "a.rs", 3)],
            )
            .unwrap();
        store
            .replace_file_edges("repo", "main", "b.rs", &[edge("b", "c", "b.rs", 5)])
            .unwrap();
        store
    }

    #[test]
    fn callers_callees_and_references() {
        let store = seeded();
        assert_eq!(store.get_callees("repo", "main", "a").unwrap().len(), 2);
        assert_eq!(store.get_callers("repo", "main", "c").unwrap(), vec!["b"]);
        let refs = store.find_references("repo", "main", "b").unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].source, "a");
        assert_eq!(refs[0].line, 2);
    }

    #[test]
    fn impact_bfs_upstream_and_downstream() {
        let store = seeded();
        let impact = store.impact_analysis("repo", "main", "c", 5).unwrap();
        // upstream of c: b (1), a (2)
        assert!(impact.upstream.contains(&("b".to_string(), 1)));
        assert!(impact.upstream.contains(&("a".to_string(), 2)));
        // downstream of a: b (1), d (1), c (2)
        let down = store
            .impact_analysis("repo", "main", "a", 5)
            .unwrap()
            .downstream;
        assert!(down.contains(&("b".to_string(), 1)));
        assert!(down.contains(&("c".to_string(), 2)));
    }

    /// The shape a Java repository actually produces: every `source` qualified
    /// because every method lives in a class, and a `target` qualified only
    /// when the call site had a receiver whose type resolved.
    fn java_like() -> Store {
        let store = Store::open_in_memory(3).unwrap();
        store
            .replace_file_edges(
                "repo",
                "main",
                "OficinaResource.java",
                &[edge(
                    "OficinaResource.actualizar",
                    "OficinaService.actualizar",
                    "OficinaResource.java",
                    129,
                )],
            )
            .unwrap();
        store
            .replace_file_edges(
                "repo",
                "main",
                "TicketResource.java",
                &[edge(
                    "TicketResource.actualizar",
                    "TicketService.actualizar",
                    "TicketResource.java",
                    148,
                )],
            )
            .unwrap();
        store
            .replace_file_edges(
                "repo",
                "main",
                "OficinaService.java",
                &[edge(
                    "OficinaService.actualizar",
                    "Oficina.persist",
                    "OficinaService.java",
                    121,
                )],
            )
            .unwrap();
        store
    }

    /// The defect this whole change exists for. `actualizar` is written into
    /// the graph only as `Clase.actualizar`, so an exact-match lookup on the
    /// bare name returned nothing at all — which reads as "this symbol is not
    /// called" and is the most expensive wrong answer the graph can give.
    #[test]
    fn a_bare_name_finds_the_edges_written_under_its_qualified_forms() {
        let store = java_like();

        let callers = store.get_callers("repo", "main", "actualizar").unwrap();
        assert!(callers.contains(&"OficinaResource.actualizar".to_string()));
        assert!(callers.contains(&"TicketResource.actualizar".to_string()));

        let callees = store.get_callees("repo", "main", "actualizar").unwrap();
        assert!(callees.contains(&"Oficina.persist".to_string()));

        let refs = store.find_references("repo", "main", "actualizar").unwrap();
        assert_eq!(refs.len(), 2, "both call sites, not zero");
    }

    /// Expansion is what the caller has to be told about: four declarations
    /// answer to `actualizar` here, and a blast radius that silently merges
    /// them is worse than an empty one.
    #[test]
    fn resolving_a_bare_name_lists_every_declaration_it_could_mean() {
        let store = java_like();
        let names = store.resolve_symbol("repo", "main", "actualizar").unwrap();
        assert_eq!(
            names,
            vec![
                "OficinaResource.actualizar",
                "OficinaService.actualizar",
                "TicketResource.actualizar",
                "TicketService.actualizar",
            ]
        );
    }

    /// The other half of the contract: asking for one specific method has to
    /// keep meaning one specific method. If qualifying a name stopped
    /// narrowing the answer, there would be no way left to disambiguate.
    #[test]
    fn a_qualified_name_does_not_collect_its_homonyms() {
        let store = java_like();
        assert_eq!(
            store
                .resolve_symbol("repo", "main", "OficinaService.actualizar")
                .unwrap(),
            vec!["OficinaService.actualizar"]
        );
        assert_eq!(
            store
                .get_callers("repo", "main", "OficinaService.actualizar")
                .unwrap(),
            vec!["OficinaResource.actualizar"]
        );
    }

    /// A name unknown to the graph answers empty, not with an error and not by
    /// matching everything.
    #[test]
    fn an_unknown_name_resolves_to_itself_and_finds_nothing() {
        let store = java_like();
        assert_eq!(
            store.resolve_symbol("repo", "main", "noExiste").unwrap(),
            vec!["noExiste"]
        );
        assert!(store
            .get_callers("repo", "main", "noExiste")
            .unwrap()
            .is_empty());
    }

    /// Expansion applies to the question, never to the walk. The names a
    /// traversal reaches came out of the graph already; expanding them again
    /// at every hop would drag in an unrelated homonym and report it as part
    /// of the blast radius.
    #[test]
    fn traversal_does_not_re_expand_the_names_it_walks() {
        let store = java_like();
        // `Oficina.persist` calls a bare `flush`; an unrelated `Repo.flush`
        // exists and calls something that must never surface here.
        store
            .replace_file_edges(
                "repo",
                "main",
                "Oficina.java",
                &[edge("Oficina.persist", "flush", "Oficina.java", 40)],
            )
            .unwrap();
        store
            .replace_file_edges(
                "repo",
                "main",
                "Repo.java",
                &[edge("Repo.flush", "noDebeAparecer", "Repo.java", 12)],
            )
            .unwrap();

        let down = store
            .impact_analysis("repo", "main", "OficinaService.actualizar", 5)
            .unwrap()
            .downstream;
        let reached: Vec<&str> = down.iter().map(|(n, _)| n.as_str()).collect();
        assert!(reached.contains(&"Oficina.persist"));
        assert!(reached.contains(&"flush"));
        assert!(
            !reached.contains(&"noDebeAparecer"),
            "the walk expanded `flush` into `Repo.flush` and followed it: {reached:?}"
        );
    }

    /// Seeding, on the other hand, does expand: the question was asked with a
    /// bare name, so every declaration behind it is a legitimate entry point.
    ///
    /// And a seed still gets reported when it is reached as a neighbour. In
    /// `OficinaResource.actualizar -> OficinaService.actualizar` both ends
    /// answer to `actualizar`; treating a seed as already-seen would delete the
    /// one edge the question was actually about.
    #[test]
    fn impact_seeds_from_every_form_of_a_bare_name() {
        let store = java_like();
        let impact = store.impact_analysis("repo", "main", "actualizar", 5).unwrap();
        assert!(impact
            .upstream
            .contains(&("OficinaResource.actualizar".to_string(), 1)));
        assert!(impact
            .upstream
            .contains(&("TicketResource.actualizar".to_string(), 1)));
        assert!(impact
            .downstream
            .contains(&("Oficina.persist".to_string(), 1)));
    }

    /// The repair path for a database whose ART indexes lost their entries to a
    /// replayed write-ahead log: it has to put the rows back untouched, and the
    /// constraints they came with have to work afterwards — a rebuild that
    /// quietly dropped the `UNIQUE` would look like a success and corrupt the
    /// graph on the next run.
    #[test]
    fn rebuilding_indexes_keeps_the_rows_and_the_constraints() {
        let store = seeded();
        let before = store.graph_edges("repo", "main", None, None, 0).unwrap();

        let repaired = store.rebuild_indexes().unwrap();
        assert!(
            repaired.iter().any(|t| t == "graph_edges"),
            "rebuilt: {repaired:?}"
        );

        let after = store.graph_edges("repo", "main", None, None, 0).unwrap();
        assert_eq!(before, after, "the rows must survive untouched");

        // Replacing a file's edges deletes and re-inserts: the operation that
        // fails on a damaged index, and the reason repair exists.
        store
            .replace_file_edges("repo", "main", "a.rs", &[edge("a", "b", "a.rs", 9)])
            .unwrap();
        let a_edges = store
            .graph_edges("repo", "main", None, Some("a.rs"), 0)
            .unwrap();
        assert_eq!(a_edges.len(), 1, "the delete half took effect");
    }

    #[test]
    fn graph_edges_bulk_export_and_filters() {
        let store = seeded();
        // All edges for repo/main: a->b, a->d, b->c.
        let all = store.graph_edges("repo", "main", None, None, 0).unwrap();
        assert_eq!(all.len(), 3);
        // Deterministic order by (source, target): a->b, a->d, b->c.
        assert_eq!((all[0].source.as_str(), all[0].target.as_str()), ("a", "b"));
        assert_eq!((all[2].source.as_str(), all[2].target.as_str()), ("b", "c"));
        // Filter by source_file: only a.rs edges (a->b, a->d).
        let a = store
            .graph_edges("repo", "main", None, Some("a.rs"), 0)
            .unwrap();
        assert_eq!(a.len(), 2);
        assert!(a.iter().all(|e| e.source_file == "a.rs"));
        // Limit caps the result.
        assert_eq!(
            store
                .graph_edges("repo", "main", None, None, 1)
                .unwrap()
                .len(),
            1
        );
        // Filter by kind: all are "calls".
        assert_eq!(
            store
                .graph_edges("repo", "main", Some("calls"), None, 0)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn replace_dedups_and_replaces() {
        let store = Store::open_in_memory(3).unwrap();
        // Duplicate (source,target,kind) collapses to one edge.
        store
            .replace_file_edges(
                "r",
                "m",
                "f.rs",
                &[edge("x", "y", "f.rs", 1), edge("x", "y", "f.rs", 2)],
            )
            .unwrap();
        assert_eq!(store.get_callers("r", "m", "y").unwrap(), vec!["x"]);
        // Re-running replaces (still one edge, not appended).
        store
            .replace_file_edges("r", "m", "f.rs", &[edge("x", "y", "f.rs", 9)])
            .unwrap();
        assert_eq!(store.find_references("r", "m", "y").unwrap().len(), 1);
    }
}
