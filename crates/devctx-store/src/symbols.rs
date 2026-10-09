//! The symbol graph (`symbols` + `edges`, PLAN-009 DD-2): per-file writers,
//! basic reads, and the maintenance a file's rows need when it is deleted,
//! renamed or copied to another branch.
//!
//! Every write deletes the file's rows and appends the new ones with DuckDB's
//! `Appender` — one call per table rather than a statement per row, which is
//! what an occurrence-per-row table (often thousands of rows per file) needs.
//! The appender joins the caller's transaction (`Store::in_transaction`), so
//! a file's vectors, `graph_edges`, symbols and edges commit together.

use duckdb::params;

use crate::error::Result;
use crate::store::Store;

/// A row of `symbols`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoredSymbol {
    /// Stable id (DD-3): the same on every branch.
    pub id: u64,
    /// Container's id; the file symbol's for a top-level symbol; `None` for
    /// the file symbol.
    pub parent_id: Option<u64>,
    /// Repo-relative path.
    pub file: String,
    /// `file`/`class`/`method`/…
    pub kind: String,
    /// Bare name (`actualizar`; the base name for the file symbol).
    pub name: String,
    /// `OfficeService.actualizar`; the path for the file symbol.
    pub qualified: String,
    /// The nearest container's name, if any.
    pub container: Option<String>,
    /// Package / module, once the structured extraction knows it.
    pub package: Option<String>,
    /// Definition head without the body.
    pub signature: Option<String>,
    /// 1-based first line.
    pub start_line: i32,
    /// 1-based last line.
    pub end_line: i32,
    /// Byte offset of the definition.
    pub start_byte: i64,
    /// Byte offset of its end.
    pub end_byte: i64,
    /// public / export / pub, once known.
    pub exported: Option<bool>,
    /// Global PageRank, set by the link pass.
    pub rank: Option<f64>,
    /// Resolved non-test incoming calls, set by the link pass.
    pub in_degree: Option<i32>,
    /// The file is a test (`path_kind`).
    pub is_test: bool,
    /// The file symbol's only: the `content_hash` of the file the rows were
    /// parsed from, the same value `file_state` records. Lets
    /// [`Store::graph_out_of_step`] tell rows another binary left behind on a
    /// file it modified.
    pub content_hash: Option<String>,
}

/// A row of `edges`: one occurrence of a relation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredSymbolEdge {
    /// `calls`/`instantiates`/`imports`/…
    pub kind: String,
    /// The symbol the occurrence is in (always one of `file`'s).
    pub src_id: u64,
    /// The destination's id, once the link pass resolves it.
    pub dst_id: Option<u64>,
    /// The destination as written, qualified as far as the file allows.
    pub dst_name: String,
    /// File of the occurrence.
    pub file: String,
    /// 1-based line.
    pub line: i32,
    /// `high`/`medium`/`low`, from the link pass.
    pub confidence: Option<String>,
    /// How the link pass resolved it.
    pub resolution: Option<String>,
    /// Defined outside the repository, with evidence.
    pub external: Option<bool>,
    /// The occurrence is in a test file.
    pub from_test: bool,
    /// `treesitter` today.
    pub edge_source: String,
    /// What the file said about a call's receiver (PLAN-009 TASK-005):
    /// the link pass re-resolves the row from it and `dst_name`.
    pub hint: Option<String>,
}

const SYMBOL_COLS: &[&str] = &[
    "repo",
    "branch",
    "id",
    "parent_id",
    "file",
    "kind",
    "name",
    "qualified",
    "container",
    "package",
    "signature",
    "start_line",
    "end_line",
    "start_byte",
    "end_byte",
    "exported",
    "rank",
    "in_degree",
    "is_test",
    "content_hash",
];

const EDGE_COLS: &[&str] = &[
    "repo",
    "branch",
    "kind",
    "src_id",
    "dst_id",
    "dst_name",
    "file",
    "line",
    "confidence",
    "resolution",
    "external",
    "from_test",
    "edge_source",
    "hint",
];

pub(crate) fn row_to_symbol(r: &duckdb::Row<'_>) -> duckdb::Result<StoredSymbol> {
    Ok(StoredSymbol {
        id: r.get(0)?,
        parent_id: r.get(1)?,
        file: r.get(2)?,
        kind: r.get(3)?,
        name: r.get(4)?,
        qualified: r.get(5)?,
        container: r.get(6)?,
        package: r.get(7)?,
        signature: r.get(8)?,
        start_line: r.get(9)?,
        end_line: r.get(10)?,
        start_byte: r.get::<_, Option<i64>>(11)?.unwrap_or(0),
        end_byte: r.get::<_, Option<i64>>(12)?.unwrap_or(0),
        exported: r.get(13)?,
        rank: r.get(14)?,
        in_degree: r.get(15)?,
        is_test: r.get::<_, Option<bool>>(16)?.unwrap_or(false),
        content_hash: r.get(17)?,
    })
}

