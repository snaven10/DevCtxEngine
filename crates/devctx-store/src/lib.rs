//! `devctx-store` — DuckDB-backed persistence for DevCtxEngine.
//!
//! F1: vector store (brute-force cosine) + full schema. See
//! `docs/rust-rewrite-plan.md` §4. The HNSW/VSS index is deferred to F8;
//! `array_cosine_distance` over `FLOAT[N]` columns is a core DuckDB function
//! and needs no extension.

mod error;
mod graph;
mod memory;
mod projects;
mod routes;
mod schema;
mod state;
mod store;

pub use error::{Result, StoreError};
pub use graph::{GraphEdge, ImpactResult, Reference, StoredEdge};
pub use memory::{Memory, MemoryStats};
pub use projects::{ProjectIndexStats, ProjectRecord};
pub use routes::StoredRoute;
pub use schema::init_schema;
pub use state::{FileState, IndexRecord};
pub use store::Store;

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

    #[test]
    fn hnsw_index_supports_search_insert_delete() {
        let store = Store::open_in_memory(DIM).unwrap();
        if !store.enable_hnsw().unwrap() {
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
        assert!(store.enable_hnsw().unwrap());
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
        if !store.rebuild_fts().unwrap() {
            return; // FTS extension unavailable.
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
