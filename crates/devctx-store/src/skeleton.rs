//! What the `skeleton` tool reads besides a file's symbols (PLAN-009
//! TASK-014, DD-17): the first imports of a file and their count, whether a
//! file has rows at all, and the indexed paths nearest to one that has none.

use duckdb::params;

use crate::error::Result;
use crate::store::Store;

/// The imports of one file, as the skeleton summarizes them: how many
/// distinct targets, and the first ones by line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileImports {
    /// Distinct import targets of the file.
    pub total: usize,
    /// The first `limit` of them, by first line and then by text.
    pub first: Vec<String>,
}

impl Store {
    /// `file`'s distinct import targets on `branch`: their number and the
    /// first `limit` (by the line of their first occurrence, then by text;
    /// `limit` > 0). Only those `limit` are read.
    pub fn file_imports(
        &self,
        repo: &str,
        branch: &str,
        file: &str,
        limit: usize,
    ) -> Result<FileImports> {
        // One statement: the window counts the groups before `LIMIT` cuts
        // them, so the count needs no second scan and the rest no transfer.
        let mut stmt = self.conn.prepare(
            "SELECT dst_name, count(*) OVER () AS total
               FROM live_edges
              WHERE repo = ? AND branch = ? AND file = ? AND kind = 'imports'
              GROUP BY dst_name
              ORDER BY min(line), dst_name
              LIMIT ?",
        )?;
        let rows = stmt.query_map(params![repo, branch, file, limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut out = FileImports::default();
        for row in rows {
            let (name, total) = row?;
            out.total = total as usize;
            out.first.push(name);
        }
        Ok(out)
    }

    /// Whether `file` has any row in `vectors` on `branch` (it was indexed,
    /// even if its language yields no symbols).
    pub fn has_file_rows(&self, repo: &str, branch: &str, file: &str) -> Result<bool> {
        let found: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM vectors
                             WHERE repo = ? AND branch = ? AND file = ? AND NOT is_deletion)",
            params![repo, branch, file],
            |r| r.get(0),
        )?;
        Ok(found)
    }

    /// Up to `n` files of `branch` that have a symbol graph, nearest to
    /// `asked` (a path that has none): see [`rank_file_suggestions`].
    pub fn file_suggestions(
        &self,
        repo: &str,
        branch: &str,
        asked: &str,
        n: usize,
    ) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT file FROM symbols
              WHERE repo = ? AND branch = ? AND kind = 'file' AND parent_id IS NULL",
        )?;
        let rows = stmt.query_map(params![repo, branch], |r| r.get::<_, String>(0))?;
        let files = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rank_file_suggestions(asked, &files, n))
    }
}