pub(crate) const SYMBOL_SELECT: &str =
    "SELECT id, parent_id, file, kind, name, qualified, container, \
     package, signature, start_line, end_line, start_byte, end_byte, exported, rank, \
     in_degree, is_test, content_hash FROM symbols";

fn row_to_edge(r: &duckdb::Row<'_>) -> duckdb::Result<StoredSymbolEdge> {
    Ok(StoredSymbolEdge {
        kind: r.get(0)?,
        src_id: r.get(1)?,
        dst_id: r.get(2)?,
        dst_name: r.get(3)?,
        file: r.get(4)?,
        line: r.get::<_, Option<i32>>(5)?.unwrap_or(0),
        confidence: r.get(6)?,
        resolution: r.get(7)?,
        external: r.get(8)?,
        from_test: r.get::<_, Option<bool>>(9)?.unwrap_or(false),
        edge_source: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
        hint: r.get(11)?,
    })
}

/// An aggregated edge of a branch's PageRank graph
/// ([`Store::branch_rank_edges`]).
#[derive(Debug, Clone, PartialEq)]
pub struct RankEdge {
    /// Source symbol.
    pub src: u64,
    /// Destination symbol.
    pub dst: u64,
    /// Relation (`calls`, `instantiates`, …; never `contains`).
    pub kind: String,
    /// `high`, `medium`, `low`.
    pub confidence: Option<String>,
    /// The occurrences are in a test file.
    pub from_test: bool,
    /// Occurrences.
    pub n: u64,
}

const EDGE_SELECT: &str = "SELECT kind, src_id, dst_id, dst_name, file, line, confidence, \
     resolution, external, from_test, edge_source, hint FROM edges";

impl Store {
    /// Replace `file`'s rows in `symbols` with `symbols`.
    pub fn replace_file_symbols(
        &self,
        repo: &str,
        branch: &str,
        file: &str,
        symbols: &[StoredSymbol],
    ) -> Result<()> {
        // In a transaction (the caller's, or its own): the cached graph
        // check is dropped before the write and again at its commit.
        self.in_transaction(|| self.replace_file_symbols_tx(repo, branch, file, symbols))
    }

    fn replace_file_symbols_tx(
        &self,
        repo: &str,
        branch: &str,
        file: &str,
        symbols: &[StoredSymbol],
    ) -> Result<()> {
        self.forget_graph_step(Some(branch));
        self.w()?.execute(
            "DELETE FROM symbols WHERE repo = ? AND branch = ? AND file = ?",
            params![repo, branch, file],
        )?;
        if symbols.is_empty() {
            return Ok(());
        }
        let w = self.w()?;
        let mut app = w.appender_with_columns("symbols", SYMBOL_COLS)?;
        for s in symbols {
            app.append_row(params![
                repo,
                branch,
                s.id,
                s.parent_id,
                file,
                s.kind,
                s.name,
                s.qualified,
                s.container,
                s.package,
                s.signature,
                s.start_line,
                s.end_line,
                s.start_byte,
                s.end_byte,
                s.exported,
                s.rank,
                s.in_degree,
                s.is_test,
                s.content_hash,
            ])?;
        }
        app.flush()?;
        Ok(())
    }

    /// Replace `file`'s rows in `edges` with `edges`.
    pub fn replace_file_symbol_edges(
        &self,
        repo: &str,
        branch: &str,
        file: &str,
        edges: &[StoredSymbolEdge],
    ) -> Result<()> {
        self.w()?.execute(
            "DELETE FROM edges WHERE repo = ? AND branch = ? AND file = ?",
            params![repo, branch, file],
        )?;
        if edges.is_empty() {
            return Ok(());
        }
        let w = self.w()?;
        let mut app = w.appender_with_columns("edges", EDGE_COLS)?;
        for e in edges {
            app.append_row(params![
                repo,
                branch,
                e.kind,
                e.src_id,
                e.dst_id,
                e.dst_name,
                file,
                e.line,
                e.confidence,
                e.resolution,
                e.external,
                e.from_test,
                e.edge_source,
                e.hint,
            ])?;
        }
        app.flush()?;
        Ok(())
    }

    /// Replace both of `file`'s graph tables, as one transaction (or as part
    /// of the caller's).
    pub fn replace_file_graph(
        &self,
        repo: &str,
        branch: &str,
        file: &str,
        symbols: &[StoredSymbol],
        edges: &[StoredSymbolEdge],
    ) -> Result<()> {
        self.in_transaction(|| {
            self.replace_file_symbols(repo, branch, file, symbols)?;
            self.replace_file_symbol_edges(repo, branch, file, edges)
        })
    }

