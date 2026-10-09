//! `devctx-store` — DuckDB-backed persistence for DevCtxEngine.
//!
//! Vector store + full schema. See `docs/architecture-spec.md` §4.
//! `array_cosine_distance` over `FLOAT[N]` is a core DuckDB function and needs
//! no extension, so brute-force search always works; the HNSW (VSS) and BM25
//! (FTS) indexes are opt-in accelerators, dropped before a bulk load and
//! rebuilt after.

mod error;
mod graph;
mod lookup;
mod memory;
mod memory_refs;
mod projects;
mod routes;
mod schema;
mod state;
mod store;
mod symbols;

pub use error::{Result, StoreError};
pub use graph::{GraphEdge, ImpactResult, Reference, StoredEdge};
pub use lookup::{
    sym_hex, ExternalSites, MinConfidence, SymbolDefinition, SymbolReference, ViewEdge,
};
pub use memory::{Memory, MemoryStats};
pub use memory_refs::{short_label, FileIndex, LinkedMemory, SymbolRef};
pub use projects::{ProjectIndexStats, ProjectRecord};
pub use routes::StoredRoute;
pub use schema::init_schema;
pub use state::{CopySetup, FileState, IndexRecord, EMBED_FP_META_KEY, EXTRACTOR_META_KEY};
pub use store::{normalize_metric, MemoryReport, Store};
pub use symbols::{StoredSymbol, StoredSymbolEdge};

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_core::types::{SearchFilter, VectorMetadata, VectorPoint};

    const DIM: usize = 3;

    fn point(id: &str, vec: [f32; DIM], file: &str, language: &str) -> VectorPoint {
        VectorPoint {
            id: id.to_string(),
            vector: vec.to_vec(),
            text: format!("text of {id}"),
            metadata: VectorMetadata {
                repo: "demo".into(),
                branch: "main".into(),
                file: file.into(),
                symbol: id.into(),
                symbol_type: "function".into(),
                language: language.into(),
                start_line: 1,
                end_line: 10,
                chunk_level: "function".into(),
                content_hash: "hash".into(),
                indexed_at: "2026-08-05T00:00:00Z".into(),
                ..Default::default()
            },
        }
    }

    /// Build the BM25 index for a test that is *about* keyword search.
    ///
    /// The FTS extension is expected (bundled DuckDB installs it on first
    /// use, and CI has the network for it), so its absence fails the test
    /// rather than letting it pass with nothing asserted. An environment that
    /// genuinely cannot load it opts out with `DEVCTX_TEST_ALLOW_NO_FTS=1`,
    /// and the skip is then announced instead of silent.
    fn require_fts(store: &Store, test: &str) -> bool {
        if store.rebuild_fts().unwrap() {
            return true;
        }
        assert!(
            std::env::var_os("DEVCTX_TEST_ALLOW_NO_FTS").is_some(),
            "{test}: the DuckDB FTS extension could not be loaded; set \
             DEVCTX_TEST_ALLOW_NO_FTS=1 to skip keyword tests explicitly"
        );
        eprintln!("SKIPPED {test}: FTS unavailable (DEVCTX_TEST_ALLOW_NO_FTS)");
        false
    }

    fn seeded() -> Store {
        let store = Store::open_in_memory(DIM).unwrap();
        store
            .upsert(&[
                point("a", [1.0, 0.0, 0.0], "a.rs", "rust"),
                point("b", [0.0, 1.0, 0.0], "b.py", "python"),
                point("c", [0.9, 0.1, 0.0], "c.rs", "rust"),
            ])
            .unwrap();
        store
    }

    #[test]
    fn round_trip_preserves_point() {
        let store = seeded();
        let mut all = store.scroll_all("demo", "main").unwrap();
        all.sort_by(|x, y| x.id.cmp(&y.id));
        assert_eq!(all.len(), 3);
        let a = &all[0];
        assert_eq!(a.id, "a");
        assert_eq!(a.vector, vec![1.0, 0.0, 0.0]);
        assert_eq!(a.text, "text of a");
        assert_eq!(a.metadata.language, "rust");
        assert_eq!(a.metadata.start_line, 1);
    }

    #[test]
    fn search_ranks_by_cosine() {
        let store = seeded();
        let hits = store
            .search(&[1.0, 0.0, 0.0], &SearchFilter::default(), 2)
            .unwrap();
        let ids: Vec<_> = hits.iter().map(|h| h.point.id.clone()).collect();
        assert_eq!(ids, vec!["a", "c"]);
        assert!(hits[0].score > 0.99, "score was {}", hits[0].score);
        assert!(hits[0].score >= hits[1].score);
    }

    /// Export carries embeddings so an import between matching machines does not
    /// pay to recompute them — measured at 46 minutes for 2090 memories.
    #[test]
    fn a_vector_can_be_read_back_by_id() {
        let store = Store::open_in_memory(DIM).unwrap();
        store
            .upsert(&[point("mem_a", [0.5, 0.25, 0.125], "a.rs", "rust")])
            .unwrap();

        let got = store.vector_by_id("mem_a").unwrap().expect("stored");
        assert_eq!(got, vec![0.5, 0.25, 0.125]);
        assert!(store.vector_by_id("mem_missing").unwrap().is_none());
    }

    #[test]
    fn memory_report_reads_settings_and_counts_without_failing() {
        let store = Store::open_in_memory(DIM).unwrap();
        store
            .upsert(&[point("mem_a", [0.5, 0.25, 0.125], "a.rs", "rust")])
            .unwrap();
        let r = store.memory_report();
        assert!(r.memory_limit.is_some(), "{r:?}");
        assert!(r.threads.is_some(), "{r:?}");
        assert_eq!(r.vectors, Some(1));
        assert_eq!(r.dimension, DIM);
        assert_eq!(r.hnsw_estimated_bytes(), Some(DIM as u64 * 4));
        assert!(!r.hnsw_present);
    }

    #[test]
    fn hnsw_index_supports_search_insert_delete() {
        let store = Store::open_in_memory(DIM).unwrap();
        if !store.enable_hnsw("cosine").unwrap() {
            return; // VSS extension unavailable (e.g. offline): brute-force path.
        }
        store
            .upsert(&[
                point("a", [1.0, 0.0, 0.0], "a.rs", "rust"),
                point("c", [0.9, 0.1, 0.0], "c.rs", "rust"),
            ])
            .unwrap();
        let hits = store
            .search(&[1.0, 0.0, 0.0], &SearchFilter::default(), 2)
            .unwrap();
        assert_eq!(hits[0].point.id, "a");

        // upsert (delete + insert) and delete_by_file with the index present.
        store
            .upsert(&[point("a", [0.0, 0.0, 1.0], "a.rs", "rust")])
            .unwrap();
        store.delete_by_file("demo", "main", "c.rs").unwrap();
        assert_eq!(store.count(&SearchFilter::default()).unwrap(), 1);

        // enable_hnsw is idempotent.
        assert!(store.enable_hnsw("cosine").unwrap());
        assert_eq!(store.hnsw_metric().as_deref(), Some("cosine"));

        // Switching metric replaces the index rather than leaving both, and the
        // search follows it: a distance function that disagrees with the index
        // would quietly stop using it.
        assert!(store.enable_hnsw("ip").unwrap());
        assert_eq!(store.hnsw_metric().as_deref(), Some("ip"));
        let hits = store
            .search(&[0.0, 0.0, 1.0], &SearchFilter::default(), 2)
            .unwrap();
        assert_eq!(hits[0].point.id, "a", "unit vectors rank the same under ip");
        assert!(hits[0].score > 0.99, "score was {}", hits[0].score);
    }

    #[test]
    fn keyword_search_ranks_by_bm25() {
        let store = Store::open_in_memory(DIM).unwrap();
        let mk = |id: &str, text: &str| VectorPoint {
            id: id.into(),
            vector: vec![0.0; DIM],
            text: text.into(),
            metadata: VectorMetadata {
                repo: "demo".into(),
                branch: "main".into(),
                language: "rust".into(),
                ..Default::default()
            },
        };
        store
            .upsert(&[
                mk("a", "connect to the postgres database"),
                mk("b", "greet a user by name"),
                mk("c", "database connection pool setup"),
            ])
            .unwrap();
        if !require_fts(&store, "keyword_search") {
            return;
        }
        let hits = store
            .keyword_search("database connection", &SearchFilter::default(), 5)
            .unwrap();
        let ids: Vec<_> = hits.iter().map(|h| h.point.id.clone()).collect();
        assert!(ids.contains(&"a".to_string()));
        assert!(ids.contains(&"c".to_string()));
        assert!(!ids.contains(&"b".to_string()), "non-matching doc returned");

        // Filter is honored.
        let filtered = store
            .keyword_search(
                "database",
                &SearchFilter {
                    languages: vec!["python".into()],
                    ..Default::default()
                },
                5,
            )
            .unwrap();
        assert!(filtered.is_empty(), "language filter not applied");
    }

    #[test]
    fn search_filters_by_language() {
        let store = seeded();
        let filter = SearchFilter {
            languages: vec!["python".into()],
            ..Default::default()
        };
        let hits = store.search(&[1.0, 0.0, 0.0], &filter, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].point.id, "b");
    }

    #[test]
    fn upsert_replaces_existing_id() {
        let store = seeded();
        store
            .upsert(&[point("a", [0.0, 0.0, 1.0], "a.rs", "rust")])
            .unwrap();
        assert_eq!(store.count(&SearchFilter::default()).unwrap(), 3);
        let a = store
            .scroll_all("demo", "main")
            .unwrap()
            .into_iter()
            .find(|p| p.id == "a")
            .unwrap();
        assert_eq!(a.vector, vec![0.0, 0.0, 1.0]);
    }

    #[test]
    fn delete_by_file_removes_only_that_file() {
        let store = seeded();
        let n = store.delete_by_file("demo", "main", "a.rs").unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.count(&SearchFilter::default()).unwrap(), 2);
    }

    #[test]
    fn delete_memory_vectors_sweeps_chunk_ids() {
        let store = Store::open_in_memory(DIM).unwrap();
        store
            .upsert(&[
                point("mem_abc", [1.0, 0.0, 0.0], "", "memory"),
                point("mem_abc_c1", [0.0, 1.0, 0.0], "", "memory"),
                point("mem_abc_c2", [0.0, 0.0, 1.0], "", "memory"),
                point("mem_other", [1.0, 1.0, 0.0], "", "memory"),
            ])
            .unwrap();
        let n = store.delete_memory_vectors("mem_abc").unwrap();
        assert_eq!(n, 3);
        assert_eq!(store.count(&SearchFilter::default()).unwrap(), 1);
    }

    #[test]
    fn rename_file_updates_rows() {
        let store = seeded();
        let n = store
            .rename_file("demo", "main", "a.rs", "renamed.rs")
            .unwrap();
        assert_eq!(n, 1);
        let filter = SearchFilter::default();
        let hit = store
            .search(&[1.0, 0.0, 0.0], &filter, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(hit.point.metadata.file, "renamed.rs");
    }

    #[test]
    fn dimension_mismatch_is_rejected() {
        let store = Store::open_in_memory(DIM).unwrap();
        let mut p = point("x", [1.0, 0.0, 0.0], "x.rs", "rust");
        p.vector = vec![1.0, 0.0]; // wrong length
        let err = store.upsert(&[p]).unwrap_err();
        assert!(matches!(err, StoreError::DimensionMismatch { .. }));
    }

    #[test]
    fn persists_to_a_file() {
        let path = std::env::temp_dir().join("devctx_store_f1_persist.duckdb");
        let _ = std::fs::remove_file(&path);
        {
            let store = Store::open(&path, DIM).unwrap();
            store
                .upsert(&[point("a", [1.0, 0.0, 0.0], "a.rs", "rust")])
                .unwrap();
        }
        {
            let store = Store::open(&path, DIM).unwrap();
            assert_eq!(store.count(&SearchFilter::default()).unwrap(), 1);
        }
        let _ = std::fs::remove_file(&path);
    }
}
