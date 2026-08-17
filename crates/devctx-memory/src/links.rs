//! Reading and writing the memory↔graph junction across the two stores.
//!
//! The split this module exists to bridge: the call graph is per-repository and
//! lives in the project store, while a global or group memory lives in the
//! central one. A memory about this repository's `charge()` must be findable
//! from `charge()` no matter which of the two holds its text.
//!
//! So the junction row always goes in the **project** store — next to the graph
//! it points into — and carries only the memory's id. Resolving that id looks
//! locally first and falls back to the central store. The alternative, copying
//! memory text into every project that mentions it, would make an edit in one
//! place leave stale copies everywhere else.

use devctx_store::{short_label, LinkedMemory, Memory, Store};

/// Link a memory to the code it concerns, in the store that owns the graph.
///
/// Best-effort and infallible by design: linking is an enrichment, and a
/// repository that has not been indexed yet, or whose store is momentarily held
/// by something else, must not turn a successful `remember` into a failure.
/// Returns how many links were written — zero is a normal outcome.
pub fn link_memory(graph_store: &Store, m: &Memory) -> usize {
    graph_store.extract_symbol_refs(m).unwrap_or(0)
}

/// File paths a memory's prose names, for memories whose `files` field is empty.
///
/// Half a real corpus carries no `files` at all — the field was added after the
/// memories were, or whoever wrote them named the file in a sentence instead.
/// Those are the memories a symbol lookup most wants and least finds.
///
/// This is a *candidate* list and nothing more. It over-matches by design and
/// is safe only because the caller checks every candidate against the index
/// before linking: measured on a real corpus, the pattern that finds
/// `apps/registry/src/app/components/firmar-registro.ts` also finds
/// `Shepherd.js`, which is a library nobody indexed, and `CLAUDE.md`, which is
/// not code. The index is what tells them apart, never the pattern.
pub fn paths_in_text(text: &str) -> Vec<String> {
    const EXTS: [&str; 17] = [
        "java",
        "ts",
        "tsx",
        "js",
        "jsx",
        "html",
        "scss",
        "css",
        "py",
        "rs",
        "go",
        "sql",
        "yaml",
        "yml",
        "xml",
        "properties",
        "kt",
    ];
    // Split on whitespace and the punctuation that wraps a path in prose, but
    // never on `.` or `/`, which are part of the path itself.
    const BREAKS: &[char] = &[
        '`', '"', '\'', '(', ')', '[', ']', '{', '}', ',', ';', ':', '<', '>', '|', '*',
    ];
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || BREAKS.contains(&c)) {
        // A path at the end of a sentence keeps the full stop; a path in a list
        // keeps the comma. Neither is part of it.
        let tok = raw.trim_end_matches('.');
        let Some((stem, ext)) = tok.rsplit_once('.') else {
            continue;
        };
        // A bare `.ts`, or `src/.ts`, names nothing.
        if !EXTS.contains(&ext) || stem.is_empty() || stem.ends_with('/') {
            continue;
        }
        let tok = tok.to_string();
        if !out.contains(&tok) {
            out.push(tok);
        }
    }
    out
}

/// What a backfill pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Memories considered.
    pub examined: usize,
    /// Memories that named at least one file this repository has indexed.
    pub matched: usize,
    /// Memories that gained at least one symbol-level link.
    pub linked: usize,
    /// Junction rows written in total.
    pub rows: usize,
    /// Memories skipped for naming no file this repository has — because their
    /// `files` field was empty and, when the text pass ran, their prose named
    /// nothing indexed either.
    pub without_files: usize,
    /// Memories linked from paths recovered from their prose rather than from a
    /// `files` field. Counted apart because the derivation is weaker, and
    /// anyone deciding how far to trust the sweep needs to see the split.
    pub from_text: usize,
    /// Memories whose every named file is unknown to this repository — normally
    /// another repo's memory seen from here, not an error.
    pub not_in_repo: usize,
}

