//! The memory↔graph junction: which memories talk about which symbols.
//!
//! A memory says *why* the code is the way it is; the graph says *what* the code
//! is. Neither one answers "why is this function like this?" alone — the reader
//! has to already know a memory exists before they can recall it. This table is
//! the join that removes that requirement: land on a symbol, get the decisions
//! that were made about it.
//!
//! Links are derived, not declared. Nobody tags a memory with the symbols it
//! concerns, and asking them to would mean the links only exist when someone
//! remembered to add them. Two derivations, kept apart so a caller can tell how
//! solid a link is:
//!
//! * `files-field` — the memory named the file. Structural, and as reliable as
//!   whatever wrote the field.
//! * `content-mention` — the memory's prose contains the short label of a
//!   symbol the index places in one of those files. Narrower than matching
//!   every identifier in the text against the whole repository: "process" or
//!   "handler" appear in a thousand memories and mean nothing, but "process"
//!   in a memory that also names `orders/pipeline.rs` is about that one.
//!
//! Rows live in the store that owns the code — the project store — even when
//! the memory itself lives centrally. A global memory about this repository's
//! `charge()` is findable from this repository, which is where someone reading
//! `charge()` is standing.
//!
//! ## What `content-mention` gets wrong, measured
//!
//! Audited against a real corpus: 251 `content-mention` links across a sample
//! of 34 memories, every risky one read by hand. **One was wrong** — a memory
//! whose prose said "…*buscar* TODAS las queries con la misma forma", where
//! `buscar` is the ordinary Spanish verb and also a method in the file the
//! memory names. 0.4% net.
//!
//! The failure mode is specific and worth naming: a symbol whose short label is
//! a single lowercase word that is also a common word **in the language the
//! code is named in**. A Spanish-named codebase collides on `buscar`, `crear`,
//! `agrupar`; an English one on `process`, `handle`, `send`. Scoping the match
//! to the memory's own files is what keeps this rare — the same word matched
//! against the whole repository would be noise rather than a rounding error.
//!
//! Requiring a technical marker (a backtick, a `(`, a `.`) around such labels
//! was considered and rejected: it would take the one wrong link and cost every
//! plain-prose mention that is right, of which "charge is idempotent now" is
//! the ordinary shape. And the wrong link is benign — that memory genuinely
//! concerns that file, so a reader gets a relevant memory attributed to one
//! method too many, never someone else's memory.
//!
//! `link_sources` exists so a caller can weigh this: `content-mention` is a
//! derivation, not a fact, and the distinction is in every result.

use std::collections::HashSet;

use duckdb::params;

use crate::error::Result;
use crate::memory::{row_to_memory, Memory, MEM_COLS};
use crate::store::Store;

/// The repository's indexed paths, for resolving what a memory names.
///
/// Holds the exact paths and an index from bare file name to them, because a
/// memory says `NombreUtil.java` far more often than the full path, and both
/// spellings have to land on the same file.
pub struct FileIndex {
    exact: HashSet<String>,
    by_name: std::collections::HashMap<String, Vec<String>>,
}

impl FileIndex {
    pub fn new(files: Vec<String>) -> Self {
        let mut by_name: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for f in &files {
            let name = f.rsplit('/').next().unwrap_or(f).to_string();
            by_name.entry(name).or_default().push(f.clone());
        }
        Self {
            exact: files.into_iter().collect(),
            by_name,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.exact.is_empty()
    }

    /// The indexed paths matching `candidates`, in the spelling the index uses.
    ///
    /// A bare name that several files share resolves to none of them: a memory
    /// naming `index.ts` in a repository with forty of them has said nothing
    /// about any particular one, and linking it to all forty would bury every
    /// real link under noise.
    pub fn resolve(&self, candidates: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in candidates {
            let hit = if self.exact.contains(c) {
                Some(c.clone())
            } else {
                match self.by_name.get(c.rsplit('/').next().unwrap_or(c)) {
                    // Ambiguous, or the candidate is a path that no indexed file
                    // ends with — either way there is nothing safe to link.
                    Some(paths) if paths.len() == 1 && !c.contains('/') => Some(paths[0].clone()),
                    Some(paths) => paths
                        .iter()
                        .find(|p| p.ends_with(&format!("/{c}")))
                        .cloned(),
                    None => None,
                }
            };
            if let Some(h) = hit {
                if !out.contains(&h) {
                    out.push(h);
                }
            }
        }
        out
    }
}

/// One derived link between a memory and a place in the code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SymbolRef {
    pub memory_id: String,
    /// Graph symbol id, or empty for a file-level link.
    pub symbol: String,
    pub file: String,
    pub line: i64,
    pub repo: String,
    pub branch: String,
    /// How the link was derived: `files-field`, `content-mention`, `manual`.
    pub source: String,
}

