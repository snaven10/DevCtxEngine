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
            paths: None,
            exclude: &[],
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
            paths: None,
            exclude: &[],
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

    /// Index exactly these paths, whatever git thinks changed.
    fn index_paths(store: &Store, root: &Path, paths: &[String]) -> IndexResult {
        run(IndexRequest {
            store,
            embedder: &FakeEmbedder,
            repo_root: root,
            incremental: true,
            model_name: "minilm-l6",
            progress: None,
            paths: Some(paths),
            exclude: &[],
        })
        .unwrap()
    }

    /// A file watcher fires on *save*, when HEAD has not moved — so the
    /// commit-diff path sees nothing. An explicit path list is what lets the
    /// pipeline index work that has not been committed yet.
    #[test]
    fn explicit_paths_index_uncommitted_work() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_paths_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(&dir, "src/lib.rs", "pub fn alpha() -> i32 { 1 }\n");
        commit_all(&dir, "initial");

        let store = Store::open_in_memory(DIM).unwrap();
        assert_eq!(index(&store, &dir).files_indexed, 1);

        let repo_path = std::fs::canonicalize(&dir)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let branch = GitRepo::open(&dir).unwrap().state().branch;
        let committed = store
            .get_index_record(&repo_path, &branch)
            .unwrap()
            .unwrap()
            .last_commit;

        // Edit without committing: the commit diff is empty, so a normal
        // incremental run does nothing at all.
        write(
            &dir,
            "src/lib.rs",
            "pub fn alpha() -> i32 { 1 }\npub fn beta() -> i32 { 2 }\n",
        );
        assert_eq!(
            index(&store, &dir).files_indexed,
            0,
            "a commit diff cannot see a save"
        );

        // Naming the file directly does index it.
        let paths = vec!["src/lib.rs".to_string()];
        let targeted = index_paths(&store, &dir, &paths);
        assert_eq!(targeted.files_indexed, 1);
        assert!(!targeted.full_reindex);
        assert_eq!(targeted.symbols, 2, "the uncommitted symbol was picked up");

        // Re-running with unchanged content is a no-op, via the hash check.
        let again = index_paths(&store, &dir, &paths);
        assert_eq!(again.files_indexed, 0);
        assert_eq!(again.files_skipped, 1);

        // The recorded commit must not have advanced to HEAD: this run covered
        // uncommitted work, so the next incremental still needs that diff.
        let after = store
            .get_index_record(&repo_path, &branch)
            .unwrap()
            .unwrap();
        assert_eq!(after.last_commit, committed);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A named path that has vanished from the work tree is a deletion.
    #[test]
    fn an_explicit_path_that_is_gone_is_removed_from_the_index() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_pathdel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(&dir, "src/lib.rs", "pub fn alpha() -> i32 { 1 }\n");
        commit_all(&dir, "initial");

        let store = Store::open_in_memory(DIM).unwrap();
        index(&store, &dir);
        assert!(store.count(&SearchFilter::default()).unwrap() > 0);

        std::fs::remove_file(dir.join("src/lib.rs")).unwrap();
        let res = index_paths(&store, &dir, &["src/lib.rs".to_string()]);
        assert_eq!(res.files_deleted, 1);
        assert_eq!(store.count(&SearchFilter::default()).unwrap(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A full reindex must not throw away work that is written but not
    /// committed: that is exactly the code you are most likely to ask about,
    /// and the watcher had already put it in the index.
    #[test]
    fn a_full_reindex_keeps_uncommitted_files() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_untracked_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(&dir, ".gitignore", "target/\n*.log\n");
        write(&dir, "src/lib.rs", "pub fn tracked() -> i32 { 1 }\n");
        commit_all(&dir, "initial");

        let store = Store::open_in_memory(DIM).unwrap();
        index(&store, &dir);

        // A brand-new file, never `git add`ed, plus ignored noise beside it.
        write(&dir, "src/draft.rs", "pub fn drafted() -> i32 { 2 }\n");
        write(&dir, "build.log", "noise\n");
        write(&dir, "target/gen.rs", "pub fn generated() -> i32 { 3 }\n");

        let full = run(IndexRequest {
            store: &store,
            embedder: &FakeEmbedder,
            repo_root: &dir,
            incremental: false,
            model_name: "minilm-l6",
            progress: None,
            paths: None,
            exclude: &[],
        })
        .unwrap();
        assert!(full.full_reindex);
        assert_eq!(full.files_pruned, 0, "nothing should have been pruned");

        let indexed = indexed_files(&store);
        assert!(indexed.contains(&"src/lib.rs".to_string()));
        assert!(
            indexed.contains(&"src/draft.rs".to_string()),
            "uncommitted work must survive a full reindex: {indexed:?}"
        );
        assert!(
            !indexed.iter().any(|f| f.starts_with("target/")),
            "git-ignored files must stay out: {indexed:?}"
        );
        assert!(!indexed.contains(&"build.log".to_string()));
        assert!(
            !indexed.iter().any(|f| f.starts_with(".devctx/")),
            "the index must not swallow its own state: {indexed:?}"
        );
        assert!(
            !indexed.iter().any(|f| f.starts_with(".fastembed_cache/")),
            "nor the downloaded model cache: {indexed:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An incremental run picks up a new file too, so `index` behaves the same
    /// way whether or not it happens to be doing a full pass.
    #[test]
    fn an_incremental_run_picks_up_a_new_untracked_file() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_incr_new_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(&dir, "src/lib.rs", "pub fn tracked() -> i32 { 1 }\n");
        commit_all(&dir, "initial");

        let store = Store::open_in_memory(DIM).unwrap();
        index(&store, &dir);

        write(&dir, "src/draft.rs", "pub fn drafted() -> i32 { 2 }\n");
        let r = index(&store, &dir);
        assert!(!r.full_reindex);
        assert_eq!(r.files_indexed, 1);
        assert!(indexed_files(&store).contains(&"src/draft.rs".to_string()));

        // Running again changes nothing: the content hash still matches.
        let again = index(&store, &dir);
        assert_eq!(again.files_indexed, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every distinct file currently present in the index.
    fn indexed_files(store: &Store) -> Vec<String> {
        let hits = store
            .search(&[0.1; DIM], &SearchFilter::default(), 100)
            .unwrap();
        let mut files: Vec<String> = hits
            .into_iter()
            .map(|h| h.point.metadata.file)
            .filter(|f| !f.is_empty())
            .collect();
        files.sort();
        files.dedup();
        files
    }

    fn index_excluding(store: &Store, root: &Path, exclude: &[String]) -> IndexResult {
        run(IndexRequest {
            store,
            embedder: &FakeEmbedder,
            repo_root: root,
            incremental: true,
            model_name: "minilm-l6",
            progress: None,
            paths: None,
            exclude,
        })
        .unwrap()
    }

    /// `indexing.exclude` keeps tracked-but-uninteresting code out, using the
    /// same pattern syntax as `.gitignore`.
    #[test]
    fn configured_excludes_keep_files_out_of_the_index() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_exclude_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(&dir, "src/lib.rs", "pub fn kept() -> i32 { 1 }\n");
        write(
            &dir,
            "src/api.generated.rs",
            "pub fn generated() -> i32 { 2 }\n",
        );
        write(
            &dir,
            "vendor/dep/mod.rs",
            "pub fn vendored() -> i32 { 3 }\n",
        );
        write(&dir, "docs/notes.md", "# Notes\n\nsome prose\n");
        commit_all(&dir, "initial");

        // A directory rule covers everything beneath it; a `*` rule matches at
        // any depth. Both are gitignore semantics, not literal globs.
        let exclude = vec![
            "vendor/".to_string(),
            "*.generated.rs".to_string(),
            "docs/notes.md".to_string(),
        ];

        let store = Store::open_in_memory(DIM).unwrap();
        index_excluding(&store, &dir, &exclude);

        let indexed = indexed_files(&store);
        assert_eq!(
            indexed,
            vec!["src/lib.rs".to_string()],
            "only the non-excluded file should be indexed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Adding an exclude retroactively removes what it now covers, so the config
    /// is the whole truth rather than only applying to future files.
    #[test]
    fn a_new_exclude_prunes_what_it_now_covers() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_exclude2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(&dir, "src/lib.rs", "pub fn kept() -> i32 { 1 }\n");
        write(&dir, "vendor/dep.rs", "pub fn vendored() -> i32 { 2 }\n");
        commit_all(&dir, "initial");

        let store = Store::open_in_memory(DIM).unwrap();
        index_excluding(&store, &dir, &[]);
        assert_eq!(indexed_files(&store).len(), 2);

        let res = run(IndexRequest {
            store: &store,
            embedder: &FakeEmbedder,
            repo_root: &dir,
            incremental: false,
            model_name: "minilm-l6",
            progress: None,
            paths: None,
            exclude: &["vendor/".to_string()],
        })
        .unwrap();
        assert_eq!(res.files_pruned, 1);
        assert_eq!(indexed_files(&store), vec!["src/lib.rs".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A malformed pattern must not take the whole index down with it.
    #[test]
    fn a_broken_pattern_is_dropped_not_fatal() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_exclude3_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(&dir, "src/lib.rs", "pub fn kept() -> i32 { 1 }\n");
        commit_all(&dir, "initial");

        let store = Store::open_in_memory(DIM).unwrap();
        let r = index_excluding(&store, &dir, &["[".to_string()]);
        assert_eq!(r.files_indexed, 1, "a bad pattern must not stop indexing");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