    /// Replace the resolved edges of several files — every kind but
    /// `contains`, which the link pass never touches (DD-6) — with theirs,
    /// in one transaction (the caller's, or its own): each file's rows are
    /// written whole. The link pass's writer. One `DELETE` for the batch:
    /// per file, its scan of the unindexed table was most of the pass.
    pub fn replace_files_linked_edges(
        &self,
        repo: &str,
        branch: &str,
        files: &[(&str, Vec<StoredSymbolEdge>)],
    ) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        debug_assert!(files
            .iter()
            .all(|(_, rows)| rows.iter().all(|e| e.kind != "contains")));
        self.in_transaction(|| {
            self.forget_graph_step(Some(branch));
            let marks = vec!["?"; files.len()].join(", ");
            let mut args: Vec<&dyn duckdb::ToSql> = vec![&repo, &branch];
            for (f, _) in files {
                args.push(f);
            }
            self.w()?.execute(
                &format!(
                    "DELETE FROM edges WHERE repo = ? AND branch = ? AND kind <> 'contains' \
                     AND file IN ({marks})"
                ),
                args.as_slice(),
            )?;
            let w = self.w()?;
            let mut app = w.appender_with_columns("edges", EDGE_COLS)?;
            for (file, rows) in files {
                for e in rows {
                    app.append_row(params![
                        repo,
                        branch,
                        e.kind,
                        e.src_id,
                        e.dst_id,
                        e.dst_name,
                        file,
                        e.line,
                        e.confidence,
                        e.resolution,
                        e.external,
                        e.from_test,
                        e.edge_source,
                        e.hint,
                    ])?;
                }
            }
            app.flush()?;
            Ok(())
        })
    }

    /// `calls` of a branch with neither a destination nor the `external`
    /// mark — undecided — over the `live_edges` view: a discarded call is
    /// not counted (TASK-008 and the metrics read the same way).
    pub fn branch_undecided_calls(&self, repo: &str, branch: &str) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM live_edges WHERE repo = ? AND branch = ? AND kind = 'calls'
               AND dst_id IS NULL AND NOT coalesce(external, false)",
            params![repo, branch],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// `calls` of a branch the link pass discarded (DD-7): kept as rows,
    /// out of `live_edges`.
    pub fn branch_discarded_calls(&self, repo: &str, branch: &str) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM edges WHERE repo = ? AND branch = ? AND kind = 'calls'
               AND resolution = 'discarded'",
            params![repo, branch],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Every symbol of a branch (the link pass's index).
    pub fn branch_symbols(&self, repo: &str, branch: &str) -> Result<Vec<StoredSymbol>> {
        let mut stmt = self.conn.prepare(&format!(
            "{SYMBOL_SELECT} WHERE repo = ? AND branch = ? ORDER BY file, start_byte, id"
        ))?;
        let rows = stmt.query_map(params![repo, branch], row_to_symbol)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Every edge of a branch but `contains`, by file and line (what the
    /// link pass resolves).
    pub fn branch_linkable_edges(&self, repo: &str, branch: &str) -> Result<Vec<StoredSymbolEdge>> {
        let mut stmt = self.conn.prepare(&format!(
            "{EDGE_SELECT} WHERE repo = ? AND branch = ? AND kind <> 'contains'
             ORDER BY file, line, src_id, dst_name"
        ))?;
        let rows = stmt.query_map(params![repo, branch], row_to_edge)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Forget `file`'s symbols and edges.
    pub fn delete_file_graph(&self, repo: &str, branch: &str, file: &str) -> Result<()> {
        self.in_transaction(|| {
            self.forget_graph_step(Some(branch));
            self.w()?.execute(
                "DELETE FROM symbols WHERE repo = ? AND branch = ? AND file = ?",
                params![repo, branch, file],
            )?;
            self.w()?.execute(
                "DELETE FROM edges WHERE repo = ? AND branch = ? AND file = ?",
                params![repo, branch, file],
            )?;
            Ok(())
        })
    }

    /// `file`'s symbols, in source order (the file symbol first).
    pub fn file_symbols(&self, repo: &str, branch: &str, file: &str) -> Result<Vec<StoredSymbol>> {
        let mut stmt = self.conn.prepare(&format!(
            "{SYMBOL_SELECT} WHERE repo = ? AND branch = ? AND file = ?
             ORDER BY start_byte, end_byte DESC, id"
        ))?;
        let rows = stmt.query_map(params![repo, branch, file], row_to_symbol)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The symbol with `id` on `branch`, if any.
    pub fn symbol_by_id(&self, repo: &str, branch: &str, id: u64) -> Result<Option<StoredSymbol>> {
        let mut stmt = self.conn.prepare(&format!(
            "{SYMBOL_SELECT} WHERE repo = ? AND branch = ? AND id = ? LIMIT 1"
        ))?;
        let mut rows = stmt.query_map(params![repo, branch, id], row_to_symbol)?;
        rows.next().transpose().map_err(Into::into)
    }

    /// `file`'s edges, by line.
    pub fn file_symbol_edges(
        &self,
        repo: &str,
        branch: &str,
        file: &str,
    ) -> Result<Vec<StoredSymbolEdge>> {
        let mut stmt = self.conn.prepare(&format!(
            "{EDGE_SELECT} WHERE repo = ? AND branch = ? AND file = ?
             ORDER BY line, src_id, dst_name"
        ))?;
        let rows = stmt.query_map(params![repo, branch, file], row_to_edge)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The edges a branch's PageRank runs on (PLAN-009 DD-12): `live_edges`
    /// with a destination — a call the link pass discarded never counts —
    /// but `contains`, aggregated per `(src, dst, kind, confidence,
    /// from_test)` with their number of occurrences, in that order.
    pub fn branch_rank_edges(&self, repo: &str, branch: &str) -> Result<Vec<RankEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT src_id, dst_id, kind, confidence, coalesce(from_test, false), count(*)
               FROM live_edges
              WHERE repo = ? AND branch = ? AND dst_id IS NOT NULL AND kind <> 'contains'
              GROUP BY ALL
              ORDER BY 1, 2, 3, 4 NULLS FIRST, 5",
        )?;
        let rows = stmt.query_map(params![repo, branch], |r| {
            Ok(RankEdge {
                src: r.get(0)?,
                dst: r.get(1)?,
                kind: r.get(2)?,
                confidence: r.get(3)?,
                from_test: r.get(4)?,
                n: r.get::<_, i64>(5)? as u64,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Write `symbols.rank` and `symbols.in_degree` of a branch, one
    /// `(id, rank, in_degree)` per symbol, in one transaction (a temporary
    /// table and one `UPDATE`). A symbol not in `ranks` keeps its values.
    pub fn write_branch_ranks(
        &self,
        repo: &str,
        branch: &str,
        ranks: &[(u64, f64, i32)],
    ) -> Result<()> {
        self.in_transaction(|| {
            let w = self.w()?;
            w.execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS rank_update
                   (id UBIGINT, rank DOUBLE, in_degree INTEGER);
                 DELETE FROM rank_update;",
            )?;
            let mut app = w.appender("rank_update")?;
            for &(id, rank, deg) in ranks {
                app.append_row(params![id, rank, deg])?;
            }
            app.flush()?;
            drop(app);
            w.execute(
                "UPDATE symbols SET rank = u.rank, in_degree = u.in_degree
                   FROM rank_update u
                  WHERE symbols.repo = ? AND symbols.branch = ? AND symbols.id = u.id",
                params![repo, branch],
            )?;
            w.execute_batch("DELETE FROM rank_update;")?;
            Ok(())
        })
    }

    /// The global rank of the innermost symbol containing each chunk (DD-4:
    /// by file and line range; never a `file` symbol), or `None`: a chunk of
    /// no symbol (a whole-file or doc chunk, a memory), or a branch whose link
    /// pass has not ranked it (an index from before TASK-010). `chunks` are
    /// `(repo, branch, file, start_line, end_line)`; the answer is in their
    /// order. One query per `(repo, branch)` among them.
    pub fn chunk_ranks(&self, chunks: &[(&str, &str, &str, i32, i32)]) -> Result<Vec<Option<f64>>> {
        let mut out = vec![None; chunks.len()];
        let mut groups: Vec<((&str, &str), Vec<usize>)> = Vec::new();
        for (i, c) in chunks.iter().enumerate() {
            if c.2.is_empty() {
                continue;
            }
            match groups.iter_mut().find(|(k, _)| *k == (c.0, c.1)) {
                Some((_, v)) => v.push(i),
                None => groups.push(((c.0, c.1), vec![i])),
            }
        }
        for ((repo, branch), idx) in groups {
            let values = vec!["(?, ?, ?, ?)"; idx.len()].join(", ");
            let mut args: Vec<duckdb::types::Value> = Vec::with_capacity(idx.len() * 4 + 2);
            for &i in &idx {
                let c = chunks[i];
                args.push((i as i64).into());
                args.push(c.2.to_string().into());
                args.push(c.3.into());
                args.push(c.4.into());
            }
            args.push(repo.to_string().into());
            args.push(branch.to_string().into());
            let sql = format!(
                "SELECT c.i, s.rank
                   FROM (VALUES {values}) AS c(i, file, lo, hi)
                   JOIN symbols s ON s.file = c.file
                    AND s.start_line <= c.lo AND s.end_line >= c.hi
                  WHERE s.repo = ? AND s.branch = ? AND s.kind <> 'file'
                QUALIFY row_number() OVER (
                    PARTITION BY c.i
                    ORDER BY s.end_line - s.start_line, s.start_line DESC, s.id) = 1"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(duckdb::params_from_iter(args), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<f64>>(1)?))
            })?;
            for r in rows {
                let (i, rank) = r?;
                out[i as usize] = rank;
            }
        }
        Ok(out)
    }

    /// `(symbols, edges)` rows of a branch.
    pub fn graph_row_counts(&self, repo: &str, branch: &str) -> Result<(u64, u64)> {
        let n = |table: &str| -> Result<u64> {
            let n: i64 = self.conn.query_row(
                &format!("SELECT count(*) FROM {table} WHERE repo = ? AND branch = ?"),
                params![repo, branch],
                |r| r.get(0),
            )?;
            Ok(n as u64)
        };
        Ok((n("symbols")?, n("edges")?))
    }

    /// Copy `file`'s symbols and edges from one branch to another.
    ///
    /// Symbols travel as they are: their ids carry no branch. Edges travel
    /// without their resolution (`dst_id`, `confidence`, `resolution`,
    /// `external`): what a call resolves to depends on the rest of the
    /// branch, so the destination branch's link pass decides it again. A
    /// `contains` edge keeps it: both ends are symbols of this file, the
    /// same on every branch.
    pub(crate) fn copy_file_graph(
        &self,
        repo: &str,
        from_branch: &str,
        to_branch: &str,
        file: &str,
    ) -> Result<()> {
        let symbols = self.file_symbols(repo, from_branch, file)?;
        let edges: Vec<StoredSymbolEdge> = self
            .file_symbol_edges(repo, from_branch, file)?
            .into_iter()
            .map(|e| {
                if e.kind == "contains" {
                    return e;
                }
                StoredSymbolEdge {
                    dst_id: None,
                    confidence: None,
                    resolution: None,
                    external: None,
                    ..e
                }
            })
            .collect();
        self.replace_file_graph(repo, to_branch, file, &symbols, &edges)
    }

    /// Forget `old`'s graph rows on a rename; the reindex of `new` writes
    /// them again.
    ///
    /// A symbol id carries its file (DD-3), so the renamed file's symbols are
    /// new symbols: nothing of the old rows is worth keeping under a new key,
    /// and the qualified names would have to change with the path anyway.
    /// Edges from other files already resolved into the old ids lose their
    /// destination (`dst_id` and its resolution), which the link pass
    /// re-resolves (DD-6), rather than pointing at symbols that no longer
    /// exist. Memories keep pointing by name.
    pub(crate) fn rename_file_graph(&self, repo: &str, branch: &str, old: &str) -> Result<()> {
        self.in_transaction(|| {
            self.w()?.execute(
                "UPDATE edges SET dst_id = NULL, confidence = NULL, resolution = NULL,
                     external = NULL
                 WHERE repo = ? AND branch = ? AND file <> ? AND dst_id IN
                     (SELECT id FROM symbols WHERE repo = ? AND branch = ? AND file = ?)",
                params![repo, branch, old, repo, branch, old],
            )?;
            self.delete_file_graph(repo, branch, old)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_core::symbol_id::{file_symbol_id, symbol_id, FILE_KIND};

    const DIM: usize = 3;

    fn sym(
        repo: &str,
        file: &str,
        kind: &str,
        qualified: &str,
        parent: Option<u64>,
    ) -> StoredSymbol {
        let id = if kind == FILE_KIND {
            file_symbol_id(repo, file)
        } else {
            symbol_id(repo, file, "callable", qualified, "")
        };
        StoredSymbol {
            id,
            parent_id: parent,
            file: file.into(),
            kind: kind.into(),
            name: qualified.rsplit('.').next().unwrap().into(),
            qualified: qualified.into(),
            signature: Some(format!("fn {qualified}()")),
            start_line: 1,
            end_line: 2,
            // The file symbol spans the file, so it sorts first.
            start_byte: if kind == FILE_KIND { 0 } else { 10 },
            end_byte: if kind == FILE_KIND { 100 } else { 20 },
            ..Default::default()
        }
    }

    fn file_graph(repo: &str, file: &str) -> (Vec<StoredSymbol>, Vec<StoredSymbolEdge>) {
        let f = sym(repo, file, FILE_KIND, file, None);
        let run = sym(repo, file, "function", "run", Some(f.id));
        let call = |line| StoredSymbolEdge {
            kind: "calls".into(),
            src_id: run.id,
            dst_name: "helper".into(),
            file: file.into(),
            line,
            edge_source: "treesitter".into(),
            ..Default::default()
        };
        let edges = vec![call(3), call(4)];
        (vec![f, run], edges)
    }

    /// A call the link pass discarded stays a row but is out of the live
    /// view and of the undecided count (follow-up 4 of TASK-005's review).
    #[test]
    fn discarded_calls_are_out_of_the_live_view() {
        let store = Store::open_in_memory(DIM).unwrap();
        let (syms, mut edges) = file_graph("r", "a.py");
        edges[1].resolution = Some("discarded".into());
        edges[1].confidence = Some("low".into());
        edges[1].external = Some(false);
        store
            .replace_file_graph("r", "main", "a.py", &syms, &edges)
            .unwrap();
        let live: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM live_edges WHERE repo = 'r' AND branch = 'main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live, 1);
        assert_eq!(store.branch_undecided_calls("r", "main").unwrap(), 1);
        assert_eq!(store.branch_discarded_calls("r", "main").unwrap(), 1);
    }

    /// The PageRank graph reads `live_edges` (TASK-010): a discarded call
    /// pointed at a symbol never counts; occurrences are aggregated with
    /// their number, `contains` and undecided rows are left out.
    #[test]
    fn the_rank_graph_reads_live_edges_only() {
        let store = Store::open_in_memory(DIM).unwrap();
        let f = sym("r", "a.py", FILE_KIND, "a.py", None);
        let run = sym("r", "a.py", "function", "run", Some(f.id));
        let mut helper = sym("r", "a.py", "function", "helper", Some(f.id));
        helper.start_line = 5;
        helper.end_line = 6;
        let call = |line, dst: Option<u64>| StoredSymbolEdge {
            kind: "calls".into(),
            src_id: run.id,
            dst_id: dst,
            dst_name: "helper".into(),
            file: "a.py".into(),
            line,
            confidence: Some("high".into()),
            resolution: Some("same_file".into()),
            external: Some(false),
            edge_source: "treesitter".into(),
            ..Default::default()
        };
        let mut discarded = call(5, Some(helper.id));
        discarded.resolution = Some("discarded".into());
        let mut contains = call(1, Some(run.id));
        contains.kind = "contains".into();
        contains.src_id = f.id;
        let edges = vec![
            call(3, Some(helper.id)),
            call(4, Some(helper.id)),
            discarded,
            call(6, None),
            contains,
        ];
        store
            .replace_file_graph(
                "r",
                "main",
                "a.py",
                &[f, run.clone(), helper.clone()],
                &edges,
            )
            .unwrap();
        let got = store.branch_rank_edges("r", "main").unwrap();
        assert_eq!(
            got,
            vec![RankEdge {
                src: run.id,
                dst: helper.id,
                kind: "calls".into(),
                confidence: Some("high".into()),
                from_test: false,
                n: 2,
            }]
        );
    }

    /// Ranks are written per symbol and read back per chunk: the innermost
    /// symbol containing the chunk's lines, never the file symbol; a chunk
    /// of no symbol, of another branch or before any rank is `None`.
    #[test]
    fn ranks_are_written_and_read_per_chunk() {
        let store = Store::open_in_memory(DIM).unwrap();
        let mut f = sym("r", "a.rs", FILE_KIND, "a.rs", None);
        f.end_line = 100;
        let mut outer = sym("r", "a.rs", "impl", "Outer", Some(f.id));
        (outer.start_line, outer.end_line) = (10, 50);
        let mut inner = sym("r", "a.rs", "method", "Outer.inner", Some(outer.id));
        (inner.start_line, inner.end_line) = (20, 30);
        store
            .replace_file_symbols(
                "r",
                "main",
                "a.rs",
                &[f.clone(), outer.clone(), inner.clone()],
            )
            .unwrap();
        let chunks = [
            ("r", "main", "a.rs", 22, 28),
            ("r", "main", "a.rs", 12, 18),
            ("r", "main", "a.rs", 1, 100),
            ("r", "dev", "a.rs", 22, 28),
            ("r", "main", "", 0, 0),
        ];
        assert_eq!(store.chunk_ranks(&chunks).unwrap(), vec![None; 5]);
        store
            .write_branch_ranks(
                "r",
                "main",
                &[(f.id, 0.5, 0), (outer.id, 0.2, 1), (inner.id, 0.3, 7)],
            )
            .unwrap();
        assert_eq!(
            store.chunk_ranks(&chunks).unwrap(),
            vec![Some(0.3), Some(0.2), None, None, None]
        );
        let back = store.symbol_by_id("r", "main", inner.id).unwrap().unwrap();
        assert_eq!((back.rank, back.in_degree), (Some(0.3), Some(7)));
    }

    /// One row per occurrence: the writer does not fold two calls to the same
    /// destination from the same symbol, which `graph_edges` does.
    #[test]
    fn repeated_occurrences_are_rows_of_their_own() {
        let store = Store::open_in_memory(DIM).unwrap();
        let (s, e) = file_graph("r", "a.py");
        store
            .replace_file_graph("r", "main", "a.py", &s, &e)
            .unwrap();
        let back = store.file_symbol_edges("r", "main", "a.py").unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back, e);
        assert_eq!(store.file_symbols("r", "main", "a.py").unwrap(), s);
        // UBIGINT round-trips the whole range, top bit included.
        let mut big = s.clone();
        big[1].id = u64::MAX - 1;
        store
            .replace_file_symbols("r", "main", "a.py", &big)
            .unwrap();
        assert!(store
            .symbol_by_id("r", "main", u64::MAX - 1)
            .unwrap()
            .is_some());
    }

    /// Replacing is replacing: a second write leaves only the second set, and
    /// other files and branches are untouched.
    #[test]
    fn a_rewrite_replaces_only_its_own_file_and_branch() {
        let store = Store::open_in_memory(DIM).unwrap();
        let (s, e) = file_graph("r", "a.py");
        let (s2, e2) = file_graph("r", "b.py");
        store
            .replace_file_graph("r", "main", "a.py", &s, &e)
            .unwrap();
        store
            .replace_file_graph("r", "main", "b.py", &s2, &e2)
            .unwrap();
        store
            .replace_file_graph("r", "dev", "a.py", &s, &e)
            .unwrap();
        store
            .replace_file_graph("r", "main", "a.py", &s[..1], &[])
            .unwrap();
        assert_eq!(store.file_symbols("r", "main", "a.py").unwrap().len(), 1);
        assert!(store
            .file_symbol_edges("r", "main", "a.py")
            .unwrap()
            .is_empty());
        assert_eq!(store.graph_row_counts("r", "main").unwrap(), (3, 2));
        assert_eq!(store.graph_row_counts("r", "dev").unwrap(), (2, 2));
    }

    /// The appender writes inside the caller's transaction: a file whose
    /// transaction fails leaves its old rows, never a half-written set.
    #[test]
    fn a_failed_transaction_keeps_the_old_rows() {
        let store = Store::open_in_memory(DIM).unwrap();
        let (s, e) = file_graph("r", "a.py");
        store
            .replace_file_graph("r", "main", "a.py", &s, &e)
            .unwrap();
        let err = store.in_transaction(|| -> Result<()> {
            store.replace_file_graph("r", "main", "a.py", &s[..1], &[])?;
            assert_eq!(store.file_symbols("r", "main", "a.py")?.len(), 1);
            Err(crate::StoreError::Decode("cut".into()))
        });
        assert!(err.is_err());
        assert_eq!(store.file_symbols("r", "main", "a.py").unwrap(), s);
        assert_eq!(store.file_symbol_edges("r", "main", "a.py").unwrap(), e);
    }

    #[test]
    fn deleting_a_file_leaves_no_orphans() {
        let store = Store::open_in_memory(DIM).unwrap();
        let (s, e) = file_graph("r", "a.py");
        store
            .replace_file_graph("r", "main", "a.py", &s, &e)
            .unwrap();
        store
            .replace_file_graph("r", "dev", "a.py", &s, &e)
            .unwrap();
        store.delete_file_graph("r", "main", "a.py").unwrap();
        assert_eq!(store.graph_row_counts("r", "main").unwrap(), (0, 0));
        assert_eq!(store.graph_row_counts("r", "dev").unwrap(), (2, 2));
    }

    /// A rename forgets the old path's graph rows, keeps nothing under the
    /// new path for the reindex to collide with, and unresolves the edges of
    /// other files that pointed into it (the link pass re-resolves them).
    #[test]
    fn a_rename_drops_the_old_rows_for_the_reindex() {
        let store = Store::open_in_memory(DIM).unwrap();
        let (s, e) = file_graph("r", "src/a.py");
        store
            .replace_file_graph("r", "main", "src/a.py", &s, &e)
            .unwrap();
        // b.py calls a.py's `run`, already resolved.
        let (bs, mut be) = file_graph("r", "b.py");
        be[0].dst_id = Some(s[1].id);
        be[0].confidence = Some("high".into());
        store
            .replace_file_graph("r", "main", "b.py", &bs, &be)
            .unwrap();

        store
            .rename_file("r", "main", "src/a.py", "lib/moved.py")
            .unwrap();

        for f in ["src/a.py", "lib/moved.py"] {
            assert!(
                store.file_symbols("r", "main", f).unwrap().is_empty(),
                "{f}"
            );
            assert!(
                store.file_symbol_edges("r", "main", f).unwrap().is_empty(),
                "{f}"
            );
        }
        let b = store.file_symbol_edges("r", "main", "b.py").unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].dst_id, None, "no edge points at a symbol that is gone");
        assert_eq!(b[0].confidence, None);
        assert_eq!(store.file_symbols("r", "main", "b.py").unwrap(), bs);

        // The reindex at the new path writes ids of their own, not the old ones.
        let (moved, moved_edges) = file_graph("r", "lib/moved.py");
        assert_ne!(moved[1].id, s[1].id);
        store
            .replace_file_graph("r", "main", "lib/moved.py", &moved, &moved_edges)
            .unwrap();
        assert_eq!(store.graph_row_counts("r", "main").unwrap(), (4, 4));
    }

    /// The tables are created on a database that predates them, and the DDL
    /// is folded into the file at once, not left in the WAL for a replay.
    #[test]
    fn an_existing_database_gains_the_tables_with_a_checkpoint() {
        let dir = std::env::temp_dir().join(format!("devctx_symbols_mig_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("index.duckdb");
        {
            let store = Store::open(&db, DIM).unwrap();
            store
                .conn
                .execute_batch("DROP TABLE symbols; DROP TABLE edges; CHECKPOINT;")
                .unwrap();
        }
        {
            let store = Store::open(&db, DIM).unwrap();
            let wal = dir.join("index.duckdb.wal");
            let wal_len = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
            assert_eq!(wal_len, 0, "the CREATE TABLE stayed in the WAL");
            let (s, e) = file_graph("r", "a.py");
            store
                .replace_file_graph("r", "main", "a.py", &s, &e)
                .unwrap();
            store.checkpoint();
        }
        let store = Store::open(&db, DIM).unwrap();
        assert_eq!(store.graph_row_counts("r", "main").unwrap(), (2, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Build an HNSW index for a test that is *about* one. VSS is expected
    /// (installed on first use; CI has the network), so its absence fails the
    /// test instead of passing it with nothing asserted. An environment that
    /// genuinely cannot load it opts out with `DEVCTX_TEST_ALLOW_NO_VSS=1`,
    /// and the skip is announced. Same contract as `require_fts`.
    fn require_vss(store: &Store, test: &str) -> bool {
        if store.enable_hnsw("cosine").unwrap() {
            return true;
        }
        assert!(
            std::env::var_os("DEVCTX_TEST_ALLOW_NO_VSS").is_some(),
            "{test}: the DuckDB VSS extension could not be loaded; set \
             DEVCTX_TEST_ALLOW_NO_VSS=1 to skip HNSW tests explicitly"
        );
        eprintln!("SKIPPED {test}: VSS unavailable (DEVCTX_TEST_ALLOW_NO_VSS)");
        false
    }

    /// A database with an HNSW index and one vector, and without the graph
    /// tables (as a 0.9.0 index is), folded into the file. `None` when the
    /// test was skipped for want of VSS.
    fn hnsw_db_without_graph_tables(test: &str) -> Option<std::path::PathBuf> {
        let dir = std::env::temp_dir().join(format!("devctx_{test}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("index.duckdb");
        let store = Store::open(&db, DIM).unwrap();
        if !require_vss(&store, test) {
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
        store
            .conn
            .execute_batch(
                "INSERT INTO vectors (id, vector, repo, branch, file)
                 VALUES ('p', [0.1, 0.2, 0.3]::FLOAT[3], 'r', 'main', 'a.rs');
                 DROP TABLE symbols; DROP TABLE edges; CHECKPOINT;",
            )
            .unwrap();
        Some(db)
    }

    /// The same upgrade over an index with an HNSW index (`storage.hnsw`, the
    /// default `init` writes). The checkpoint after the new tables has to bind
    /// the HNSW index; opened without VSS loaded first, DuckDB failed it with
    /// "unknown index type 'HNSW'" and invalidated the database — measured on
    /// a real 0.9.0 index before this test existed.
    #[test]
    fn an_existing_hnsw_database_gains_the_tables() {
        let test = "an_existing_hnsw_database_gains_the_tables";
        let Some(db) = hnsw_db_without_graph_tables(test) else {
            return;
        };
        {
            let store = Store::open(&db, DIM).expect("an HNSW index must open after an upgrade");
            assert_eq!(store.graph_row_counts("r", "main").unwrap(), (0, 0));
            let wal = db.with_extension("duckdb.wal");
            assert_eq!(std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0), 0);
            store.delete_by_file("r", "main", "a.rs").unwrap();
        }
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// The machine that cannot load VSS (offline, never installed): every
    /// checkpoint over the HNSW index would invalidate the database, so the
    /// schema upgrade and `Store`'s own checkpoints skip it, the connection
    /// stays usable, and the WAL waits for an open that can load VSS.
    #[test]
    fn an_hnsw_database_without_vss_skips_the_checkpoint() {
        let test = "an_hnsw_database_without_vss_skips_the_checkpoint";
        let Some(db) = hnsw_db_without_graph_tables(test) else {
            return;
        };
        {
            let config = duckdb::Config::default()
                .enable_autoload_extension(false)
                .unwrap();
            let conn = duckdb::Connection::open_with_flags(&db, config).unwrap();
            assert!(
                !crate::schema::checkpoint_is_safe(&conn),
                "VSS is not loaded here"
            );
            crate::schema::init_schema(&conn, DIM).expect("the upgrade must not checkpoint");
            let store = Store::over_connection(conn, DIM);
            assert!(matches!(
                store.try_checkpoint(),
                Err(crate::StoreError::CheckpointUnsafe)
            ));
            assert!(matches!(
                store.force_checkpoint(),
                Err(crate::StoreError::CheckpointUnsafe)
            ));
            store.checkpoint(); // best-effort: no panic, no invalidation
            let (s, e) = file_graph("r", "a.py");
            store
                .replace_file_graph("r", "main", "a.py", &s, &e)
                .expect("the database is still valid");
            assert_eq!(store.graph_row_counts("r", "main").unwrap(), (2, 2));
        }
        // An open that loads VSS replays the WAL and can checkpoint it.
        let store = Store::open(&db, DIM).unwrap();
        assert_eq!(store.graph_row_counts("r", "main").unwrap(), (2, 2));
        store.try_checkpoint().unwrap();
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }
}