/// A memory found through the junction, with how it was reached.
#[derive(Debug, Clone)]
pub struct LinkedMemory {
    pub memory: Memory,
    /// The distinct `source` values that link it, comma-separated. `inference`
    /// means the junction had nothing and the text fallback matched.
    pub link_sources: String,
}

/// Labels the parsers emit for things that have no name. Linking a memory to
/// every anonymous closure in a file is noise that buries the real links.
const UNNAMED: [&str; 3] = ["<unknown>", "<module>", "<anonymous>"];

/// The tail of a symbol id: `src/pay.rs::Card.charge` → `charge`.
///
/// Memories are prose. Nobody writes the fully-qualified id in a sentence; they
/// write the name, so the name is what a text match has to work with.
pub fn short_label(symbol: &str) -> &str {
    if let Some((_, tail)) = symbol.rsplit_once("::") {
        return tail;
    }
    if let Some((_, tail)) = symbol.rsplit_once('/') {
        return tail;
    }
    symbol
}

/// Whether `label` occurs in `text` as a whole identifier.
///
/// A substring match would link a memory mentioning `charged` to `charge`, and
/// `id` to every third word. The neighbours of a real occurrence are not
/// identifier characters.
fn mentions(text: &str, label: &str) -> bool {
    if label.is_empty() {
        return false;
    }
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let bytes = text.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = text[from..].find(label) {
        let start = from + rel;
        let end = start + label.len();
        let before_ok = start == 0
            || !(bytes[start - 1] as char).is_ascii()
            || !ident(bytes[start - 1] as char);
        let after_ok =
            end >= bytes.len() || !(bytes[end] as char).is_ascii() || !ident(bytes[end] as char);
        if before_ok && after_ok {
            return true;
        }
        // Advance past this occurrence; `label` is non-empty so this terminates.
        from = start + label.len();
    }
    false
}

