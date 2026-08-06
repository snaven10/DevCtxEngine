//! `devctx-index` — the indexing pipeline for DevCtxEngine.
//!
//! Orchestrates git diff → parse → chunk → embed → store with deterministic
//! chunk ids and incremental `index_state`/`file_state`. Parseable code also
//! yields call-graph edges + routes; raw-text files (markdown/json/yaml/…) are
//! indexed as file-spanning chunks. See `docs/rust-rewrite-plan.md` §8 (F4).
//! Stale full-reindex cleanup is a follow-up.

pub mod error;
pub mod git;
pub mod id;
pub mod pipeline;

pub use error::{IndexError, Result};
pub use git::{Change, GitRepo, GitState};
pub use id::chunk_id;
pub use pipeline::{run, IndexRequest, IndexResult, ProgressSink};

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_core::SearchFilter;
    use devctx_embed::{EmbeddingProvider, Result as EmbedResult};
    use devctx_store::Store;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const DIM: usize = 8;

    /// Deterministic offline embedder: one DIM-length vector per text.
    struct FakeEmbedder;
    impl EmbeddingProvider for FakeEmbedder {
        fn embed(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    (0..DIM)
                        .map(|j| ((t.len() + j) % 10) as f32 / 10.0)
                        .collect()
                })
                .collect())
        }
        fn dimension(&self) -> usize {
            DIM
        }
        fn model_name(&self) -> &str {
            "fake"
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn commit_all(dir: &Path, msg: &str) {
        git(dir, &["add", "-A"]);
        git(
            dir,
            &[
                "-c",
                "user.email=t@t.io",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                msg,
            ],
        );
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn index(store: &Store, root: &Path) -> IndexResult {
        run(IndexRequest {
            store,
            embedder: &FakeEmbedder,
            repo_root: root,
            incremental: true,
            model_name: "minilm-l6",
            progress: None,
        })
        .unwrap()
    }

    #[test]
    fn indexes_raw_text_files() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_raw_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(
            &dir,
            "docs/notes.md",
            "# Notes\n\nWe use Postgres for storage.\n",
        );
        commit_all(&dir, "docs");

        let store = Store::open_in_memory(DIM).unwrap();
        let r = index(&store, &dir);
        assert_eq!(r.files_indexed, 1);
        assert!(r.chunks >= 1);

        let hits = store
            .search(&[0.1; DIM], &SearchFilter::default(), 10)
            .unwrap();
        assert!(
            hits.iter().any(|h| h.point.metadata.file == "docs/notes.md"
                && h.point.metadata.language == "markdown"
                && h.point.metadata.chunk_level == "file"),
            "no markdown chunk indexed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn indexes_kotlin_routes_via_raw_path() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_kt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(
            &dir,
            "UserController.kt",
            "@RestController\n@RequestMapping(\"/api\")\nclass UserController {\n    @GetMapping(\"/users\")\n    fun list(): List<User> { return emptyList() }\n}\n",
        );
        commit_all(&dir, "kt");

        let store = Store::open_in_memory(DIM).unwrap();
        let r = index(&store, &dir);
        assert_eq!(r.files_indexed, 1);

        let git_repo = GitRepo::open(&dir).unwrap();
        let branch = git_repo.state().branch;
        let routes = store
            .search_routes(&git_repo.short_name(), &branch, None, None)
            .unwrap();
        assert!(
            routes
                .iter()
                .any(|r| r.path == "/api/users" && r.handler_symbol == "UserController.list"),
            "kotlin route not extracted: {routes:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_reindex_prunes_vanished_files() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_prune_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(&dir, "a.rs", "pub fn a() -> i32 { 1 }\n");
        write(&dir, "b.rs", "pub fn b() -> i32 { 2 }\n");
        commit_all(&dir, "init");

        let store = Store::open_in_memory(DIM).unwrap();
        let r1 = index(&store, &dir);
        assert_eq!(r1.files_indexed, 2);

        // Remove b.rs and commit; then force a FULL reindex (git diff won't be used).
        std::fs::remove_file(dir.join("b.rs")).unwrap();
        commit_all(&dir, "rm b");
        let r2 = run(IndexRequest {
            store: &store,
            embedder: &FakeEmbedder,
            repo_root: &dir,
            incremental: false,
            model_name: "minilm-l6",
            progress: None,
        })
        .unwrap();

        assert!(r2.full_reindex);
        assert_eq!(r2.files_indexed, 1, "only a.rs remains");
        assert_eq!(r2.files_pruned, 1, "b.rs should be pruned");

        let b_hits = store
            .search(&[0.1; DIM], &SearchFilter::default(), 100)
            .unwrap()
            .into_iter()
            .filter(|h| h.point.metadata.file == "b.rs")
            .count();
        assert_eq!(b_hits, 0, "stale b.rs vectors remain");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn end_to_end_incremental_indexing() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_e2e_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);

        write(
            &dir,
            "src/lib.rs",
            "pub fn alpha() -> i32 { 1 }\npub fn beta() -> i32 { 2 }\n",
        );
        commit_all(&dir, "initial");

        let store = Store::open_in_memory(DIM).unwrap();

        // 1. First index: full reindex, one file, chunks stored.
        let r1 = index(&store, &dir);
        assert!(r1.full_reindex);
        assert_eq!(r1.files_indexed, 1);
        assert!(r1.chunks >= 1);
        assert_eq!(r1.symbols, 2);
        let stored = store.count(&SearchFilter::default()).unwrap();
        assert_eq!(stored as usize, r1.chunks);
        assert!(!r1.commit.is_empty());

        // 2. Re-run with no changes: incremental, nothing reprocessed.
        let r2 = index(&store, &dir);
        assert!(!r2.full_reindex);
        assert_eq!(r2.files_indexed, 0);
        assert_eq!(r2.files_deleted, 0);
        assert_eq!(store.count(&SearchFilter::default()).unwrap(), stored);

        // 3. Modify the file: incremental reindex of just that file.
        write(
            &dir,
            "src/lib.rs",
            "pub fn alpha() -> i32 { 1 }\npub fn beta() -> i32 { 2 }\npub fn gamma() -> i32 { 3 }\n",
        );
        commit_all(&dir, "add gamma");
        let r3 = index(&store, &dir);
        assert!(!r3.full_reindex);
        assert_eq!(r3.files_indexed, 1);
        assert_eq!(r3.symbols, 3);

        // 4. Add a new file.
        write(&dir, "src/extra.rs", "pub fn helper() -> i32 { 9 }\n");
        commit_all(&dir, "add extra");
        let r4 = index(&store, &dir);
        assert_eq!(r4.files_indexed, 1);
        let extra_hits = store
            .search(&[0.1; DIM], &SearchFilter::default(), 100)
            .unwrap()
            .into_iter()
            .filter(|h| h.point.metadata.file == "src/extra.rs")
            .count();
        assert!(extra_hits >= 1);

        // 5. Delete the new file: removed from the index.
        std::fs::remove_file(dir.join("src/extra.rs")).unwrap();
        commit_all(&dir, "rm extra");
        let r5 = index(&store, &dir);
        assert_eq!(r5.files_deleted, 1);
        let remaining = store
            .search(&[0.1; DIM], &SearchFilter::default(), 100)
            .unwrap()
            .into_iter()
            .filter(|h| h.point.metadata.file == "src/extra.rs")
            .count();
        assert_eq!(remaining, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
