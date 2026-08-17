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