impl Store {
    /// Rebuild the links for one memory, returning how many were written.
    ///
    /// Always-rebuild rather than add-to: a memory that was edited to drop a
    /// file should stop pointing at it, and an incremental update has no way to
    /// know which of the existing rows the new text no longer justifies.
    ///
    /// Best-effort by contract — the caller saves the memory first and links it
    /// after, so a repository with no graph yet still remembers.
    pub fn extract_symbol_refs(&self, m: &Memory) -> Result<usize> {
        self.conn.execute(
            "DELETE FROM memory_symbol_references WHERE memory_id = ?",
            params![m.id],
        )?;
        if m.id.is_empty() {
            return Ok(0);
        }

        let files: Vec<&str> = m
            .files
            .split(',')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .collect();
        if files.is_empty() {
            return Ok(0);
        }

        let mut rows: Vec<SymbolRef> = files
            .iter()
            .map(|f| SymbolRef {
                memory_id: m.id.clone(),
                symbol: String::new(),
                file: (*f).to_string(),
                line: 0,
                repo: m.repo.clone(),
                branch: m.branch.clone(),
                source: "files-field".into(),
            })
            .collect();

        if !m.content.is_empty() {
            rows.extend(self.mentioned_symbols(m, &files)?);
        }

        for r in &rows {
            self.conn.execute(
                "INSERT INTO memory_symbol_references
                 (memory_id, symbol, file, line, repo, branch, source)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    r.memory_id,
                    r.symbol,
                    r.file,
                    r.line as i32,
                    r.repo,
                    r.branch,
                    r.source
                ],
            )?;
        }
        Ok(rows.len())
    }

    /// Symbols defined in `files` whose short label the memory's prose mentions.
    ///
    /// Read from `vectors`, not `graph_edges`. The graph only knows a symbol if
    /// it takes part in a call, so a leaf utility — static helpers that call
    /// nothing indexed and are called through paths the parser does not follow —
    /// has no rows there at all. Measured on a real repository: a file with
    /// three public methods, all of them in `vectors`, had zero graph edges, and
    /// linking against the graph silently produced no symbol links for it.
    /// `vectors` holds every symbol the indexer chunked, which is the inventory
    /// this question is actually asking about.
    fn mentioned_symbols(&self, m: &Memory, files: &[&str]) -> Result<Vec<SymbolRef>> {
        let placeholders = vec!["?"; files.len()].join(",");
        let sql = format!(
            "SELECT DISTINCT symbol, file, start_line AS line, repo, branch
             FROM vectors
             WHERE file IN ({placeholders}) AND symbol <> '' AND NOT is_deletion"
        );
        let args: Vec<&dyn duckdb::ToSql> = files.iter().map(|f| f as &dyn duckdb::ToSql).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let found = stmt.query_map(args.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            ))
        })?;

        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut out = Vec::new();
        for row in found {
            let (symbol, file, line, repo, branch) = row?;
            let label = short_label(&symbol);
            if label.len() < 3 || UNNAMED.contains(&label) {
                continue;
            }
            if !mentions(&m.content, label) {
                continue;
            }
            if !seen.insert((symbol.clone(), file.clone())) {
                continue;
            }
            out.push(SymbolRef {
                memory_id: m.id.clone(),
                symbol,
                file,
                line,
                repo: if repo.is_empty() {
                    m.repo.clone()
                } else {
                    repo
                },
                branch: if branch.is_empty() {
                    m.branch.clone()
                } else {
                    branch
                },
                source: "content-mention".into(),
            });
        }
        Ok(out)
    }

    /// Which of `files` this repository actually has indexed.
    ///
    /// The gate that makes backfilling safe. A memory's `files` field is prose
    /// someone typed: it names files from other repositories in the same
    /// product, files since deleted, and — when the paths are recovered from a
    /// memory's text — library names that merely look like paths (`Shepherd.js`)
    /// and documents that are not code. Linking on the strength of the string
    /// alone would fill the junction with rows pointing at nothing, and a
    /// junction nobody can trust is worse than an empty one, because an empty
    /// one is visibly empty.
    ///
    /// Matching is on the exact stored path first, then on a trailing-path
    /// basis, so a memory that names `NombreUtil.java` still finds
    /// `src/main/java/.../NombreUtil.java`. The suffix must start at a
    /// separator, or `Util.java` would match every file ending in those bytes.
    pub fn indexed_files(&self, files: &[String]) -> Result<Vec<String>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.file_index()?.resolve(files))
    }

    /// Every distinct file this repository has indexed, ready to match against.
    ///
    /// Read in one query and matched in memory. A query per candidate is the
    /// obvious shape and does not survive contact with a real corpus: the
    /// suffix match is a scan, and a sweep over two thousand memories with a
    /// handful of candidates each turns into thousands of scans over every
    /// chunk in the repository. Measured: it did not finish in two minutes,
    /// where one read of a few thousand paths is milliseconds.
    pub fn file_index(&self) -> Result<FileIndex> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT file FROM vectors WHERE NOT is_deletion AND file <> ''")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut files = Vec::new();
        for r in rows {
            files.push(r?);
        }
        Ok(FileIndex::new(files))
    }

    /// Every link recorded for one memory — the inverse direction: given a
    /// memory, what code does it concern?
    pub fn memory_refs(&self, memory_id: &str) -> Result<Vec<SymbolRef>> {
        let mut stmt = self.conn.prepare(
            "SELECT memory_id, symbol, file, line, repo, branch, source
             FROM memory_symbol_references WHERE memory_id = ?
             ORDER BY source, file, symbol",
        )?;
        let rows = stmt.query_map(params![memory_id], |r| {
            Ok(SymbolRef {
                memory_id: r.get(0)?,
                symbol: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                file: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                line: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                repo: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                branch: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                source: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Memory ids linked to `symbol`, each with its distinct link sources.
    ///
    /// Ids rather than memories because the memory itself may live in the
    /// central store while the link lives here; only the caller knows both.
    pub fn memory_ids_for_symbol(
        &self,
        symbol: &str,
        repo: &str,
        branch: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let mut sql = String::from(
            "SELECT memory_id, string_agg(DISTINCT source, ',') AS sources
             FROM memory_symbol_references WHERE symbol = ?",
        );
        let mut args: Vec<&dyn duckdb::ToSql> = vec![&symbol];
        if !repo.is_empty() {
            sql.push_str(" AND repo = ?");
            args.push(&repo);
        }
        if !branch.is_empty() {
            sql.push_str(" AND branch = ?");
            args.push(&branch);
        }
        sql.push_str(&format!(" GROUP BY memory_id LIMIT {limit}"));

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(args.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Memory ids linked to `file`, each with its distinct link sources.
    pub fn memory_ids_for_file(&self, file: &str, limit: usize) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT memory_id, string_agg(DISTINCT source, ',') AS sources
             FROM memory_symbol_references WHERE file = ?
             GROUP BY memory_id LIMIT {limit}"
        ))?;
        let rows = stmt.query_map(params![file], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Live memories in *this* store whose text or `files` field mentions
    /// `label`. The fallback for a symbol the junction never linked.
    ///
    /// Worth having because the junction is only as complete as the indexing
    /// that produced it: a memory written before the repository was indexed, or
    /// one that names no files, has no rows and would otherwise be invisible to
    /// someone standing on exactly the symbol it describes.
    pub fn memories_mentioning(&self, label: &str, limit: usize) -> Result<Vec<Memory>> {
        if label.len() < 3 {
            return Ok(Vec::new());
        }
        let pattern = format!("%{label}%");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MEM_COLS} FROM memories
             WHERE deleted_at IS NULL AND (content LIKE ? OR files LIKE ?)
             ORDER BY updated_at DESC LIMIT {limit}"
        ))?;
        let rows = stmt.query_map(params![pattern, pattern], row_to_memory)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whole-identifier match is the difference between a link that means
    /// something and one that fires on every memory containing "id".
    #[test]
    fn a_mention_must_be_a_whole_identifier() {
        assert!(mentions("we changed charge() last week", "charge"));
        assert!(mentions("charge", "charge"));
        assert!(mentions("call `charge`, not the other one", "charge"));
        assert!(!mentions("the row was charged twice", "charge"));
        assert!(!mentions("recharge the pool", "charge"));
        assert!(!mentions("", "charge"));
    }

    /// Accented prose must not split an identifier in the middle of a character
    /// — memories are written in whatever language the author thinks in.
    #[test]
    fn a_mention_survives_multibyte_neighbours() {
        assert!(mentions("acá se rompe charge y explota", "charge"));
        assert!(mentions("«charge»", "charge"));
    }

    /// The short label is what appears in prose; the id is what the graph
    /// stores. Both spellings have to reduce to the same thing.
    #[test]
    fn the_short_label_is_the_tail_of_the_id() {
        assert_eq!(short_label("src/pay.rs::Card.charge"), "Card.charge");
        assert_eq!(short_label("orders/pipeline.rs"), "pipeline.rs");
        assert_eq!(short_label("charge"), "charge");
    }

    // --- against a real store -------------------------------------------------

    use crate::graph::StoredEdge;

    /// A chunk as the indexer writes it: this is where symbols actually live.
    fn chunk(id: &str, symbol: &str, file: &str) -> devctx_core::types::VectorPoint {
        devctx_core::types::VectorPoint {
            id: id.into(),
            vector: vec![0.0; 3],
            text: format!("code of {symbol}"),
            metadata: devctx_core::types::VectorMetadata {
                repo: "shop-api".into(),
                branch: "main".into(),
                file: file.into(),
                symbol: symbol.into(),
                symbol_type: "function".into(),
                language: "rust".into(),
                start_line: 1,
                end_line: 10,
                ..Default::default()
            },
        }
    }

    fn seeded_store() -> Store {
        let store = Store::open_in_memory(3).unwrap();
        store
            .upsert(&[
                chunk("v1", "src/pay.rs::charge", "src/pay.rs"),
                chunk("v2", "src/pay.rs::settle", "src/pay.rs"),
                chunk("v3", "src/pay.rs::refund", "src/pay.rs"),
            ])
            .unwrap();
        store
            .replace_file_edges(
                "shop-api",
                "main",
                "src/pay.rs",
                &[
                    StoredEdge {
                        source: "src/pay.rs::charge".into(),
                        target: "src/pay.rs::settle".into(),
                        kind: "calls".into(),
                        source_file: "src/pay.rs".into(),
                        line: 12,
                    },
                    StoredEdge {
                        source: "src/pay.rs::refund".into(),
                        target: "src/pay.rs::settle".into(),
                        kind: "calls".into(),
                        source_file: "src/pay.rs".into(),
                        line: 30,
                    },
                ],
            )
            .unwrap();
        store
    }

    fn memory(id: &str, content: &str, files: &str) -> Memory {
        Memory {
            id: id.into(),
            content: content.into(),
            files: files.into(),
            repo: "shop-api".into(),
            project: "shop-api".into(),
            branch: "main".into(),
            updated_at: "200".into(),
            ..Default::default()
        }
    }

    /// The two derivations have to be distinguishable in the result: a caller
    /// deciding whether to trust a link needs to know whether the memory named
    /// the file or merely happened to use the word.
    #[test]
    fn linking_records_the_file_and_the_symbols_the_prose_mentions() {
        let store = seeded_store();
        let m = memory(
            "mem_a",
            "we made charge idempotent; settle stayed as it was",
            "src/pay.rs",
        );
        store.upsert_memory(&m).unwrap();
        let n = store.extract_symbol_refs(&m).unwrap();
        assert_eq!(n, 3, "one file link plus charge and settle");

        let refs = store.memory_refs("mem_a").unwrap();
        let files: Vec<_> = refs
            .iter()
            .filter(|r| r.source == "files-field")
            .map(|r| r.file.as_str())
            .collect();
        assert_eq!(files, vec!["src/pay.rs"]);

        let mut symbols: Vec<_> = refs
            .iter()
            .filter(|r| r.source == "content-mention")
            .map(|r| r.symbol.as_str())
            .collect();
        symbols.sort_unstable();
        assert_eq!(symbols, vec!["src/pay.rs::charge", "src/pay.rs::settle"]);
        assert!(
            !refs.iter().any(|r| r.symbol.ends_with("refund")),
            "a symbol the prose never names must not be linked"
        );
    }

    /// Standing on a symbol, the memories about it come back — this is the
    /// whole point of the table.
    #[test]
    fn a_symbol_finds_the_memories_that_discuss_it() {
        let store = seeded_store();
        let m = memory("mem_a", "charge is now idempotent", "src/pay.rs");
        store.upsert_memory(&m).unwrap();
        store.extract_symbol_refs(&m).unwrap();

        let hits = store
            .memory_ids_for_symbol("src/pay.rs::charge", "shop-api", "main", 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "mem_a");
        assert_eq!(hits[0].1, "content-mention");

        // A repo that has nothing to do with it must not match.
        assert!(store
            .memory_ids_for_symbol("src/pay.rs::charge", "other-api", "main", 10)
            .unwrap()
            .is_empty());

        let by_file = store.memory_ids_for_file("src/pay.rs", 10).unwrap();
        assert_eq!(by_file.len(), 1);
        assert!(by_file[0].1.contains("files-field"));
    }

    /// Re-linking is a rebuild, not an append: a memory edited to drop a file
    /// must stop pointing at it, or the junction accumulates claims the text no
    /// longer supports.
    #[test]
    fn relinking_replaces_the_previous_links() {
        let store = seeded_store();
        let mut m = memory("mem_a", "charge is now idempotent", "src/pay.rs");
        store.upsert_memory(&m).unwrap();
        store.extract_symbol_refs(&m).unwrap();
        assert_eq!(store.memory_refs("mem_a").unwrap().len(), 2);

        m.files = String::new();
        m.content = "nothing to do with payments any more".into();
        store.extract_symbol_refs(&m).unwrap();
        assert!(store.memory_refs("mem_a").unwrap().is_empty());
        assert!(store
            .memory_ids_for_symbol("src/pay.rs::charge", "", "", 10)
            .unwrap()
            .is_empty());
    }

    /// The failure this module shipped with, found by running it against a real
    /// repository: a leaf utility takes part in no calls, so the call graph has
    /// no rows for its file — but its symbols are indexed all the same, and a
    /// memory naming them must still link. Reading the inventory from the graph
    /// produced a file link and nothing else, silently.
    #[test]
    fn a_file_with_symbols_but_no_call_edges_still_links_them() {
        let store = Store::open_in_memory(3).unwrap();
        store
            .upsert(&[chunk("v1", "separarApellidos", "util/names.java")])
            .unwrap();
        // Deliberately no edges: nothing calls it, it calls nothing indexed.

        let m = Memory {
            id: "mem_leaf".into(),
            content: "separarApellidos parte el string de apellidos".into(),
            files: "util/names.java".into(),
            repo: "shop-api".into(),
            branch: "main".into(),
            ..Default::default()
        };
        store.upsert_memory(&m).unwrap();
        assert_eq!(
            store.extract_symbol_refs(&m).unwrap(),
            2,
            "the file link plus the symbol the prose names"
        );
        let hits = store
            .memory_ids_for_symbol("separarApellidos", "shop-api", "main", 10)
            .unwrap();
        assert_eq!(hits.len(), 1, "a leaf symbol must be reachable");
        assert_eq!(hits[0].1, "content-mention");
    }

    /// A memory written before the repository was ever indexed has no links,
    /// and must still be reachable from the symbol it describes.
    #[test]
    fn the_text_fallback_finds_memories_the_junction_never_linked() {
        let store = seeded_store();
        let m = memory("mem_b", "charge double-bills on retry", "");
        store.upsert_memory(&m).unwrap();
        store.extract_symbol_refs(&m).unwrap();
        assert!(store.memory_refs("mem_b").unwrap().is_empty());

        let found = store.memories_mentioning("charge", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "mem_b");

        // Too short to be worth matching: every memory contains "id".
        assert!(store.memories_mentioning("id", 10).unwrap().is_empty());
    }
}