/// Link memories that were saved before the junction existed.
///
/// Migration and import write memories without linking them, and every memory
/// that predates the junction has no rows at all — so `memories_by_symbol`
/// answers for them by text inference or not at all, which reads as "nothing is
/// recorded about this code" when a great deal is.
///
/// Runs against one repository and links only the files that repository has
/// indexed, so calling it in each project distributes a shared memory's links
/// to whichever repositories its files actually live in. Re-running is free:
/// linking a memory rebuilds its rows rather than adding to them.
///
/// `dry_run` reports what would happen and writes nothing, because the first
/// question about a sweep over two thousand memories is what it is about to do.
pub fn backfill_links(
    graph_store: &Store,
    memories: &[Memory],
    dry_run: bool,
    from_text: bool,
) -> BackfillReport {
    let mut r = BackfillReport {
        examined: memories.len(),
        ..Default::default()
    };
    // Read once. Resolving each candidate with its own query is the obvious
    // shape and does not survive a real corpus — the suffix match is a scan, and
    // thousands of them over every chunk in the repository did not finish in two
    // minutes. One read of a few thousand paths is milliseconds.
    let Ok(index) = graph_store.file_index() else {
        return r;
    };
    for m in memories {
        let named: Vec<String> = m
            .files
            .split(',')
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty())
            .collect();

        // The `files` field first, always: it is what someone stated, and a
        // path recovered from prose can only ever be a guess about it. The text
        // pass runs only for memories that stated nothing — never to second-
        // guess a field whose files this repository simply does not have.
        let mut present = if named.is_empty() {
            Vec::new()
        } else {
            index.resolve(&named)
        };
        let mut recovered = false;
        if named.is_empty() && from_text {
            present = index.resolve(&paths_in_text(&m.content));
            recovered = !present.is_empty();
        }

        if present.is_empty() {
            if named.is_empty() {
                r.without_files += 1;
            } else {
                r.not_in_repo += 1;
            }
            continue;
        }
        r.matched += 1;
        if recovered {
            r.from_text += 1;
        }

        // Link the resolved paths, not what was typed: the field may hold a bare
        // file name, and the junction has to carry the path the index uses or
        // `memories_by_file` will not find it again.
        let mut resolved = m.clone();
        resolved.files = present.join(",");
        if dry_run {
            continue;
        }
        let n = link_memory(graph_store, &resolved);
        if n > 0 {
            r.rows += n;
            // One row per file is the floor; more means a symbol matched.
            if n > present.len() {
                r.linked += 1;
            }
        }
    }
    r
}

/// Memories that discuss `symbol`, most recently updated first.
///
/// Two stages, and the caller can tell them apart by `link_sources`:
///
/// 1. The junction — `files-field` / `content-mention`. Structural: something
///    connected this memory to this symbol at write time.
/// 2. A text search over memories that mention the symbol's short label, used
///    only when stage 1 found nothing, and marked `inference`. A memory written
///    before the repository was indexed has no junction rows, and dropping it
///    would mean the tool answers "nothing known about this" about a symbol
///    someone documented.
pub fn memories_by_symbol(
    graph_store: &Store,
    central: Option<&Store>,
    symbol: &str,
    repo: &str,
    branch: &str,
    limit: usize,
) -> Vec<LinkedMemory> {
    let linked = graph_store
        .memory_ids_for_symbol(symbol, repo, branch, limit)
        .unwrap_or_default();
    let out = resolve(graph_store, central, &linked);
    if !out.is_empty() {
        return out;
    }
    infer(graph_store, central, short_label(symbol), limit)
}

/// Memories that concern `file`. Same two stages as [`memories_by_symbol`].
pub fn memories_by_file(
    graph_store: &Store,
    central: Option<&Store>,
    file: &str,
    limit: usize,
) -> Vec<LinkedMemory> {
    let linked = graph_store
        .memory_ids_for_file(file, limit)
        .unwrap_or_default();
    let out = resolve(graph_store, central, &linked);
    if !out.is_empty() {
        return out;
    }
    infer(graph_store, central, file, limit)
}

/// Turn `(memory_id, sources)` pairs into memories, looking in the project
/// store first and the central one after.
fn resolve(
    graph_store: &Store,
    central: Option<&Store>,
    linked: &[(String, String)],
) -> Vec<LinkedMemory> {
    let mut out = Vec::new();
    for (id, sources) in linked {
        let found = graph_store
            .get_memory(id)
            .ok()
            .flatten()
            .or_else(|| central.and_then(|c| c.get_memory(id).ok().flatten()));
        // A junction row whose memory is gone is not an error: `memory-forget`
        // removes the memory, and the row is derived data that the next link
        // pass would drop anyway.
        if let Some(m) = found.filter(|m| m.deleted_at.is_none()) {
            out.push(LinkedMemory {
                memory: m,
                link_sources: sources.clone(),
            });
        }
    }
    out.sort_by(|a, b| b.memory.updated_at.cmp(&a.memory.updated_at));
    out
}

