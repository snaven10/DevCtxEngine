//! `devctx-index` — the indexing pipeline for DevCtxEngine.
//!
//! Orchestrates git diff → parse → chunk → embed → store with deterministic
//! chunk ids and incremental `index_state`/`file_state`. Parseable code also
//! yields call-graph edges + routes; raw-text files (markdown/json/yaml/…) are
//! indexed as file-spanning chunks. Branch-aware: rows whose `content_hash`
//! matches an already-indexed branch are copied rather than re-embedded.
//! See `docs/architecture-spec.md` §9.

pub mod error;
pub mod git;
pub mod id;
pub mod pipeline;

pub use devctx_parse::extractor_fingerprint;
pub use error::{IndexError, Result};
pub use git::{Change, GitRepo, GitState};
pub use id::chunk_id;
pub use pipeline::{
    run, IndexRequest, IndexResult, ProgressSink, PENDING_FTS_META_KEY, PENDING_HNSW_META_KEY,
};

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
            branch: None,
        })
        .unwrap()
    }

    fn index_branch(store: &Store, root: &Path, branch: &str, full: bool) -> IndexResult {
        run(IndexRequest {
            store,
            embedder: &FakeEmbedder,
            repo_root: root,
            incremental: !full,
            model_name: "minilm-l6",
            progress: None,
            paths: None,
            exclude: &[],
            branch: Some(branch),
        })
        .unwrap()
    }

    /// A repository with `main` and a feature branch that changes one file.
    fn two_branch_repo(tag: &str) -> PathBuf {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_branch_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        write(&dir, "a.py", "def alpha():\n    return 1\n");
        write(&dir, "b.py", "def beta():\n    return 2\n");
        commit_all(&dir, "main");

        git(&dir, &["checkout", "-q", "-b", "feature"]);
        write(&dir, "b.py", "def beta():\n    return 22\n");
        commit_all(&dir, "feature");
        git(&dir, &["checkout", "-q", "main"]);
        dir
    }

    fn files_on(store: &Store, branch: &str) -> Vec<String> {
        let mut v: Vec<String> = store
            .search(
                &[0.1; DIM],
                &SearchFilter {
                    branch: Some(branch.to_string()),
                    ..Default::default()
                },
                100,
            )
            .unwrap()
            .into_iter()
            .map(|h| h.point.metadata.file)
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The point of the whole design: a branch that is not checked out can be
    /// indexed, from git rather than from disk, and its rows stay its own.
    #[test]
    fn a_branch_that_is_not_checked_out_can_be_indexed() {
        let dir = two_branch_repo("notout");
        let store = Store::open_in_memory(DIM).unwrap();

        let main = index_branch(&store, &dir, "main", true);
        assert_eq!(main.branch, "main");
        let feat = index_branch(&store, &dir, "feature", true);
        assert_eq!(feat.branch, "feature", "the run is about the named branch");

        assert_eq!(files_on(&store, "main"), vec!["a.py", "b.py"]);
        assert_eq!(files_on(&store, "feature"), vec!["a.py", "b.py"]);

        // And the content differs where the branches differ.
        let on = |branch: &str| -> String {
            store
                .search(
                    &[0.1; DIM],
                    &SearchFilter {
                        branch: Some(branch.into()),
                        ..Default::default()
                    },
                    100,
                )
                .unwrap()
                .into_iter()
                .filter(|h| h.point.metadata.file == "b.py")
                .map(|h| h.point.text)
                .collect()
        };
        assert!(on("main").contains("return 2"), "{}", on("main"));
        assert!(on("feature").contains("return 22"), "{}", on("feature"));
    }

    /// Counts how many texts were embedded, so a test can prove the pipeline
    /// actually took the copy path rather than that a query would have allowed
    /// it to.
    struct CountingEmbedder(std::sync::atomic::AtomicUsize);
    impl EmbeddingProvider for CountingEmbedder {
        fn embed(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
            self.0
                .fetch_add(texts.len(), std::sync::atomic::Ordering::SeqCst);
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

    /// The claim the whole design rests on, measured rather than assumed: the
    /// second branch must not re-embed what the first already has.
    ///
    /// An earlier test asserted only that the lookup *would* find the shared
    /// content, which proves the query and not the pipeline. Counting the
    /// embedder is what distinguishes the two.
    #[test]
    fn the_second_branch_embeds_only_what_actually_differs() {
        let dir = two_branch_repo("noreembed");
        let store = Store::open_in_memory(DIM).unwrap();

        let first = CountingEmbedder(std::sync::atomic::AtomicUsize::new(0));
        run(IndexRequest {
            store: &store,
            embedder: &first,
            repo_root: &dir,
            incremental: false,
            model_name: "minilm-l6",
            progress: None,
            paths: None,
            exclude: &[],
            branch: Some("main"),
        })
        .unwrap();
        let on_main = first.0.load(std::sync::atomic::Ordering::SeqCst);
        assert!(on_main > 0, "main had to embed something");

        // `feature` changes b.py and leaves a.py byte-identical.
        let second = CountingEmbedder(std::sync::atomic::AtomicUsize::new(0));
        run(IndexRequest {
            store: &store,
            embedder: &second,
            repo_root: &dir,
            incremental: false,
            model_name: "minilm-l6",
            progress: None,
            paths: None,
            exclude: &[],
            branch: Some("feature"),
        })
        .unwrap();
        let on_feature = second.0.load(std::sync::atomic::Ordering::SeqCst);

        assert!(
            on_feature < on_main,
            "the second branch embedded {on_feature} chunks against {on_main} for the \
             first, so nothing was reused"
        );
    }

    /// Re-indexing a branch is the ordinary operation, not an edge case, and it
    /// used to abort the whole run: the copy path inserted graph edges without
    /// clearing the destination's, so the second pass collided with the
    /// uniqueness constraint. Every test here indexed each branch exactly once,
    /// which is why a fixture cleaner than reality kept passing.
    #[test]
    fn a_branch_can_be_indexed_twice() {
        let dir = two_branch_repo("twice");
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);

        let first = index_branch(&store, &dir, "feature", true);
        assert!(first.files_indexed > 0);
        // The pass that used to fail.
        let second = index_branch(&store, &dir, "feature", true);
        assert_eq!(
            second.files_indexed, first.files_indexed,
            "a repeat of the same work must produce the same result"
        );

        // And it is a replacement, not an accumulation.
        assert_eq!(files_on(&store, "feature"), vec!["a.py", "b.py"]);
        assert_eq!(files_on(&store, "main"), vec!["a.py", "b.py"]);
    }

    /// `--full` must cure a stale index, so it may not copy rows another branch
    /// holds from an older extractor: copying them and then stamping the branch
    /// fresh would hide the very staleness the stamp exists to report.
    #[test]
    fn a_full_run_does_not_copy_rows_from_a_stale_branch() {
        let dir = two_branch_repo("stale_copy");
        let store = Store::open_in_memory(DIM).unwrap();
        let repo_path = GitRepo::open(&dir)
            .unwrap()
            .root()
            .to_string_lossy()
            .to_string();
        index_branch(&store, &dir, "feature", true);
        store
            .set_index_meta(&repo_path, "feature", "extractor", "v0-old")
            .unwrap();

        let main = index_branch(&store, &dir, "main", true);
        assert!(main.full_reindex);
        assert_eq!(main.files_copied, 0, "stale rows must not be copied");
        assert!(!main.extractor_stale);
        let now = extractor_fingerprint();
        assert!(!store.extractor_stale(&repo_path, "main", &now).unwrap());

        // Control: main is now fresh, so a fresh branch may copy from it.
        store
            .set_index_meta(&repo_path, "feature", "extractor", &now)
            .unwrap();
        let again = index_branch(&store, &dir, "feature", true);
        assert!(
            again.files_copied > 0,
            "a current-extractor branch is a valid source"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The index of a version that predates `index_meta` has no extractor
    /// record at all. `--full` must treat that as stale too: copy nothing.
    #[test]
    fn a_full_run_does_not_copy_from_a_branch_with_no_extractor_record() {
        let dir = two_branch_repo("no_record");
        let store = Store::open_in_memory(DIM).unwrap();
        let repo_path = GitRepo::open(&dir)
            .unwrap()
            .root()
            .to_string_lossy()
            .to_string();
        index_branch(&store, &dir, "feature", true);
        store
            .delete_index_meta(&repo_path, "feature", "extractor")
            .unwrap();
        assert!(store
            .get_index_meta(&repo_path, "feature", "extractor")
            .unwrap()
            .is_none());

        let main = index_branch(&store, &dir, "main", true);
        assert!(main.full_reindex);
        // The discriminating assertion: the JOIN on index_meta finds no row.
        assert_eq!(main.files_copied, 0, "an unrecorded branch is not a source");
        assert!(main.files_indexed > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Branches share commits, so the file they share must not be embedded
    /// twice — that is what makes keeping several branches indexed affordable.
    #[test]
    fn an_unchanged_file_is_copied_rather_than_re_embedded() {
        let dir = two_branch_repo("dedup");
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        // The key the pipeline files rows under is git's toplevel, which is
        // not `dir` when the temp dir is a symlink (macOS `/var`).
        let repo_path = GitRepo::open(&dir)
            .unwrap()
            .root()
            .to_string_lossy()
            .to_string();

        // `a.py` is byte-identical on both branches; `b.py` is not.
        let hash_a = store
            .get_file_hash(&repo_path, "main", "a.py")
            .unwrap()
            .expect("indexed on main");
        assert_eq!(
            store
                .branch_with_same_content(
                    &repo_path,
                    "a.py",
                    &hash_a,
                    "main",
                    &extractor_fingerprint()
                )
                .unwrap(),
            None,
            "only main has it so far"
        );

        index_branch(&store, &dir, "feature", true);

        // Now the shared file resolves across branches, and the changed one does not.
        assert_eq!(
            store
                .branch_with_same_content(
                    &repo_path,
                    "a.py",
                    &hash_a,
                    "feature",
                    &extractor_fingerprint()
                )
                .unwrap()
                .as_deref(),
            Some("main"),
            "the shared file was recognised as already indexed"
        );
        let hash_b_main = store
            .get_file_hash(&repo_path, "main", "b.py")
            .unwrap()
            .unwrap();
        let hash_b_feat = store
            .get_file_hash(&repo_path, "feature", "b.py")
            .unwrap()
            .unwrap();
        assert_ne!(hash_b_main, hash_b_feat, "the changed file must differ");
    }

    /// Dropping a branch takes its rows and nothing else. A branch merged and
    /// deleted must not leave rows a reused name would inherit.
    #[test]
    fn dropping_a_branch_leaves_the_others_intact() {
        let dir = two_branch_repo("drop");
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        index_branch(&store, &dir, "feature", true);

        let repo = dir.file_name().unwrap().to_string_lossy().to_string();
        let removed = store
            .drop_branch(&repo, &dir.to_string_lossy(), "feature")
            .unwrap();
        assert!(removed > 0, "the branch had rows");

        assert!(
            files_on(&store, "feature").is_empty(),
            "the dropped branch must have no rows left"
        );
        assert_eq!(files_on(&store, "main"), vec!["a.py", "b.py"]);
        assert!(!store.has_branch_rows(&repo, "feature").unwrap());
        assert!(store.has_branch_rows(&repo, "main").unwrap());
    }

    /// A branch git does not have is reported, never created.
    #[test]
    fn indexing_an_unknown_branch_is_an_error() {
        let dir = two_branch_repo("unknown");
        let store = Store::open_in_memory(DIM).unwrap();
        let err = run(IndexRequest {
            store: &store,
            embedder: &FakeEmbedder,
            repo_root: &dir,
            incremental: false,
            model_name: "minilm-l6",
            progress: None,
            paths: None,
            exclude: &[],
            branch: Some("no-such-branch"),
        })
        .unwrap_err();
        assert!(
            matches!(err, IndexError::UnknownBranch(ref b) if b == "no-such-branch"),
            "got {err:?}"
        );
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
            branch: None,
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
    fn an_index_from_an_older_extractor_stays_stale_until_a_full_run() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_extractor_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(&dir, "a.rs", "pub fn a() -> i32 { 1 }\n");
        commit_all(&dir, "init");

        let store = Store::open_in_memory(DIM).unwrap();
        let r1 = index(&store, &dir);
        let repo_path = GitRepo::open(&dir)
            .unwrap()
            .root()
            .to_string_lossy()
            .to_string();
        let branch = r1.branch.clone();
        let now = extractor_fingerprint();
        assert!(
            !store.extractor_stale(&repo_path, &branch, &now).unwrap(),
            "a fresh index carries the current extractor"
        );
        assert!(!r1.extractor_stale);

        // An index made by another extractor (or by none at all).
        store
            .set_index_meta(&repo_path, &branch, "extractor", "v0-old")
            .unwrap();
        assert!(store.extractor_stale(&repo_path, &branch, &now).unwrap());
        write(&dir, "b.rs", "pub fn b() -> i32 { 2 }\n");
        commit_all(&dir, "add b");
        let r2 = index(&store, &dir);
        assert!(!r2.full_reindex);
        assert!(r2.extractor_stale, "an incremental run must not hide it");
        assert!(store.extractor_stale(&repo_path, &branch, &now).unwrap());

        let r3 = run(IndexRequest {
            store: &store,
            embedder: &FakeEmbedder,
            repo_root: &dir,
            incremental: false,
            model_name: "minilm-l6",
            progress: None,
            paths: None,
            exclude: &[],
            branch: None,
        })
        .unwrap();
        assert!(r3.full_reindex && !r3.extractor_stale);
        assert!(!store.extractor_stale(&repo_path, &branch, &now).unwrap());

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
            branch: None,
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
            branch: None,
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
            branch: None,
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
            "legacy/dep/mod.rs",
            "pub fn vendored() -> i32 { 3 }\n",
        );
        write(&dir, "docs/notes.md", "# Notes\n\nsome prose\n");
        commit_all(&dir, "initial");

        // A directory rule covers everything beneath it; a `*` rule matches at
        // any depth. Both are gitignore semantics, not literal globs.
        let exclude = vec![
            "legacy/".to_string(),
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
        write(&dir, "legacy/dep.rs", "pub fn vendored() -> i32 { 2 }\n");
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
            exclude: &["legacy/".to_string()],
            branch: None,
        })
        .unwrap();
        assert_eq!(res.files_pruned, 1);
        assert_eq!(indexed_files(&store), vec!["src/lib.rs".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A repository with tracked build output and vendored code, as git sees it.
    fn repo_with_generated_files(tag: &str) -> PathBuf {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(&dir, "src/lib.rs", "pub fn kept() -> i32 { 1 }\n");
        write(
            &dir,
            "dist/app.js",
            "export function bundled() { return 1 }\n",
        );
        write(&dir, "build/gen.rs", "pub fn built() -> i32 { 2 }\n");
        write(&dir, "target/out.rs", "pub fn compiled() -> i32 { 3 }\n");
        write(
            &dir,
            "node_modules/dep/index.js",
            "export function dep() { return 4 }\n",
        );
        commit_all(&dir, "initial");
        dir
    }

    /// Vendor / generated / build output that git *tracks* stays out of the
    /// index by default, and `default_excludes: false` brings it all back.
    #[test]
    fn default_excludes_keep_tracked_build_output_out_and_can_be_turned_off() {
        let dir = repo_with_generated_files("defex");

        let store = Store::open_in_memory(DIM).unwrap();
        let cfg = devctx_core::config::Indexing::default();
        index_excluding(&store, &dir, &cfg.effective_excludes());
        assert_eq!(indexed_files(&store), vec!["src/lib.rs".to_string()]);

        let store = Store::open_in_memory(DIM).unwrap();
        let off = devctx_core::config::Indexing {
            default_excludes: false,
            ..Default::default()
        };
        index_excluding(&store, &dir, &off.effective_excludes());
        assert_eq!(
            indexed_files(&store),
            vec![
                "build/gen.rs".to_string(),
                "dist/app.js".to_string(),
                "node_modules/dep/index.js".to_string(),
                "src/lib.rs".to_string(),
                "target/out.rs".to_string(),
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `include_build: true` opts `build/` back in and nothing else.
    #[test]
    fn include_build_reindexes_build_but_not_the_other_defaults() {
        let dir = repo_with_generated_files("incbuild");
        let store = Store::open_in_memory(DIM).unwrap();
        let cfg = devctx_core::config::Indexing {
            include_build: true,
            ..Default::default()
        };
        index_excluding(&store, &dir, &cfg.effective_excludes());
        assert_eq!(
            indexed_files(&store),
            vec!["build/gen.rs".to_string(), "src/lib.rs".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Switching the defaults on over an existing index prunes what they cover
    /// on the next plain (incremental) `index` — the exclude set is
    /// fingerprinted, and a changed one reconciles the index against it
    /// (prune the newly excluded, add the newly included) without a full run.
    #[test]
    fn a_changed_exclude_set_prunes_on_the_next_incremental_run() {
        let dir = repo_with_generated_files("defprune");
        let store = Store::open_in_memory(DIM).unwrap();
        index_excluding(&store, &dir, &[]);
        assert_eq!(indexed_files(&store).len(), 5);

        let defaults = devctx_core::config::Indexing::default().effective_excludes();
        let res = index_excluding(&store, &dir, &defaults);
        assert_eq!(res.files_pruned, 4, "{res:?}");
        assert!(!res.full_reindex, "{res:?}");
        assert_eq!(indexed_files(&store), vec!["src/lib.rs".to_string()]);

        // Unchanged set again: back to cheap incremental runs, no pruning pass.
        let res = index_excluding(&store, &dir, &defaults);
        assert!(!res.full_reindex);

        // And opting back out brings the files in again, also incrementally:
        // only the four newly included files are embedded.
        let res = index_excluding(&store, &dir, &[]);
        assert!(!res.full_reindex, "{res:?}");
        assert_eq!(res.files_indexed, 4, "{res:?}");
        assert_eq!(indexed_files(&store).len(), 5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An index from before 0.9 has no exclude fingerprint, so the first
    /// `index` after upgrading saw a "changed" exclude set and forced a FULL
    /// run — every file re-embedded, about an hour per 1400 files. It must stay
    /// incremental: prune what the set now excludes, leave the rest untouched,
    /// stamp the fingerprint.
    #[test]
    fn an_index_without_an_exclude_fingerprint_is_reconciled_incrementally() {
        let dir = repo_with_generated_files("defupgrade");
        let store = Store::open_in_memory(DIM).unwrap();
        index_excluding(&store, &dir, &[]);
        assert_eq!(indexed_files(&store).len(), 5);

        // What a store written before the fingerprint existed looks like.
        let git = crate::git::GitRepo::open(&dir).unwrap();
        let repo_path = git.root().to_string_lossy().to_string();
        let branch = git.state().branch;
        store
            .delete_index_meta(&repo_path, &branch, crate::pipeline::EXCLUDE_META_KEY)
            .unwrap();

        let defaults = devctx_core::config::Indexing::default().effective_excludes();
        let res = index_excluding(&store, &dir, &defaults);
        assert!(
            !res.full_reindex,
            "an exclude change forced a full run: {res:?}"
        );
        assert_eq!(res.files_pruned, 4, "{res:?}");
        assert_eq!(
            res.files_indexed, 0,
            "unchanged files were re-embedded: {res:?}"
        );
        assert_eq!(indexed_files(&store), vec!["src/lib.rs".to_string()]);
        assert_eq!(
            store
                .get_index_meta(&repo_path, &branch, crate::pipeline::EXCLUDE_META_KEY)
                .unwrap()
                .as_deref(),
            Some(crate::pipeline::exclude_fingerprint(&defaults).as_str()),
            "the fingerprint is stamped after the reconcile"
        );

        // Settled: the next run has nothing to reconcile.
        let res = index_excluding(&store, &dir, &defaults);
        assert_eq!((res.files_pruned, res.files_indexed), (0, 0), "{res:?}");
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

    /// A sink that cancels once `after` files have been started, and records
    /// the phases it was told about.
    struct CancelAfter {
        after: usize,
        seen: std::sync::atomic::AtomicUsize,
        phases: std::sync::Mutex<Vec<String>>,
    }

    impl CancelAfter {
        fn new(after: usize) -> Self {
            Self {
                after,
                seen: std::sync::atomic::AtomicUsize::new(0),
                phases: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn phases(&self) -> Vec<String> {
            self.phases.lock().unwrap().clone()
        }
    }

    impl ProgressSink for CancelAfter {
        fn start(&self, _total: usize) {}
        fn file(&self, _path: &str) {
            self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn phase(&self, name: &str) {
            self.phases.lock().unwrap().push(name.to_string());
        }
        fn cancelled(&self) -> bool {
            self.seen.load(std::sync::atomic::Ordering::SeqCst) >= self.after
        }
    }

    fn run_with(store: &Store, root: &Path, full: bool, sink: &dyn ProgressSink) -> IndexResult {
        run(IndexRequest {
            store,
            embedder: &FakeEmbedder,
            repo_root: root,
            incremental: !full,
            model_name: "minilm-l6",
            progress: Some(sink),
            paths: None,
            exclude: &[],
            branch: None,
        })
        .unwrap()
    }

    fn five_file_repo(tag: &str) -> PathBuf {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_cancel_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        for i in 0..5 {
            write(
                &dir,
                &format!("m{i}.py"),
                &format!("def f{i}():\n    return {i}\n"),
            );
        }
        commit_all(&dir, "five");
        dir
    }

    /// PLAN-008 TASK-009 fixup D1: a server asked to stop cancels its index at
    /// the next file boundary instead of being cut mid-run. The cancelled run
    /// must not prune what it did not reach, nor advance the index record — the
    /// next run starts over from the same commit and finishes the job.
    #[test]
    fn a_cancelled_run_stops_between_files_and_leaves_the_record_alone() {
        let dir = five_file_repo("stop");
        let store = Store::open_in_memory(DIM).unwrap();
        let first = index(&store, &dir);
        assert_eq!(first.files_indexed, 5);
        let repo_path = GitRepo::open(&dir)
            .unwrap()
            .root()
            .to_string_lossy()
            .to_string();
        let before = store.get_index_record(&repo_path, "main").unwrap().unwrap();

        for i in 0..5 {
            write(
                &dir,
                &format!("m{i}.py"),
                &format!("def g{i}():\n    return {i}0\n"),
            );
        }
        commit_all(&dir, "touch all");

        let sink = CancelAfter::new(2);
        let cut = run_with(&store, &dir, false, &sink);
        assert!(cut.cancelled, "the run must report that it was cancelled");
        assert_eq!(cut.files_indexed, 2, "it stops at the file boundary");
        assert!(
            sink.phases().iter().any(|p| p == "checkpoint"),
            "a cancelled run still folds the WAL: {:?}",
            sink.phases()
        );
        let after = store.get_index_record(&repo_path, "main").unwrap().unwrap();
        assert_eq!(
            after.last_commit, before.last_commit,
            "a partial run must not claim the new commit"
        );
        assert_eq!(files_on(&store, "main").len(), 5, "nothing pruned");

        let rest = index(&store, &dir);
        assert!(!rest.cancelled);
        assert_eq!(rest.files_indexed, 3, "the next run finishes the job");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A full run that is cancelled must not prune the files it never reached.
    #[test]
    fn a_cancelled_full_run_prunes_nothing() {
        let dir = five_file_repo("full");
        let store = Store::open_in_memory(DIM).unwrap();
        index(&store, &dir);
        let sink = CancelAfter::new(1);
        let cut = run_with(&store, &dir, true, &sink);
        assert!(cut.cancelled);
        assert_eq!(cut.files_pruned, 0);
        assert_eq!(files_on(&store, "main").len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The phases after the file loop report themselves, so a watcher measuring
    /// "time since the run last moved" does not read them as a stuck run.
    #[test]
    fn the_phases_after_the_file_loop_report_progress() {
        let dir = five_file_repo("phases");
        let store = Store::open_in_memory(DIM).unwrap();
        index(&store, &dir);
        std::fs::remove_file(dir.join("m4.py")).unwrap();
        commit_all(&dir, "drop one");
        // A full run over an index holding a file git no longer lists prunes it.
        let sink = CancelAfter::new(usize::MAX);
        let r = run_with(&store, &dir, true, &sink);
        assert!(!r.cancelled);
        assert_eq!(r.files_pruned, 1);
        let phases = sink.phases();
        assert!(phases.iter().any(|p| p == "prune"), "{phases:?}");
        assert!(phases.iter().any(|p| p == "checkpoint"), "{phases:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dropped derived index is noted before the drop and the note survives a
    /// cancelled run; the next complete run rebuilds it and clears the note.
    #[test]
    fn a_dropped_derived_index_is_owed_to_the_next_run() {
        let dir = five_file_repo("pending");
        let store = Store::open_in_memory(DIM).unwrap();
        index(&store, &dir);
        // As if an earlier run had dropped the BM25 index and died.
        store
            .set_index_meta("", "", PENDING_FTS_META_KEY, "1")
            .unwrap();
        let cut = run_with(&store, &dir, true, &CancelAfter::new(0));
        assert!(cut.cancelled);
        assert!(
            store
                .get_index_meta("", "", PENDING_FTS_META_KEY)
                .unwrap()
                .is_some(),
            "a cancelled run keeps the index owed"
        );
        let done = index(&store, &dir);
        assert!(!done.cancelled);
        assert!(
            store
                .get_index_meta("", "", PENDING_FTS_META_KEY)
                .unwrap()
                .is_none(),
            "a complete run settles what was owed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancels at a named phase, or once `files` files have been started.
    struct CancelAt {
        phase: Option<&'static str>,
        files: usize,
        seen: std::sync::atomic::AtomicUsize,
        hit: std::sync::atomic::AtomicBool,
        phases: std::sync::Mutex<Vec<String>>,
    }

    impl CancelAt {
        fn phase(name: &'static str) -> Self {
            Self::new(Some(name), usize::MAX)
        }
        fn after_files(files: usize) -> Self {
            Self::new(None, files)
        }
        fn new(phase: Option<&'static str>, files: usize) -> Self {
            Self {
                phase,
                files,
                seen: Default::default(),
                hit: Default::default(),
                phases: Default::default(),
            }
        }
        fn phases(&self) -> Vec<String> {
            self.phases.lock().unwrap().clone()
        }
    }

    impl ProgressSink for CancelAt {
        fn start(&self, _total: usize) {}
        fn file(&self, _path: &str) {
            self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn phase(&self, name: &str) {
            self.phases.lock().unwrap().push(name.to_string());
            if self.phase == Some(name) {
                self.hit.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        fn cancelled(&self) -> bool {
            self.hit.load(std::sync::atomic::Ordering::SeqCst)
                || self.seen.load(std::sync::atomic::Ordering::SeqCst) >= self.files
        }
    }

    fn repo_path_of(dir: &Path) -> String {
        GitRepo::open(dir)
            .unwrap()
            .root()
            .to_string_lossy()
            .to_string()
    }

    /// D1b item 1: a stop that arrives after the file loop — here, once every
    /// file has been started — skips the derived-index rebuilds (minutes on a
    /// large repository, which a shutdown does not have) and keeps them owed.
    /// The data itself is complete, so the run is not "cancelled" and its
    /// record advances.
    #[test]
    fn a_stop_after_the_files_skips_the_rebuilds_and_keeps_them_owed() {
        let dir = five_file_repo("skiprebuild");
        let store = Store::open_in_memory(DIM).unwrap();
        index(&store, &dir);
        for i in 0..5 {
            write(
                &dir,
                &format!("m{i}.py"),
                &format!("def h{i}():\n    return {i}1\n"),
            );
        }
        commit_all(&dir, "touch");
        store
            .set_index_meta("", "", PENDING_FTS_META_KEY, "1")
            .unwrap();
        store
            .set_index_meta("", "", PENDING_HNSW_META_KEY, "cosine")
            .unwrap();
        let sink = CancelAt::after_files(5);
        let r = run_with(&store, &dir, false, &sink);
        assert!(!r.cancelled, "every file was written");
        assert_eq!(r.files_indexed, 5);
        let phases = sink.phases();
        assert!(
            !phases.iter().any(|p| p == "hnsw" || p == "fts"),
            "a stopping server must not start a rebuild: {phases:?}"
        );
        assert!(phases.iter().any(|p| p == "checkpoint"), "{phases:?}");
        for key in [PENDING_FTS_META_KEY, PENDING_HNSW_META_KEY] {
            assert!(
                store.get_index_meta("", "", key).unwrap().is_some(),
                "{key} must stay owed"
            );
        }
        let head = GitRepo::open(&dir).unwrap().state().commit;
        let rec = store
            .get_index_record(&repo_path_of(&dir), "main")
            .unwrap()
            .unwrap();
        assert_eq!(
            rec.last_commit, head,
            "the files are all in; the record advances"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D1b item 1: a stop during the prune stops it between files, like the
    /// file loop: nothing more is deleted and the record is not advanced.
    #[test]
    fn a_stop_during_the_prune_stops_it() {
        let dir = five_file_repo("pruncancel");
        let store = Store::open_in_memory(DIM).unwrap();
        let first = index(&store, &dir);
        let before = store
            .get_index_record(&repo_path_of(&dir), "main")
            .unwrap()
            .unwrap();
        assert_eq!(first.files_indexed, 5);
        std::fs::remove_file(dir.join("m3.py")).unwrap();
        std::fs::remove_file(dir.join("m4.py")).unwrap();
        commit_all(&dir, "drop two");
        let sink = CancelAt::phase("prune");
        let r = run_with(&store, &dir, true, &sink);
        assert!(r.cancelled, "the stop came in during the prune");
        assert_eq!(r.files_pruned, 0, "nothing deleted after the stop");
        assert_eq!(files_on(&store, "main").len(), 5);
        let after = store
            .get_index_record(&repo_path_of(&dir), "main")
            .unwrap()
            .unwrap();
        assert_eq!(after.last_commit, before.last_commit);
        let done = index_branch(&store, &dir, "main", true);
        assert_eq!(done.files_pruned, 2, "the next full run finishes the prune");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D1b item 1: a run that starts after the stop request drops nothing —
    /// it would only leave a rebuild owed that nobody runs in this process.
    #[test]
    fn a_run_started_after_the_stop_drops_nothing() {
        let dir = five_file_repo("latestart");
        let store = Store::open_in_memory(DIM).unwrap();
        index(&store, &dir);
        let has_fts = store.rebuild_fts().unwrap();
        let r = run_with(&store, &dir, true, &CancelAfter::new(0));
        assert!(r.cancelled);
        assert_eq!(r.files_indexed, 0);
        assert!(
            store
                .get_index_meta("", "", PENDING_FTS_META_KEY)
                .unwrap()
                .is_none(),
            "no derived index was taken down, so none is owed"
        );
        if has_fts {
            assert!(store.has_fts(), "the BM25 index is still there");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D1b item 2: the exit path freezes the database, checkpoints and ends
    /// the process while the indexing thread is still running. The thread's
    /// next write must fail rather than land in a WAL nothing will fold — and
    /// what it had committed before the freeze must be whole files only.
    #[test]
    fn a_write_attempted_after_the_freeze_never_reaches_the_wal() {
        struct FreezeAfter {
            files: usize,
            seen: std::sync::atomic::AtomicUsize,
            exit_conn: std::sync::Mutex<Store>,
        }
        impl ProgressSink for FreezeAfter {
            fn start(&self, _total: usize) {}
            fn file(&self, _path: &str) {
                let n = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                if n == self.files + 1 {
                    // What `exit_now` does, from another connection.
                    let exit = self.exit_conn.lock().unwrap();
                    assert!(exit.freeze(std::time::Duration::from_secs(1)));
                    exit.force_checkpoint().unwrap();
                }
            }
        }
        let dir = five_file_repo("freeze");
        let db = dir.join("idx.duckdb");
        let wal = dir.join("idx.duckdb.wal");
        let store = Store::open(&db, DIM).unwrap();
        let sink = FreezeAfter {
            files: 2,
            seen: Default::default(),
            exit_conn: std::sync::Mutex::new(store.try_clone().unwrap()),
        };
        let err = run(IndexRequest {
            store: &store,
            embedder: &FakeEmbedder,
            repo_root: &dir,
            incremental: false,
            model_name: "minilm-l6",
            progress: Some(&sink),
            paths: None,
            exclude: &[],
            branch: None,
        })
        .expect_err("the first write after the freeze must fail the run");
        assert!(err.to_string().contains("frozen"), "{err}");
        let wal_len = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
        assert_eq!(wal_len, 0, "a write after the freeze reached the WAL");
        drop(sink);
        drop(store);
        let reopened = Store::open(&db, DIM).unwrap();
        assert!(reopened.files_missing_vectors().unwrap().is_empty());
        let states = reopened
            .list_file_states(&repo_path_of(&dir), "main")
            .unwrap();
        assert_eq!(
            states.len(),
            2,
            "exactly the files committed before the freeze: {states:?}"
        );
        drop(reopened);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D1b item 4: what one transaction per file costs. A file-backed store
    /// (the WAL is where per-commit cost lives), `DEVCTX_BENCH_FILES` small
    /// files, a full run timed three times. Run with
    /// `cargo test -p devctx-index -- --ignored --nocapture bench_index_small_fixture`.
    #[test]
    #[ignore = "perf measurement; run explicitly"]
    fn bench_index_small_fixture() {
        let n: usize = std::env::var("DEVCTX_BENCH_FILES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_bench_tx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        for i in 0..n {
            write(
                &dir,
                &format!("src/m{i:04}.rs"),
                &format!(
                    "pub fn f{i}(x: u32) -> u32 {{\n    g{i}(x) + {i}\n}}\n\nfn g{i}(x: u32) -> u32 {{\n    x * 2\n}}\n"
                ),
            );
        }
        commit_all(&dir, "fixture");
        let mut times = Vec::new();
        for round in 0..3 {
            let db = dir.join(format!("bench{round}.duckdb"));
            let store = Store::open(&db, DIM).unwrap();
            let t0 = std::time::Instant::now();
            let r = index_branch(&store, &dir, "main", true);
            times.push(t0.elapsed());
            assert_eq!(r.files_indexed, n);
        }
        times.sort();
        eprintln!(
            "\nfull index of {n} files, 3 rounds: {times:?} (median {:?})\n",
            times[1]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