/// Rank indexed paths against one that matched nothing. Pure.
///
/// By class, then by edit distance between the whole paths, then by path:
/// (0) the path ends with what was asked (`core/Foo.java`, any case); (1)
/// the same file name elsewhere; (2) the same name with another extension,
/// or one name inside the other (`Foo.java`, `FooTest.java`); (3) a file
/// name a few edits away (a third of its length, at least 2); (4) another
/// file of the same directory. Anything else is not near.
pub fn rank_file_suggestions(asked: &str, files: &[String], n: usize) -> Vec<String> {
    let asked = asked.trim_start_matches("./").to_lowercase();
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let stem = |b: &str| b.split_once('.').map_or(b, |(s, _)| s).to_string();
    let dir = |p: &str| p.rsplit_once('/').map_or("", |(d, _)| d).to_string();
    let (abase, astem, adir) = (base(&asked), stem(&base(&asked)), dir(&asked));
    if abase.is_empty() {
        return Vec::new();
    }
    let threshold = (abase.chars().count() / 3).max(2);
    let mut scored: Vec<(u8, usize, &String)> = Vec::new();
    for f in files {
        let lower = f.to_lowercase();
        if lower == asked {
            continue;
        }
        let (fbase, fstem) = (base(&lower), stem(&base(&lower)));
        let class = if lower.ends_with(&format!("/{asked}")) {
            0
        } else if fbase == abase {
            1
        } else if !astem.is_empty()
            && !fstem.is_empty()
            && (fstem == astem || fstem.contains(&astem) || astem.contains(&fstem))
        {
            2
        } else if crate::store::edit_distance(&fbase, &abase) <= threshold {
            3
        } else if !adir.is_empty() && dir(&lower) == adir {
            4
        } else {
            continue;
        };
        scored.push((class, crate::store::edit_distance(&lower, &asked), f));
    }
    scored.sort();
    scored.dedup_by(|a, b| a.2 == b.2);
    scored
        .into_iter()
        .take(n)
        .map(|(_, _, f)| f.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StoredSymbol, StoredSymbolEdge};

    fn files(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A path written as its tail first, then the same file name elsewhere,
    /// then a near miss of the name; never an unrelated file. Deterministic.
    #[test]
    fn suggestions_rank_tail_then_name_then_near_miss() {
        let all = files(&[
            "src/web/OrderResource.java",
            "src/core/OrderService.java",
            "src/core/OrderServise.java",
            "test/core/OrderService.java",
            "src/core/Unrelated.java",
            "lib/z/OrderService.ts",
        ]);
        let r = rank_file_suggestions("core/OrderService.java", &all, 5);
        assert_eq!(r[0], "src/core/OrderService.java", "{r:?}");
        assert_eq!(r[1], "test/core/OrderService.java", "{r:?}");
        assert!(
            r.contains(&"src/core/OrderServise.java".to_string()),
            "{r:?}"
        );
        assert!(r.contains(&"lib/z/OrderService.ts".to_string()), "{r:?}");
        assert!(!r.contains(&"src/core/Unrelated.java".to_string()), "{r:?}");
        // A wrong directory with the right name: the name wins.
        let r = rank_file_suggestions("src/wrong/OrderService.java", &all, 2);
        assert_eq!(
            r,
            ["src/core/OrderService.java", "test/core/OrderService.java"],
            "{r:?}"
        );
        // Any input order gives the same answer.
        let mut rev = all.clone();
        rev.reverse();
        assert_eq!(
            rank_file_suggestions("core/OrderService.java", &rev, 5),
            rank_file_suggestions("core/OrderService.java", &all, 5)
        );
        assert!(rank_file_suggestions("zzzz.qq", &all, 5).is_empty());
        // A name nothing resembles: the files of its directory, nearest
        // path first.
        let r = rank_file_suggestions("src/core/Draft.java", &all, 5);
        assert_eq!(
            r,
            [
                "src/core/Unrelated.java",
                "src/core/OrderService.java",
                "src/core/OrderServise.java"
            ],
            "{r:?}"
        );
    }

    fn file_sym(file: &str) -> StoredSymbol {
        StoredSymbol {
            id: devctx_core::symbol_id::file_symbol_id("r", file),
            file: file.into(),
            kind: "file".into(),
            name: file.into(),
            qualified: file.into(),
            start_line: 1,
            end_line: 40,
            end_byte: 400,
            ..Default::default()
        }
    }

    /// The count is of distinct targets; only `limit` come back, in order
    /// of first line (then text); another file's or branch's never.
    #[test]
    fn imports_are_counted_and_only_the_first_read() {
        let store = Store::open_in_memory(3).unwrap();
        let f = file_sym("a.ts");
        let imp = |target: &str, line: i32| StoredSymbolEdge {
            kind: "imports".into(),
            src_id: f.id,
            dst_name: target.into(),
            file: "a.ts".into(),
            line,
            edge_source: "treesitter".into(),
            ..Default::default()
        };
        let edges = vec![
            imp("./z", 1),
            imp("./b", 2),
            imp("./a", 2),
            imp("./z", 3),
            imp("./c", 4),
            StoredSymbolEdge {
                kind: "calls".into(),
                ..imp("./calls", 5)
            },
        ];
        store
            .replace_file_graph("r", "main", "a.ts", std::slice::from_ref(&f), &edges)
            .unwrap();
        let g = file_sym("b.ts");
        store
            .replace_file_graph(
                "r",
                "main",
                "b.ts",
                std::slice::from_ref(&g),
                &[StoredSymbolEdge {
                    src_id: g.id,
                    file: "b.ts".into(),
                    ..imp("./other", 1)
                }],
            )
            .unwrap();
        let got = store.file_imports("r", "main", "a.ts", 3).unwrap();
        assert_eq!(got.total, 4);
        assert_eq!(got.first, ["./z", "./a", "./b"]);
        assert_eq!(
            store.file_imports("r", "dev", "a.ts", 3).unwrap(),
            FileImports::default()
        );
        assert_eq!(
            store.file_suggestions("r", "main", "c.ts", 5).unwrap(),
            ["a.ts", "b.ts"]
        );
    }
}
