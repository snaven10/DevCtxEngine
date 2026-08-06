//! Call-graph (`graph_edges`) operations: edge upsert, callers/callees,
//! references, and blast-radius impact analysis.

use std::collections::{HashSet, VecDeque};

use duckdb::params;

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

    /// Direct callers of `symbol` (sources of edges targeting it).
    pub fn get_callers(&self, repo: &str, branch: &str, symbol: &str) -> Result<Vec<String>> {
        self.distinct(
            "SELECT DISTINCT source FROM graph_edges
             WHERE repo = ? AND branch = ? AND target = ?",
            repo,
            branch,
            symbol,
        )
    }

    /// Direct callees of `symbol` (targets of edges from it).
    pub fn get_callees(&self, repo: &str, branch: &str, symbol: &str) -> Result<Vec<String>> {
        self.distinct(
            "SELECT DISTINCT target FROM graph_edges
             WHERE repo = ? AND branch = ? AND source = ?",
            repo,
            branch,
            symbol,
        )
    }

    /// All references (call sites) of `symbol`.
    pub fn find_references(
        &self,
        repo: &str,
        branch: &str,
        symbol: &str,
    ) -> Result<Vec<Reference>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_file, line, source FROM graph_edges
             WHERE repo = ? AND branch = ? AND target = ?
             ORDER BY source_file, line",
        )?;
        let rows = stmt.query_map(params![repo, branch, symbol], |r| {
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
        let mut visited: HashSet<String> = HashSet::from([start.to_string()]);
        let mut queue: VecDeque<(String, usize)> = VecDeque::from([(start.to_string(), 0)]);
        let mut out = Vec::new();
        while let Some((node, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let neighbors = if callers {
                self.get_callers(repo, branch, &node)?
            } else {
                self.get_callees(repo, branch, &node)?
            };
            for n in neighbors {
                if visited.insert(n.clone()) {
                    out.push((n.clone(), depth + 1));
                    queue.push_back((n, depth + 1));
                }
            }
        }
        Ok(out)
    }

    fn distinct(&self, sql: &str, repo: &str, branch: &str, symbol: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![repo, branch, symbol], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
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