/// The text fallback, over both stores, de-duplicated by memory id.
fn infer(
    graph_store: &Store,
    central: Option<&Store>,
    label: &str,
    limit: usize,
) -> Vec<LinkedMemory> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for store in std::iter::once(graph_store).chain(central) {
        for m in store.memories_mentioning(label, limit).unwrap_or_default() {
            if seen.insert(m.id.clone()) {
                out.push(LinkedMemory {
                    memory: m,
                    link_sources: "inference".into(),
                });
            }
        }
    }
    out.sort_by(|a, b| b.memory.updated_at.cmp(&a.memory.updated_at));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_store::StoredEdge;

    fn memory(id: &str, content: &str, files: &str, updated: &str) -> Memory {
        Memory {
            id: id.into(),
            content: content.into(),
            files: files.into(),
            repo: "shop-api".into(),
            project: "shop-api".into(),
            branch: "main".into(),
            updated_at: updated.into(),
            ..Default::default()
        }
    }

    /// A chunk as the indexer writes it. Symbols live here, not in the graph:
    /// the graph only knows the ones that take part in a call.
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

    fn project_store() -> Store {
        let s = Store::open_in_memory(3).unwrap();
        s.upsert(&[
            chunk("v1", "src/pay.rs::charge", "src/pay.rs"),
            chunk("v2", "src/pay.rs::settle", "src/pay.rs"),
        ])
        .unwrap();
        s.replace_file_edges(
            "shop-api",
            "main",
            "src/pay.rs",
            &[StoredEdge {
                source: "src/pay.rs::charge".into(),
                target: "src/pay.rs::settle".into(),
                kind: "calls".into(),
                source_file: "src/pay.rs".into(),
                line: 12,
            }],
        )
        .unwrap();
        s
    }

    /// The case the whole module exists for: the memory lives centrally, the
    /// graph lives in the project, and standing on the symbol still finds it.
    #[test]
    fn a_central_memory_is_found_through_the_project_graph() {
        let project = project_store();
        let central = Store::open_in_memory(3).unwrap();

        let m = memory("mem_g", "charge is idempotent now", "src/pay.rs", "300");
        central.upsert_memory(&m).unwrap();
        // The link is written next to the graph, not next to the memory.
        assert_eq!(link_memory(&project, &m), 2);
        assert!(project.get_memory("mem_g").unwrap().is_none());

        let hits = memories_by_symbol(
            &project,
            Some(&central),
            "src/pay.rs::charge",
            "shop-api",
            "main",
            10,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].memory.id, "mem_g");
        assert_eq!(hits[0].link_sources, "content-mention");
    }

    /// Without the central store in hand the same link resolves to nothing
    /// rather than to a half-filled memory.
    #[test]
    fn an_unresolvable_link_is_dropped_not_faked() {
        let project = project_store();
        let central = Store::open_in_memory(3).unwrap();
        let m = memory("mem_g", "charge is idempotent now", "src/pay.rs", "300");
        central.upsert_memory(&m).unwrap();
        link_memory(&project, &m);

        let hits = memories_by_symbol(&project, None, "src/pay.rs::charge", "", "", 10);
        assert!(hits.is_empty(), "no text to show, so no result");
    }

    /// A memory nobody linked is still reachable, and says so.
    #[test]
    fn the_fallback_is_marked_so_it_can_be_told_apart() {
        let project = project_store();
        let m = memory("mem_old", "charge double-bills on retry", "", "100");
        project.upsert_memory(&m).unwrap();

        let hits = memories_by_symbol(&project, None, "src/pay.rs::charge", "", "", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].memory.id, "mem_old");
        assert_eq!(hits[0].link_sources, "inference");
    }

    /// Structural links win outright: when the junction has an answer the
    /// fallback never runs, so a weak text match cannot dilute a strong result.
    #[test]
    fn a_structural_link_suppresses_the_text_fallback() {
        let project = project_store();
        let linked = memory("mem_new", "charge is idempotent now", "src/pay.rs", "300");
        let loose = memory("mem_loose", "charge came up in review", "", "400");
        project.upsert_memory(&linked).unwrap();
        project.upsert_memory(&loose).unwrap();
        link_memory(&project, &linked);

        let hits = memories_by_symbol(&project, None, "src/pay.rs::charge", "", "", 10);
        let ids: Vec<_> = hits.iter().map(|h| h.memory.id.as_str()).collect();
        assert_eq!(ids, vec!["mem_new"]);
    }

    // --- backfill ---------------------------------------------------------

    /// The case the backfill exists for: memories that carry `files` but no
    /// junction rows, because they were migrated before the junction existed.
    #[test]
    fn backfilling_links_memories_that_were_never_linked() {
        let project = project_store();
        let m = memory("mem_old", "charge is idempotent now", "src/pay.rs", "100");
        project.upsert_memory(&m).unwrap();
        assert!(project.memory_refs("mem_old").unwrap().is_empty());

        let r = backfill_links(&project, &[m], false, false);
        assert_eq!(r.matched, 1);
        assert_eq!(r.linked, 1, "a symbol link, not just the file");
        assert_eq!(r.rows, 2);

        let hits = memories_by_symbol(&project, None, "src/pay.rs::charge", "", "", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].link_sources, "content-mention");
    }

    /// A file this repository does not have must not be linked here. Shared
    /// memories name files from every repository of a product, and a junction
    /// full of rows pointing at nothing is worse than an empty one.
    #[test]
    fn a_file_this_repo_does_not_index_is_not_linked() {
        let project = project_store();
        let m = memory(
            "mem_other",
            "the web app does it here",
            "web/src/app.ts",
            "100",
        );
        project.upsert_memory(&m).unwrap();

        let r = backfill_links(&project, &[m], false, false);
        assert_eq!(r.not_in_repo, 1);
        assert_eq!(r.matched, 0);
        assert!(project.memory_refs("mem_other").unwrap().is_empty());
    }

    /// A bare file name has to resolve to the indexed path, or the link is
    /// written against a path `memories_by_file` will never be asked about.
    #[test]
    fn a_bare_file_name_resolves_to_the_indexed_path() {
        let project = project_store();
        let m = memory("mem_bare", "charge is idempotent now", "pay.rs", "100");
        project.upsert_memory(&m).unwrap();

        assert_eq!(backfill_links(&project, &[m], false, false).matched, 1);
        let refs = project.memory_refs("mem_bare").unwrap();
        assert!(
            refs.iter().all(|r| r.file == "src/pay.rs"),
            "links must carry the indexed path, got {:?}",
            refs.iter().map(|r| &r.file).collect::<Vec<_>>()
        );
    }

    /// A dry run answers the question and changes nothing.
    #[test]
    fn a_dry_run_reports_without_writing() {
        let project = project_store();
        let m = memory("mem_old", "charge is idempotent now", "src/pay.rs", "100");
        project.upsert_memory(&m).unwrap();

        let r = backfill_links(&project, std::slice::from_ref(&m), true, false);
        assert_eq!(r.matched, 1);
        assert_eq!(r.rows, 0);
        assert!(project.memory_refs("mem_old").unwrap().is_empty());
    }

    /// Memories with nothing to go on are counted, not silently dropped: the
    /// gap between "examined" and "matched" is the whole reason to run a
    /// second, heuristic pass, and it has to be visible.
    #[test]
    fn memories_without_files_are_counted() {
        let project = project_store();
        let m = memory("mem_nofiles", "charge broke once", "", "100");
        project.upsert_memory(&m).unwrap();

        let r = backfill_links(&project, &[m], false, false);
        assert_eq!(r.examined, 1);
        assert_eq!(r.without_files, 1);
        assert_eq!(r.matched, 0);
    }

    /// The pattern must find a real path in prose and reject the things that
    /// merely look like one. Both examples are taken from a real corpus.
    #[test]
    fn paths_are_recovered_from_prose_and_junk_comes_with_them() {
        let found = paths_in_text(
            "el control vive en apps/registry/src/app/firmar-registro.ts y usamos              Shepherd.js para el tour; ver CLAUDE.md. Roto en NombreUtil.java.",
        );
        assert!(found.contains(&"apps/registry/src/app/firmar-registro.ts".to_string()));
        assert!(found.contains(&"NombreUtil.java".to_string()), "{found:?}");
        // Deliberately still here: the pattern cannot tell a library from a
        // file, so it must not try. The index rejects it, and the next test
        // proves that it does.
        assert!(found.contains(&"Shepherd.js".to_string()));
        // Not code, and not in the extension list.
        assert!(!found.iter().any(|f| f.ends_with(".md")));
    }

    /// Trailing punctuation is prose, not path.
    #[test]
    fn a_path_at_the_end_of_a_sentence_keeps_its_path() {
        assert_eq!(paths_in_text("roto en pay.rs."), vec!["pay.rs"]);
        assert_eq!(paths_in_text("ver `src/pay.rs`,"), vec!["src/pay.rs"]);
        assert!(paths_in_text("solo .rs suelto").is_empty());
        assert!(paths_in_text("y src/.rs tampoco").is_empty());
    }

    /// The whole safety argument for the text pass: a candidate the index does
    /// not know is never linked, however much it looks like a path.
    #[test]
    fn a_recovered_path_the_index_does_not_know_is_not_linked() {
        let project = project_store();
        let m = memory(
            "mem_lib",
            "usamos Shepherd.js para el tour de la pantalla",
            "",
            "100",
        );
        project.upsert_memory(&m).unwrap();

        let r = backfill_links(&project, std::slice::from_ref(&m), false, true);
        assert_eq!(r.matched, 0);
        assert_eq!(r.without_files, 1);
        assert!(project.memory_refs("mem_lib").unwrap().is_empty());
    }

    /// A memory with no `files` but a real path in its prose gets linked, and
    /// is reported as text-derived so the weaker provenance stays visible.
    #[test]
    fn a_memory_with_no_files_is_linked_from_its_prose() {
        let project = project_store();
        let m = memory(
            "mem_prose",
            "el bug estaba en src/pay.rs — charge cobraba dos veces",
            "",
            "100",
        );
        project.upsert_memory(&m).unwrap();

        let r = backfill_links(&project, std::slice::from_ref(&m), false, true);
        assert_eq!(r.matched, 1);
        assert_eq!(r.from_text, 1, "text-derived links must be counted apart");

        let hits = memories_by_symbol(&project, None, "src/pay.rs::charge", "", "", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].link_sources, "content-mention");
    }

    /// Off by default: the same memory yields nothing without the flag, so the
    /// weaker pass is never something a caller gets by accident.
    #[test]
    fn the_text_pass_is_opt_in() {
        let project = project_store();
        let m = memory("mem_prose", "el bug estaba en src/pay.rs", "", "100");
        project.upsert_memory(&m).unwrap();

        let r = backfill_links(&project, std::slice::from_ref(&m), false, false);
        assert_eq!(r.matched, 0);
        assert_eq!(r.from_text, 0);
    }

    /// A stated `files` field is never second-guessed: when it names only files
    /// this repository lacks, that is the answer, and the prose does not get a
    /// vote. Otherwise a memory about another repo would be dragged in here by
    /// a path that happens to appear in its text.
    #[test]
    fn a_stated_files_field_is_not_second_guessed_by_the_prose() {
        let project = project_store();
        let m = memory(
            "mem_other",
            "el equivalente acá es src/pay.rs pero el cambio fue en el web",
            "web/src/app.ts",
            "100",
        );
        project.upsert_memory(&m).unwrap();

        let r = backfill_links(&project, std::slice::from_ref(&m), false, true);
        assert_eq!(r.not_in_repo, 1);
        assert_eq!(r.matched, 0);
    }

    /// A memory linked to a file, found by that file.
    #[test]
    fn a_file_finds_the_memories_that_name_it() {
        let project = project_store();
        let m = memory("mem_f", "rewrote the retry loop", "src/pay.rs", "300");
        project.upsert_memory(&m).unwrap();
        link_memory(&project, &m);

        let hits = memories_by_file(&project, None, "src/pay.rs", 10);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].link_sources.contains("files-field"));
    }
}
