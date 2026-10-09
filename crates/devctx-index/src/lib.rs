//! `devctx-index` — the indexing pipeline for DevCtxEngine.
//!
//! Orchestrates git diff → parse → chunk → embed → store with deterministic
//! chunk ids and incremental `index_state`/`file_state`. Parseable code also
//! yields call-graph edges + routes; raw-text files (markdown/json/yaml/…) are
//! indexed as file-spanning chunks. Branch-aware: rows whose `content_hash`
//! matches an already-indexed branch are copied rather than re-embedded.
//! See `docs/architecture-spec.md` §9.

mod env;
pub mod error;
pub mod git;
pub mod id;
mod link;
mod pagerank;
pub mod pipeline;
mod tsconfig;

pub use devctx_parse::{extractor_fingerprint, lang_for_extension, raw_text_language};
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
            hnsw: None,
            embed_fingerprint: "test-fp",
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
            hnsw: None,
            embed_fingerprint: "test-fp",
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
            hnsw: None,
            embed_fingerprint: "test-fp",
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
            hnsw: None,
            embed_fingerprint: "test-fp",
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
            second.files_indexed + second.files_unchanged,
            first.files_indexed,
            "a repeat of the same work must cover the same files"
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
        assert!(!store
            .extractor_stale(&repo_short_of(&dir), &repo_path, "main", &now)
            .unwrap());

        // Control: main is now fresh, so a fresh branch may copy from it.
        // (Dropped first: a branch that already holds the file copies nothing.)
        let repo = dir.file_name().unwrap().to_string_lossy().to_string();
        store.drop_branch(&repo, &repo_path, "feature").unwrap();
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
                    &devctx_store::CopySetup {
                        repo: &repo_short_of(&dir),
                        extractor: &extractor_fingerprint(),
                        embed_fp: "test-fp",
                        model_name: "minilm-l6",
                        dimension: DIM as i64
                    }
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
                    &devctx_store::CopySetup {
                        repo: &repo_short_of(&dir),
                        extractor: &extractor_fingerprint(),
                        embed_fp: "test-fp",
                        model_name: "minilm-l6",
                        dimension: DIM as i64
                    }
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
            hnsw: None,
            embed_fingerprint: "test-fp",
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
            hnsw: None,
            embed_fingerprint: "test-fp",
            branch: None,
        })
        .unwrap();

        assert!(r2.full_reindex);
        assert_eq!(
            r2.files_indexed + r2.files_unchanged,
            1,
            "only a.rs remains"
        );
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
            !store
                .extractor_stale(&repo_short_of(&dir), &repo_path, &branch, &now)
                .unwrap(),
            "a fresh index carries the current extractor"
        );
        assert!(!r1.extractor_stale);

        // An index made by another extractor (or by none at all).
        store
            .set_index_meta(&repo_path, &branch, "extractor", "v0-old")
            .unwrap();
        assert!(store
            .extractor_stale(&repo_short_of(&dir), &repo_path, &branch, &now)
            .unwrap());
        write(&dir, "b.rs", "pub fn b() -> i32 { 2 }\n");
        commit_all(&dir, "add b");
        let r2 = index(&store, &dir);
        assert!(!r2.full_reindex);
        assert!(r2.extractor_stale, "an incremental run must not hide it");
        assert!(store
            .extractor_stale(&repo_short_of(&dir), &repo_path, &branch, &now)
            .unwrap());

        let r3 = run(IndexRequest {
            store: &store,
            embedder: &FakeEmbedder,
            repo_root: &dir,
            incremental: false,
            model_name: "minilm-l6",
            progress: None,
            paths: None,
            exclude: &[],
            hnsw: None,
            embed_fingerprint: "test-fp",
            branch: None,
        })
        .unwrap();
        assert!(r3.full_reindex && !r3.extractor_stale);
        assert!(!store
            .extractor_stale(&repo_short_of(&dir), &repo_path, &branch, &now)
            .unwrap());

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
            hnsw: None,
            embed_fingerprint: "test-fp",
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
            hnsw: None,
            embed_fingerprint: "test-fp",
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
            hnsw: None,
            embed_fingerprint: "test-fp",
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
            hnsw: None,
            embed_fingerprint: "test-fp",
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
            hnsw: None,
            embed_fingerprint: "test-fp",
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

    fn repo_short_of(dir: &Path) -> String {
        GitRepo::open(dir).unwrap().short_name()
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
            hnsw: None,
            embed_fingerprint: "test-fp",
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

    /// PLAN-008 Q-3 precision: `build/` and `target/` are excluded at the
    /// repository root only. Nested, both are ordinary source names.
    #[test]
    fn build_and_target_are_excluded_at_the_root_only() {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_index_rootex_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        write(&dir, "src/lib.rs", "pub fn kept() -> i32 { 1 }\n");
        write(
            &dir,
            "src/x/build/Builder.java",
            "class Builder { int build() { return 1; } }\n",
        );
        write(
            &dir,
            "src/com/acme/target/Aim.java",
            "class Aim { int aim() { return 2; } }\n",
        );
        write(&dir, "build/out.js", "export function out() { return 3 }\n");
        write(&dir, "target/gen.rs", "pub fn gen() -> i32 { 4 }\n");
        write(
            &dir,
            "pkg/web/dist/app.js",
            "export function bundled() { return 5 }\n",
        );
        commit_all(&dir, "initial");

        let store = Store::open_in_memory(DIM).unwrap();
        let defaults = devctx_core::config::Indexing::default().effective_excludes();
        index_excluding(&store, &dir, &defaults);
        assert_eq!(
            indexed_files(&store),
            vec![
                "src/com/acme/target/Aim.java".to_string(),
                "src/lib.rs".to_string(),
                "src/x/build/Builder.java".to_string(),
            ],
            "nested build/ and target/ are source; root ones and any dist/ are output"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reconcile names what each pattern removed, not just how many.
    #[test]
    fn exclude_counts_are_per_deciding_pattern() {
        let ex = crate::pipeline::build_exclude(&[
            "/build/".to_string(),
            "dist/".to_string(),
            "*.min.js".to_string(),
        ]);
        let files: Vec<String> = [
            "build/a.js",
            "build/b/c.js",
            "web/dist/x.js",
            "lib/jq.min.js",
            ".devctx/state/x",
            "src/kept.rs",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            crate::pipeline::exclude_counts(&ex, &files),
            vec![
                ("/build/".to_string(), 2),
                ("dist/".to_string(), 1),
                ("*.min.js".to_string(), 1),
                ("(devctx artifacts)".to_string(), 1),
            ]
        );
    }

    /// An exclude change on the same run as ordinary commits: the reconcile
    /// must prune and add *and* the incremental diff must still be indexed.
    #[test]
    fn an_exclude_change_and_a_non_empty_diff_are_both_applied() {
        let dir = repo_with_generated_files("exdiff");
        let store = Store::open_in_memory(DIM).unwrap();
        index_excluding(&store, &dir, &[]);
        assert_eq!(indexed_files(&store).len(), 5);

        write(&dir, "src/lib.rs", "pub fn kept() -> i32 { 10 }\n");
        write(&dir, "src/new.rs", "pub fn fresh() -> i32 { 11 }\n");
        commit_all(&dir, "change");

        let defaults = devctx_core::config::Indexing::default().effective_excludes();
        let res = index_excluding(&store, &dir, &defaults);
        assert!(!res.full_reindex, "{res:?}");
        assert_eq!(res.files_pruned, 4, "{res:?}");
        assert_eq!(res.files_indexed, 2, "the diff was dropped: {res:?}");
        assert_eq!(
            indexed_files(&store),
            vec!["src/lib.rs".to_string(), "src/new.rs".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reconcile cut by a stop request must not stamp the new exclude
    /// fingerprint: the next run reconciles again and finishes the job.
    #[test]
    fn a_cancelled_reconcile_is_redone_by_the_next_run() {
        let dir = repo_with_generated_files("excancel");
        let store = Store::open_in_memory(DIM).unwrap();
        let defaults = devctx_core::config::Indexing::default().effective_excludes();
        index_excluding(&store, &dir, &defaults);
        assert_eq!(indexed_files(&store), vec!["src/lib.rs".to_string()]);

        // Opting out brings four files back; stop after the first.
        let sink = CancelAfter::new(1);
        let res = run(IndexRequest {
            store: &store,
            embedder: &FakeEmbedder,
            repo_root: &dir,
            incremental: true,
            model_name: "minilm-l6",
            progress: Some(&sink),
            paths: None,
            exclude: &[],
            hnsw: None,
            embed_fingerprint: "test-fp",
            branch: None,
        })
        .unwrap();
        assert!(res.cancelled, "{res:?}");
        assert!(indexed_files(&store).len() < 5);

        let git = crate::git::GitRepo::open(&dir).unwrap();
        let repo_path = git.root().to_string_lossy().to_string();
        let branch = git.state().branch;
        assert_eq!(
            store
                .get_index_meta(&repo_path, &branch, crate::pipeline::EXCLUDE_META_KEY)
                .unwrap()
                .as_deref(),
            Some(crate::pipeline::exclude_fingerprint(&defaults).as_str()),
            "a cancelled reconcile stamped the new exclude set"
        );

        let res = index_excluding(&store, &dir, &[]);
        assert!(!res.cancelled && !res.full_reindex, "{res:?}");
        assert_eq!(indexed_files(&store).len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }
    // ---- vector reuse by content hash (PLAN-009 TASK-002) -------------------

    /// One run with the counting embedder, `full` or incremental, under `model`.
    fn run_counting(store: &Store, root: &Path, full: bool, model: &str) -> (IndexResult, usize) {
        let emb = CountingEmbedder(std::sync::atomic::AtomicUsize::new(0));
        let res = run(IndexRequest {
            store,
            embedder: &emb,
            repo_root: root,
            incremental: !full,
            model_name: model,
            progress: None,
            paths: None,
            exclude: &[],
            hnsw: None,
            embed_fingerprint: "test-fp",
            branch: None,
        })
        .unwrap();
        (res, emb.0.load(std::sync::atomic::Ordering::SeqCst))
    }

    /// Three functions, each far above the minimum chunk size so none merges
    /// into its neighbour; `beta_ret` is what the middle one returns.
    fn three_functions(beta_ret: &str) -> String {
        let body = |name: &str, ret: &str| {
            let pad: String = (0..12)
                .map(|i| format!("    step_{i} = compute_value_{name}({i}) + {i}\n"))
                .collect();
            format!("def {name}():\n{pad}    return {ret}\n")
        };
        format!(
            "{}\n\n{}\n\n{}",
            body("alpha", "1"),
            body("beta", beta_ret),
            body("gamma", "3")
        )
    }

    fn reuse_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devctx_reuse_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        write(&dir, "m.py", &three_functions("2"));
        commit_all(&dir, "init");
        dir
    }

    /// Every vector of `file`, keyed by chunk hash.
    fn vectors_of(
        store: &Store,
        root: &Path,
        file: &str,
    ) -> std::collections::HashMap<String, Vec<f32>> {
        let git = crate::git::GitRepo::open(root).unwrap();
        store
            .vectors_by_hash(&git.short_name(), &git.state().branch, file)
            .unwrap()
    }

    /// `--full` over identical source (the rollout `EXTRACTOR_VERSION` forces)
    /// must not pay for the embedder again, and must keep the same vectors.
    #[test]
    fn a_full_reindex_of_unchanged_source_embeds_nothing() {
        let dir = reuse_repo("full");
        let store = Store::open_in_memory(DIM).unwrap();
        let (first, n1) = run_counting(&store, &dir, true, "minilm-l6");
        assert!(n1 > 0 && first.chunks_reused == 0, "{first:?} {n1}");
        let before = vectors_of(&store, &dir, "m.py");
        assert!(!before.is_empty());

        let (second, n2) = run_counting(&store, &dir, true, "minilm-l6");
        assert!(second.full_reindex, "{second:?}");
        assert_eq!(n2, 0, "unchanged chunks went to the embedder again");
        assert_eq!(second.chunks_reused, second.chunks, "{second:?}");
        assert_eq!(vectors_of(&store, &dir, "m.py"), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Editing one function embeds that function's chunk(s), not the file's.
    #[test]
    fn editing_one_function_embeds_only_its_chunks() {
        let dir = reuse_repo("edit");
        let store = Store::open_in_memory(DIM).unwrap();
        let (first, n1) = run_counting(&store, &dir, false, "minilm-l6");
        assert!(first.chunks >= 3 && n1 == first.chunks, "{first:?} {n1}");

        write(&dir, "m.py", &three_functions("22222"));
        commit_all(&dir, "edit beta");
        let (second, n2) = run_counting(&store, &dir, false, "minilm-l6");
        assert_eq!(second.files_indexed, 1, "{second:?}");
        assert!(
            n2 >= 1 && n2 < first.chunks,
            "embedded {n2} of {}",
            first.chunks
        );
        assert_eq!(second.chunks_reused, second.chunks - n2, "{second:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Vectors are only comparable within the model that made them.
    #[test]
    fn a_different_model_embeds_everything_again() {
        let dir = reuse_repo("model");
        let store = Store::open_in_memory(DIM).unwrap();
        let (first, _) = run_counting(&store, &dir, true, "minilm-l6");
        let (second, n2) = run_counting(&store, &dir, true, "bge-small");
        assert_eq!(second.chunks_reused, 0, "{second:?}");
        assert_eq!(
            n2, first.chunks,
            "every chunk must be embedded under a new model"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pipeline creates the HNSW index when the config wants one and the
    /// database never had it (the server path used to leave it out for good).
    #[test]
    fn a_new_index_gets_hnsw_when_the_config_asks_for_it() {
        // VSS unavailable (offline): nothing to assert.
        if !Store::open_in_memory(DIM)
            .unwrap()
            .enable_hnsw("cosine")
            .unwrap_or(false)
        {
            return;
        }
        let dir = reuse_repo("hnsw");
        let store = Store::open_in_memory(DIM).unwrap();
        run(IndexRequest {
            store: &store,
            embedder: &FakeEmbedder,
            repo_root: &dir,
            incremental: true,
            model_name: "minilm-l6",
            progress: None,
            paths: None,
            exclude: &[],
            hnsw: Some("cosine"),
            embed_fingerprint: "test-fp",
            branch: None,
        })
        .unwrap();
        assert_eq!(store.hnsw_metric().as_deref(), Some("cosine"));
        assert!(store
            .get_index_meta("", "", PENDING_HNSW_META_KEY)
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- fixup: lazy embedder, no needless rebuilds, fingerprint ------------

    /// A run under `fp` with the counting embedder, optionally watched by `sink`.
    fn run_fp(
        store: &Store,
        root: &Path,
        full: bool,
        fp: &str,
        hnsw: Option<&str>,
        sink: Option<&dyn ProgressSink>,
    ) -> (IndexResult, usize) {
        let emb = CountingEmbedder(std::sync::atomic::AtomicUsize::new(0));
        let res = run(IndexRequest {
            store,
            embedder: &emb,
            repo_root: root,
            incremental: !full,
            model_name: "minilm-l6",
            progress: sink,
            paths: None,
            exclude: &[],
            hnsw,
            embed_fingerprint: fp,
            branch: None,
        })
        .unwrap();
        (res, emb.0.load(std::sync::atomic::Ordering::SeqCst))
    }

    fn vss_available() -> bool {
        Store::open_in_memory(DIM)
            .unwrap()
            .enable_hnsw("cosine")
            .unwrap_or(false)
    }

    /// A reindex that finds every vector reusable never builds the model.
    #[test]
    fn a_no_change_full_run_never_loads_the_embedder() {
        let dir = reuse_repo("lazy");
        let store = Store::open_in_memory(DIM).unwrap();
        let loads = std::sync::atomic::AtomicUsize::new(0);
        let lazy = devctx_embed::LazyEmbedder::new(DIM, "minilm-l6", || {
            loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(std::sync::Arc::new(CountingEmbedder(Default::default()))
                as std::sync::Arc<dyn EmbeddingProvider>)
        });
        let go = |full: bool| {
            run(IndexRequest {
                store: &store,
                embedder: &lazy,
                repo_root: &dir,
                incremental: !full,
                model_name: "minilm-l6",
                progress: None,
                paths: None,
                exclude: &[],
                hnsw: None,
                embed_fingerprint: "test-fp",
                branch: None,
            })
            .unwrap()
        };
        let first = go(true);
        assert_eq!(
            loads.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "{first:?}"
        );
        let second = go(true);
        assert_eq!(second.chunks_reused, second.chunks, "{second:?}");
        assert_eq!(
            loads.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the model was loaded again for a run that embeds nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `--full` that changes no vector leaves the HNSW and BM25 indexes alone
    /// (on the parent it dropped and rebuilt both, whatever it had changed).
    #[test]
    fn a_no_change_full_run_does_not_rebuild_the_derived_indexes() {
        if !vss_available() {
            return;
        }
        let dir = reuse_repo("norebuild");
        let store = Store::open_in_memory(DIM).unwrap();
        run_fp(&store, &dir, true, "test-fp", Some("cosine"), None);
        assert_eq!(store.hnsw_metric().as_deref(), Some("cosine"));
        let fts = store.rebuild_fts().unwrap();
        let before = vectors_of(&store, &dir, "m.py");

        let sink = CancelAt::new(None, usize::MAX);
        let (res, n) = run_fp(&store, &dir, true, "test-fp", Some("cosine"), Some(&sink));
        assert!(res.full_reindex && n == 0, "{res:?} {n}");
        let phases = sink.phases();
        assert!(
            !phases.iter().any(|p| p == "hnsw" || p == "fts"),
            "rebuilt for a run that changed nothing: {phases:?}"
        );
        assert_eq!(store.hnsw_metric().as_deref(), Some("cosine"));
        assert_eq!(store.has_fts(), fts);
        assert!(store
            .get_index_meta("", "", PENDING_HNSW_META_KEY)
            .unwrap()
            .is_none());
        assert_eq!(vectors_of(&store, &dir, "m.py"), before);

        // An incremental run over nothing is the same.
        let sink = CancelAt::new(None, usize::MAX);
        run_fp(&store, &dir, false, "test-fp", Some("cosine"), Some(&sink));
        assert!(!sink.phases().iter().any(|p| p == "hnsw" || p == "fts"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a run does change still rebuilds, and what is owed or missing is
    /// still created by a run that changed nothing.
    #[test]
    fn owed_or_missing_indexes_are_built_even_when_nothing_changed() {
        if !vss_available() {
            return;
        }
        let dir = reuse_repo("owed");
        let store = Store::open_in_memory(DIM).unwrap();
        // No index ever existed; the config now wants one.
        run_fp(&store, &dir, true, "test-fp", None, None);
        assert!(store.hnsw_metric().is_none());
        let sink = CancelAt::new(None, usize::MAX);
        run_fp(&store, &dir, true, "test-fp", Some("cosine"), Some(&sink));
        assert_eq!(store.hnsw_metric().as_deref(), Some("cosine"));
        assert!(sink.phases().iter().any(|p| p == "hnsw"));

        // An earlier run dropped it and died: the note says it is owed.
        store.drop_hnsw().unwrap();
        store
            .set_index_meta("", "", PENDING_HNSW_META_KEY, "cosine")
            .unwrap();
        run_fp(&store, &dir, true, "test-fp", None, None);
        assert_eq!(store.hnsw_metric().as_deref(), Some("cosine"));
        assert!(store
            .get_index_meta("", "", PENDING_HNSW_META_KEY)
            .unwrap()
            .is_none());

        // A real change rebuilds.
        write(&dir, "m.py", &three_functions("22222"));
        commit_all(&dir, "edit");
        let sink = CancelAt::new(None, usize::MAX);
        run_fp(&store, &dir, false, "test-fp", Some("cosine"), Some(&sink));
        assert!(
            sink.phases().iter().any(|p| p == "hnsw"),
            "{:?}",
            sink.phases()
        );
        assert_eq!(store.hnsw_metric().as_deref(), Some("cosine"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Branch indexed under setup A, a run under B cut short, back to A: the
    /// files B already re-embedded hold B's vectors, so nothing may be reused.
    #[test]
    fn an_interrupted_run_under_another_setup_poisons_no_reuse() {
        let dir = five_file_repo("fp");
        let store = Store::open_in_memory(DIM).unwrap();
        let (first, n1) = run_fp(&store, &dir, true, "fp-a", None, None);
        assert_eq!(n1, first.chunks);
        // The same setup again does reuse.
        let (again, n) = run_fp(&store, &dir, true, "fp-a", None, None);
        assert!(
            (n, again.chunks_reused) == (0, again.chunks),
            "{again:?} {n}"
        );

        let sink = CancelAfter::new(2);
        let (cut, _) = run_fp(&store, &dir, true, "fp-b", None, Some(&sink));
        assert!(cut.cancelled, "{cut:?}");

        let (back, n) = run_fp(&store, &dir, false, "fp-a", None, None);
        assert_eq!(
            back.chunks_reused, 0,
            "reused vectors of another setup: {back:?}"
        );
        assert_eq!(n, back.chunks, "every chunk must be embedded again");
        assert!(
            back.full_reindex,
            "an interrupted transition needs a full run"
        );
        // Settled: now the same setup reuses again.
        let (settled, n) = run_fp(&store, &dir, true, "fp-a", None, None);
        assert!(
            n == 0 && settled.chunks_reused == settled.chunks,
            "{settled:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An index from before fingerprints, made by the same model at the same
    /// width, is reusable (every such index came from the one engine that
    /// existed) and is sealed with the active fingerprint by the run.
    #[test]
    fn a_legacy_index_with_the_same_model_is_reused_and_sealed() {
        let dir = reuse_repo("legacy");
        let store = Store::open_in_memory(DIM).unwrap();
        let (first, _) = run_fp(&store, &dir, true, "test-fp", None, None);
        let git = crate::git::GitRepo::open(&dir).unwrap();
        let (rp, branch) = (git.root().to_string_lossy().to_string(), git.state().branch);
        let drop_fp = || {
            store
                .delete_index_meta(&rp, &branch, devctx_store::EMBED_FP_META_KEY)
                .unwrap()
        };
        let fp_now = || {
            store
                .get_index_meta(&rp, &branch, devctx_store::EMBED_FP_META_KEY)
                .unwrap()
        };
        drop_fp();
        // Incremental: not forced into a full run, sealed.
        let (res, n) = run_fp(&store, &dir, false, "test-fp", None, None);
        assert!(!res.full_reindex && n == 0, "{res:?} {n}");
        assert_eq!(fp_now().as_deref(), Some("test-fp"));
        // Full over a legacy index: reused straight away, then sealed.
        drop_fp();
        let (full, n) = run_fp(&store, &dir, true, "test-fp", None, None);
        assert!(n == 0 && full.chunks_reused == first.chunks, "{full:?} {n}");
        assert_eq!(fp_now().as_deref(), Some("test-fp"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One run under `model` / `fp`, optionally on a named branch and watched.
    #[allow(clippy::too_many_arguments)]
    fn run_as(
        store: &Store,
        root: &Path,
        branch: Option<&str>,
        full: bool,
        fp: &str,
        model: &str,
        hnsw: Option<&str>,
        sink: Option<&dyn ProgressSink>,
    ) -> (IndexResult, usize) {
        let emb = CountingEmbedder(std::sync::atomic::AtomicUsize::new(0));
        let res = run(IndexRequest {
            store,
            embedder: &emb,
            repo_root: root,
            incremental: !full,
            model_name: model,
            progress: sink,
            paths: None,
            exclude: &[],
            hnsw,
            embed_fingerprint: fp,
            branch,
        })
        .unwrap();
        (res, emb.0.load(std::sync::atomic::Ordering::SeqCst))
    }

    /// A legacy index (no fingerprint) of another model is not reused, and an
    /// interrupted run of another model over it leaves it unusable by the old
    /// one (the "transition" mark is written for legacy indexes too).
    #[test]
    fn a_legacy_index_is_not_reused_across_models_or_after_a_transition() {
        let dir = five_file_repo("legacy2");
        let store = Store::open_in_memory(DIM).unwrap();
        let git = crate::git::GitRepo::open(&dir).unwrap();
        let (rp, branch) = (git.root().to_string_lossy().to_string(), git.state().branch);
        let key = devctx_store::EMBED_FP_META_KEY;

        run_as(&store, &dir, None, true, "fp", "minilm-l6", None, None);
        store.delete_index_meta(&rp, &branch, key).unwrap();
        // Other model: nothing reusable.
        let (other, n) = run_as(&store, &dir, None, true, "fp2", "bge-small", None, None);
        assert_eq!(other.chunks_reused, 0, "{other:?}");
        assert_eq!(n, other.chunks);

        // Legacy again, a run under another model is cut short, then the
        // original model comes back: files the cut run re-embedded hold the
        // other model's vectors, so nothing may be reused.
        store.delete_index_meta(&rp, &branch, key).unwrap();
        let sink = CancelAfter::new(2);
        let (cut, _) = run_as(&store, &dir, None, true, "fp", "minilm-l6", None, None);
        assert!(!cut.cancelled);
        store.delete_index_meta(&rp, &branch, key).unwrap();
        let (cut, _) = run_as(
            &store,
            &dir,
            None,
            true,
            "fp2",
            "bge-small",
            None,
            Some(&sink),
        );
        assert!(cut.cancelled, "{cut:?}");
        assert_eq!(
            store.get_index_meta(&rp, &branch, key).unwrap().as_deref(),
            Some("transition")
        );
        let (back, n) = run_as(&store, &dir, None, false, "fp", "minilm-l6", None, None);
        assert_eq!(back.chunks_reused, 0, "{back:?}");
        assert_eq!(n, back.chunks);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A branch that already holds everything has nothing to copy: a `--full`
    /// over two indexed branches writes nothing (on the parent it copied every
    /// file the other branch holds, rewriting rows and the derived indexes).
    #[test]
    fn a_full_run_over_two_indexed_branches_writes_nothing() {
        let dir = two_branch_repo("two_full");
        let store = Store::open_in_memory(DIM).unwrap();
        let (m, f) = ("minilm-l6", "test-fp");
        run_as(&store, &dir, Some("main"), true, f, m, None, None);
        run_as(&store, &dir, Some("feature"), true, f, m, None, None);
        let before = files_on(&store, "main");

        let hnsw = vss_available().then_some("cosine");
        run_as(&store, &dir, Some("main"), true, f, m, hnsw, None);
        let sink = CancelAt::new(None, usize::MAX);
        let (again, n) = run_as(&store, &dir, Some("main"), true, f, m, hnsw, Some(&sink));
        assert_eq!(again.files_copied, 0, "{again:?}");
        assert_eq!(n, 0, "{again:?}");
        assert_eq!(again.files_indexed, 0, "{again:?}");
        assert!(again.files_unchanged > 0, "{again:?}");
        assert_eq!(again.chunks_reused, again.chunks, "{again:?}");
        assert!(
            !sink.phases().iter().any(|p| p == "hnsw" || p == "fts"),
            "{:?}",
            sink.phases()
        );
        assert_eq!(files_on(&store, "main"), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A branch indexed under another embedding setup (or in transition) is no
    /// source for a copy: its vectors would be sealed under this one's name.
    /// A legacy branch (no fingerprint) is a source only for the same model.
    #[test]
    fn a_copy_needs_a_source_made_by_the_active_embedding_setup() {
        let key = devctx_store::EMBED_FP_META_KEY;
        let m = "minilm-l6";
        let setup = |tag: &str| {
            let dir = two_branch_repo(tag);
            let store = Store::open_in_memory(DIM).unwrap();
            let rp = GitRepo::open(&dir)
                .unwrap()
                .root()
                .to_string_lossy()
                .to_string();
            (dir, store, rp)
        };

        // Another fingerprint.
        let (dir, store, rp) = setup("cp_other");
        run_as(&store, &dir, Some("feature"), true, "fp-a", m, None, None);
        let (res, n) = run_as(&store, &dir, Some("main"), true, "fp-b", m, None, None);
        assert_eq!(res.files_copied, 0, "{res:?}");
        assert_eq!(n, res.chunks, "all embedded under the active setup");
        assert_eq!(
            store.get_index_meta(&rp, "main", key).unwrap().as_deref(),
            Some("fp-b")
        );
        let _ = std::fs::remove_dir_all(&dir);

        // Source in transition.
        let (dir, store, rp) = setup("cp_trans");
        run_as(&store, &dir, Some("feature"), true, "fp-a", m, None, None);
        store
            .set_index_meta(&rp, "feature", key, "transition")
            .unwrap();
        let (res, _) = run_as(&store, &dir, Some("main"), true, "fp-a", m, None, None);
        assert_eq!(res.files_copied, 0, "{res:?}");
        let _ = std::fs::remove_dir_all(&dir);

        // Same fingerprint: control, it does copy.
        let (dir, store, _) = setup("cp_same");
        run_as(&store, &dir, Some("feature"), true, "fp-a", m, None, None);
        let (res, _) = run_as(&store, &dir, Some("main"), true, "fp-a", m, None, None);
        assert!(res.files_copied > 0, "{res:?}");
        let _ = std::fs::remove_dir_all(&dir);

        // Legacy source, same model: compatible.
        let (dir, store, rp) = setup("cp_legacy");
        run_as(&store, &dir, Some("feature"), true, "fp-a", m, None, None);
        store.delete_index_meta(&rp, "feature", key).unwrap();
        let (res, _) = run_as(&store, &dir, Some("main"), true, "fp-a", m, None, None);
        assert!(res.files_copied > 0, "{res:?}");
        let _ = std::fs::remove_dir_all(&dir);

        // Legacy source of another model: not.
        let (dir, store, rp) = setup("cp_legacy_model");
        run_as(
            &store,
            &dir,
            Some("feature"),
            true,
            "fp-a",
            "bge-small",
            None,
            None,
        );
        store.delete_index_meta(&rp, "feature", key).unwrap();
        let (res, _) = run_as(&store, &dir, Some("main"), true, "fp-a", m, None, None);
        assert_eq!(res.files_copied, 0, "{res:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deleting a file the index never held changes nothing, so it must not
    /// take the derived indexes down (and force their rebuild).
    #[test]
    fn deleting_a_file_never_indexed_leaves_the_derived_indexes_alone() {
        if !vss_available() {
            return;
        }
        let dir = reuse_repo("ghost");
        let store = Store::open_in_memory(DIM).unwrap();
        run_fp(&store, &dir, true, "test-fp", Some("cosine"), None);
        let sink = CancelAt::new(None, usize::MAX);
        let ghost = vec!["ghost.py".to_string()];
        let res = run(IndexRequest {
            store: &store,
            embedder: &FakeEmbedder,
            repo_root: &dir,
            incremental: true,
            model_name: "minilm-l6",
            progress: Some(&sink),
            paths: Some(&ghost),
            exclude: &[],
            hnsw: Some("cosine"),
            embed_fingerprint: "test-fp",
            branch: None,
        })
        .unwrap();
        assert_eq!(res.files_deleted, 1, "{res:?}");
        assert!(
            !sink.phases().iter().any(|p| p == "hnsw" || p == "fts"),
            "{:?}",
            sink.phases()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Asking for another metric replaces the HNSW index (one exists at a time).
    #[test]
    fn enable_hnsw_replaces_the_index_when_the_metric_changes() {
        if !vss_available() {
            return;
        }
        let store = Store::open_in_memory(DIM).unwrap();
        assert!(store.enable_hnsw("cosine").unwrap());
        assert_eq!(store.hnsw_metric().as_deref(), Some("cosine"));
        assert!(store.enable_hnsw("ip").unwrap());
        assert_eq!(store.hnsw_metric().as_deref(), Some("ip"));
    }

    // ── PLAN-009 TASK-003: the `symbols` and `edges` tables ──────────────

    /// A one-commit repository on `main` with the given files.
    fn graph_repo(tag: &str, files: &[(&str, &str)]) -> (PathBuf, String) {
        let dir: PathBuf =
            std::env::temp_dir().join(format!("devctx_graph_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        for (rel, content) in files {
            write(&dir, rel, content);
        }
        commit_all(&dir, "init");
        let repo = dir.file_name().unwrap().to_string_lossy().to_string();
        (dir, repo)
    }

    fn symbol<'a>(
        syms: &'a [devctx_store::StoredSymbol],
        qualified: &str,
    ) -> &'a devctx_store::StoredSymbol {
        syms.iter()
            .find(|s| s.qualified == qualified)
            .unwrap_or_else(|| panic!("{qualified} not in {syms:?}"))
    }

    /// Every edge's source is a symbol of its own file and branch, and every
    /// parent is too: what "no orphan rows" means for these tables.
    fn assert_no_orphans(store: &Store, repo: &str, branch: &str, files: &[&str]) {
        for f in files {
            let syms = store.file_symbols(repo, branch, f).unwrap();
            let ids: HashSet<u64> = syms.iter().map(|s| s.id).collect();
            for s in &syms {
                if let Some(p) = s.parent_id {
                    assert!(ids.contains(&p), "{f}: parent of {s:?} is not in the file");
                }
            }
            for e in store.file_symbol_edges(repo, branch, f).unwrap() {
                assert!(
                    ids.contains(&e.src_id),
                    "{f}: source of {e:?} is not in the file"
                );
            }
        }
    }
    use std::collections::HashSet;

    /// The bug behind `get_references` losing calls: two calls to the same
    /// target from one method were folded into one `graph_edges` row. The
    /// `edges` table keeps one row per occurrence.
    #[test]
    fn two_calls_from_one_method_are_two_edges() {
        let (dir, repo) = graph_repo(
            "twice",
            &[(
                "mig.py",
                "def migrate_one(log):\n    log.debug('a')\n    work()\n    log.debug('b')\n",
            )],
        );
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let syms = store.file_symbols(&repo, "main", "mig.py").unwrap();
        let src = symbol(&syms, "migrate_one").id;
        let debug: Vec<_> = store
            .file_symbol_edges(&repo, "main", "mig.py")
            .unwrap()
            .into_iter()
            .filter(|e| e.dst_name == "debug")
            .collect();
        assert_eq!(debug.len(), 2, "{debug:?}");
        assert!(debug.iter().all(|e| e.src_id == src && e.kind == "calls"));
        assert_eq!((debug[0].line, debug[1].line), (2, 4));
        assert!(debug
            .iter()
            .all(|e| e.dst_id.is_none() && e.edge_source == "treesitter"));
        // `graph_edges` keeps the 0.9.0 shape: one row per pair.
        let old = store.graph_edges(&repo, "main", None, None, 0).unwrap();
        assert_eq!(old.iter().filter(|e| e.target == "debug").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Python call at module level has no enclosing function, and used to
    /// be dropped. It is an edge from the file symbol now.
    #[test]
    fn a_module_level_python_call_is_sourced_from_the_file_symbol() {
        let (dir, repo) = graph_repo(
            "module",
            &[(
                "tools/run.py",
                "def main():\n    pass\n\nmain()\nconfigure(main)\n",
            )],
        );
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let syms = store.file_symbols(&repo, "main", "tools/run.py").unwrap();
        let file = &syms[0];
        assert_eq!(file.kind, "file");
        assert_eq!(file.qualified, "tools/run.py");
        assert_eq!(file.name, "run.py");
        assert_eq!(file.parent_id, None);
        assert_eq!(symbol(&syms, "main").parent_id, Some(file.id));
        let edges = store
            .file_symbol_edges(&repo, "main", "tools/run.py")
            .unwrap();
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == "calls").collect();
        let names: Vec<_> = calls
            .iter()
            .map(|e| (e.dst_name.as_str(), e.line))
            .collect();
        assert_eq!(names, vec![("main", 4), ("configure", 5)]);
        assert!(calls.iter().all(|e| e.src_id == file.id));
        // `graph_edges` has no row for them: 0.9.0 has no source to give one.
        assert!(store
            .graph_edges(&repo, "main", None, None, 0)
            .unwrap()
            .iter()
            .all(|e| !e.source.is_empty()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The id carries no branch: the same symbol on two branches — copied or
    /// re-parsed — has one id, and a copy carries edges without resolution.
    #[test]
    fn a_symbol_has_one_id_on_every_branch() {
        let dir = two_branch_repo("graph_ids");
        let repo = dir.file_name().unwrap().to_string_lossy().to_string();
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let feat = index_branch(&store, &dir, "feature", true);
        assert!(feat.files_copied > 0, "a.py is copied, b.py re-parsed");
        for f in ["a.py", "b.py"] {
            let main = store.file_symbols(&repo, "main", f).unwrap();
            let feature = store.file_symbols(&repo, "feature", f).unwrap();
            assert_eq!(main.len(), 2, "{main:?}");
            let ids = |v: &[devctx_store::StoredSymbol]| v.iter().map(|s| s.id).collect::<Vec<_>>();
            assert_eq!(ids(&main), ids(&feature), "{f}");
        }
        assert_eq!(
            symbol(
                &store.file_symbols(&repo, "feature", "b.py").unwrap(),
                "beta"
            )
            .id,
            devctx_parse::symbol_id::symbol_id(&repo, "b.py", "callable", "beta", "")
        );
        assert_no_orphans(&store, &repo, "feature", &["a.py", "b.py"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `copy_file_rows` replaces the destination's graph rows (no duplicate
    /// on a second copy) and drops the resolution of the edges it carries.
    #[test]
    fn copying_a_file_leaves_no_orphans_or_duplicates() {
        let (dir, repo) = graph_repo("copy", &[("c.py", "def f():\n    g()\n    g()\n")]);
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        // As if the link pass had resolved one of the calls on main.
        let syms = store.file_symbols(&repo, "main", "c.py").unwrap();
        let mut edges = store.file_symbol_edges(&repo, "main", "c.py").unwrap();
        let call = edges.iter().position(|e| e.kind == "calls").unwrap();
        edges[call].dst_id = Some(syms[1].id);
        edges[call].confidence = Some("high".into());
        store
            .replace_file_graph(&repo, "main", "c.py", &syms, &edges)
            .unwrap();
        for _ in 0..2 {
            store.copy_file_rows(&repo, "main", "dev", "c.py").unwrap();
        }
        assert_eq!(store.file_symbols(&repo, "dev", "c.py").unwrap(), syms);
        let copied = store.file_symbol_edges(&repo, "dev", "c.py").unwrap();
        let (contains, calls): (Vec<_>, Vec<_>) = copied.iter().partition(|e| e.kind == "contains");
        assert_eq!(calls.len(), 2);
        assert!(calls
            .iter()
            .all(|e| e.dst_id.is_none() && e.confidence.is_none()));
        // Both ends of a `contains` are symbols of the file, the same on
        // every branch: it keeps its destination.
        assert_eq!(contains.len(), 1);
        assert_eq!(contains[0].dst_id, Some(syms[1].id));
        assert_no_orphans(&store, &repo, "dev", &["c.py"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file writes every relation it has, one row per occurrence
    /// (PLAN-009 TASK-004): calls, imports, supertypes, instantiations, type
    /// uses and containment, and its symbols carry their package,
    /// signature and visibility.
    /// The link pass of a full run (PLAN-009 TASK-005, DD-6): a call across
    /// files reaches its definition, an external one is marked, and the
    /// `contains` rows come out exactly as the parse wrote them (m-b: the
    /// link pass excludes `kind = 'contains'`).
    #[test]
    fn the_link_pass_resolves_across_files_and_leaves_contains_alone() {
        let (dir, repo) = graph_repo(
            "link",
            &[
                ("src/a/Caller.java", LINK_CALLER),
                ("src/b/Helper.java", LINK_HELPER),
            ],
        );
        let store = Store::open_in_memory(DIM).unwrap();
        let res = index_branch(&store, &dir, "main", true);
        let helper = store
            .file_symbols(&repo, "main", "src/b/Helper.java")
            .unwrap();
        let run = symbol(&helper, "Helper.run");
        let edges = store
            .file_symbol_edges(&repo, "main", "src/a/Caller.java")
            .unwrap();
        let call = edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_name == "Helper.run")
            .unwrap();
        assert_eq!(call.dst_id, Some(run.id));
        assert_eq!(call.confidence.as_deref(), Some("high"));
        assert_eq!(call.resolution.as_deref(), Some("field"));
        assert_eq!(call.external, Some(false));
        let log = edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_name == "Logger.info")
            .unwrap();
        assert_eq!((log.dst_id, log.external), (None, Some(true)));
        // `.count()` after the external `List.stream()`: nothing types it;
        // no repository name: a weak external.
        let count = edges.iter().find(|e| e.dst_name == "count").unwrap();
        assert_eq!(count.resolution.as_deref(), Some("chain_external"));
        assert_eq!(
            (count.external, count.confidence.as_deref()),
            (Some(true), Some("medium"))
        );
        // An untyped lambda parameter calling a name the repository lacks:
        // dropped — kept as `discarded`, out of the counts — and counted.
        let vanish = edges.iter().find(|e| e.dst_name == "vanish").unwrap();
        assert_eq!(vanish.resolution.as_deref(), Some("discarded"));
        assert_eq!(res.edges_discarded, 1, "{res:?}");
        let contains: Vec<_> = edges.iter().filter(|e| e.kind == "contains").collect();
        assert!(!contains.is_empty());
        for c in contains {
            assert_eq!(c.resolution.as_deref(), Some("structural"), "{c:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// PLAN-009 TASK-010: a pass that completes ranks the branch — every
    /// symbol gets a rank and an `in_degree` (incoming non-test calls of
    /// `high`/`medium`), the most called function outranks one nobody calls,
    /// the same repository indexed twice ranks the same bit for bit, and an
    /// incremental run that rewrites a file's symbols ranks them again.
    #[test]
    fn the_link_pass_ranks_the_branch() {
        let mut files = vec![
            ("src/a/Caller.java", LINK_CALLER),
            ("src/b/Helper.java", LINK_HELPER),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("rank", &files);
        let ranks = |store: &Store| -> Vec<(u64, Option<u64>, Option<i32>)> {
            let mut v: Vec<_> = store
                .branch_symbols(&repo, "main")
                .unwrap()
                .into_iter()
                .map(|s| (s.id, s.rank.map(f64::to_bits), s.in_degree))
                .collect();
            v.sort();
            v
        };
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let all = store.branch_symbols(&repo, "main").unwrap();
        assert!(all
            .iter()
            .all(|s| s.rank.is_some() && s.in_degree.is_some()));
        let helper = store
            .file_symbols(&repo, "main", "src/b/Helper.java")
            .unwrap();
        let run = symbol(&helper, "Helper.run");
        assert_eq!(run.in_degree, Some(1));
        let f1 = symbol(&all, "f1");
        assert_eq!(f1.in_degree, Some(0));
        assert!(run.rank.unwrap() > f1.rank.unwrap(), "{run:?} {f1:?}");
        let other = Store::open_in_memory(DIM).unwrap();
        index_branch(&other, &dir, "main", true);
        assert_eq!(ranks(&store), ranks(&other));
        // An incremental run rewriting `Helper.java` ranks its symbols again.
        write(&dir, "src/b/Helper.java", &format!("{LINK_HELPER}\n"));
        commit_all(&dir, "touch");
        index_branch(&store, &dir, "main", false);
        let helper = store
            .file_symbols(&repo, "main", "src/b/Helper.java")
            .unwrap();
        assert!(helper.iter().all(|s| s.rank.is_some()), "{helper:?}");
        assert_eq!(symbol(&helper, "Helper.run").in_degree, Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review of TASK-010, MAJOR 4: Rust tests live in the file they test.
    /// A symbol inside `#[cfg(test)]`, a `mod tests` or a `#[test]` function
    /// is a test symbol, and its calls are `from_test`; the rest of the file
    /// is not.
    #[test]
    fn rust_inline_tests_are_test_symbols() {
        let src = "pub fn open() {}\n\npub fn run() {\n    open();\n}\n\n#[cfg(test)]\nmod checks {\n    use super::*;\n\n    fn helper() {\n        open();\n    }\n\n    #[test]\n    fn opens() {\n        helper();\n    }\n}\n\nmod tests {\n    fn other() {\n        super::open();\n    }\n}\n\n#[test]\nfn loose() {\n    open();\n}\n\n#[cfg(not(test))]\npub fn prod_only() {}\n";
        let (dir, repo) = graph_repo("rstests", &[("src/lib.rs", src)]);
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let syms = store.file_symbols(&repo, "main", "src/lib.rs").unwrap();
        let test_of = |q: &str| symbol(&syms, q).is_test;
        assert!(
            !test_of("open") && !test_of("run") && !test_of("prod_only"),
            "{syms:?}"
        );
        for q in ["checks.helper", "checks.opens", "tests.other", "loose"] {
            assert!(test_of(q), "{q}: {syms:?}");
        }
        let edges = store
            .file_symbol_edges(&repo, "main", "src/lib.rs")
            .unwrap();
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == "calls").collect();
        let run = symbol(&syms, "run").id;
        for e in &calls {
            assert_eq!(e.from_test, e.src_id != run, "{e:?}");
        }
        assert!(calls.len() >= 5, "{calls:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review of TASK-010, MINOR 5: a pass writes only the ranks that
    /// changed; a second full pass over the same graph writes none.
    #[test]
    fn a_pass_over_the_same_graph_writes_no_rank() {
        let mut files = vec![
            ("src/a/Caller.java", LINK_CALLER),
            ("src/b/Helper.java", LINK_HELPER),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("rankagain", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let pass = || {
            crate::link::link_branch(
                &store,
                &repo,
                "main",
                &std::collections::HashSet::new(),
                true,
                &devctx_parse::resolve::env::LinkEnv::default(),
                &|| false,
            )
            .unwrap()
        };
        let again = pass();
        assert_eq!(again.files_written, 0, "{again:?}");
        assert_eq!(again.ranks_written, 0, "{again:?}");
        assert!(again.rank_converged, "{again:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An index from before TASK-010 — no rank anywhere, linked under the
    /// previous `LINK_VERSION` — is ranked by the next run, with nothing to
    /// reindex: the new version relinks the branch, and the pass ranks it.
    #[test]
    fn an_index_from_before_ranks_gets_them_on_the_next_run() {
        let mut files = vec![
            ("src/a/Caller.java", LINK_CALLER),
            ("src/b/Helper.java", LINK_HELPER),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("rankold", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let repo_path = GitRepo::open(&dir)
            .unwrap()
            .root()
            .to_string_lossy()
            .into_owned();
        // What a 0.10 index before TASK-010 holds: no rank, no in-degree.
        let mut by_file: std::collections::BTreeMap<String, Vec<devctx_store::StoredSymbol>> =
            Default::default();
        for mut s in store.branch_symbols(&repo, "main").unwrap() {
            (s.rank, s.in_degree) = (None, None);
            by_file.entry(s.file.clone()).or_default().push(s);
        }
        for (file, syms) in &by_file {
            store
                .replace_file_symbols(&repo, "main", file, syms)
                .unwrap();
        }
        assert!(store
            .branch_symbols(&repo, "main")
            .unwrap()
            .iter()
            .all(|s| s.rank.is_none()));
        store
            .set_index_meta(&repo_path, "main", crate::link::LINK_VERSION_META_KEY, "20")
            .unwrap();
        let res = index_branch(&store, &dir, "main", false);
        assert_eq!(res.files_indexed, 0, "{res:?}");
        let all = store.branch_symbols(&repo, "main").unwrap();
        assert!(
            all.iter()
                .all(|s| s.rank.is_some() && s.in_degree.is_some()),
            "{all:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const LINK_CALLER: &str = "\
package a;

import b.Helper;
import java.util.List;
import org.jboss.logging.Logger;

public class Caller {
    private static final Logger LOG = Logger.getLogger(Caller.class);
    Helper helper;

    long go(List<String> xs) {
        helper.run();
        LOG.info(\"x\");
        xs.forEach(x -> x.vanish());
        return xs.stream().count();
    }
}
";

    /// Enough files that one written file stays under the full-pass
    /// threshold (a fifth of the branch): the incremental selection is what
    /// is tested, not the full pass.
    const LINK_FILLER: [(&str, &str); 6] = [
        ("f1.py", "def f1():\n    pass\n"),
        ("f2.py", "def f2():\n    pass\n"),
        ("f3.py", "def f3():\n    pass\n"),
        ("f4.py", "def f4():\n    pass\n"),
        ("f5.py", "def f5():\n    pass\n"),
        ("f6.py", "def f6():\n    pass\n"),
    ];

    const LINK_HELPER: &str = "\
package b;

public class Helper {
    public void run() {}
}
";

    /// The incremental link pass (DD-6 mode b, m-b): only `Helper.java`
    /// changes, yet the edge of `Caller.java` into it is re-resolved — its
    /// destination is gone (renamed), then a symbol named like it is back.
    /// `Caller.java` is not reindexed, and its `contains` rows are never
    /// touched by the link pass's rewrite of its edges.
    #[test]
    fn an_incremental_link_pass_follows_a_definition_that_moved() {
        let mut files = vec![
            ("src/a/Caller.java", LINK_CALLER),
            ("src/b/Helper.java", LINK_HELPER),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("linkinc", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let caller = |store: &Store| {
            store
                .file_symbol_edges(&repo, "main", "src/a/Caller.java")
                .unwrap()
        };
        let before = caller(&store);
        let contains = |edges: &[devctx_store::StoredSymbolEdge]| -> Vec<_> {
            edges
                .iter()
                .filter(|e| e.kind == "contains")
                .cloned()
                .collect()
        };
        let call = |edges: &[devctx_store::StoredSymbolEdge]| {
            edges
                .iter()
                .find(|e| e.kind == "calls" && e.dst_name == "Helper.run")
                .cloned()
                .unwrap()
        };
        assert!(call(&before).dst_id.is_some());

        // `run` renamed: the edge's destination no longer exists.
        write(
            &dir,
            "src/b/Helper.java",
            &LINK_HELPER.replace("run()", "walk()"),
        );
        commit_all(&dir, "rename run");
        let inc = index_branch(&store, &dir, "main", false);
        assert_eq!(inc.files_indexed, 1, "only Helper.java: {inc:?}");
        let after = caller(&store);
        let moved = call(&after);
        assert_eq!(moved.dst_id, None, "{moved:?}");
        assert_eq!(moved.confidence.as_deref(), Some("low"));
        assert_eq!(contains(&after), contains(&before), "contains untouched");

        // Back under its name (an added symbol whose name the edge ends in).
        write(
            &dir,
            "src/b/Helper.java",
            &LINK_HELPER.replace("run()", "run() {}\n    public void walk()"),
        );
        commit_all(&dir, "run is back");
        index_branch(&store, &dir, "main", false);
        let helper = store
            .file_symbols(&repo, "main", "src/b/Helper.java")
            .unwrap();
        let back = call(&caller(&store));
        assert_eq!(back.dst_id, Some(symbol(&helper, "Helper.run").id));
        assert_eq!(back.resolution.as_deref(), Some("field"));
        assert_eq!(contains(&caller(&store)), contains(&before));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every non-`contains` edge of the branch, in a comparable order.
    fn linked_rows(store: &Store, repo: &str) -> Vec<String> {
        let mut rows: Vec<String> = store
            .branch_linkable_edges(repo, "main")
            .unwrap()
            .iter()
            .map(|e| format!("{e:?}"))
            .collect();
        rows.sort();
        rows
    }

    /// Run the link pass over the whole branch, as a full run would, and
    /// answer whether it changed any row: an incremental pass must leave
    /// nothing for it to fix.
    fn full_link_changes(store: &Store, repo: &str, dir: &Path) -> Vec<(String, String)> {
        let before = linked_rows(store, repo);
        let files: Vec<String> = crate::git::GitRepo::open(dir)
            .unwrap()
            .changes(None)
            .unwrap()
            .iter()
            .map(|c| match c {
                crate::git::Change::Added(p)
                | crate::git::Change::Modified(p)
                | crate::git::Change::Deleted(p) => p.clone(),
                crate::git::Change::Renamed { to, .. } => to.clone(),
            })
            .collect();
        let env = crate::env::load(
            &|rel: &str| std::fs::read_to_string(dir.join(rel)).ok(),
            &files,
        )
        .env();
        crate::link::link_branch(
            store,
            repo,
            "main",
            &std::collections::HashSet::new(),
            true,
            &env,
            &|| false,
        )
        .unwrap();
        let after = linked_rows(store, repo);
        before
            .iter()
            .zip(&after)
            .filter(|(a, b)| a != b)
            .map(|(a, b)| (a.clone(), b.clone()))
            .chain((before.len() != after.len()).then(|| {
                (
                    format!("{} rows", before.len()),
                    format!("{} rows", after.len()),
                )
            }))
            .collect()
    }

    fn index_with(
        store: &Store,
        dir: &Path,
        full: bool,
        progress: Option<&dyn ProgressSink>,
    ) -> IndexResult {
        run(IndexRequest {
            store,
            embedder: &FakeEmbedder,
            repo_root: dir,
            incremental: !full,
            model_name: "minilm-l6",
            progress,
            paths: None,
            exclude: &[],
            hnsw: None,
            embed_fingerprint: "test-fp",
            branch: Some("main"),
        })
        .unwrap()
    }

    /// Review of TASK-017, point 8: scenario (e) through `impact_graph`, not
    /// only the link pass — a class that changes its `implements` in an
    /// incremental run is reached through the new interface and no longer
    /// through the old one, exactly as a full index of the same tree answers.
    #[test]
    fn dispatch_follows_an_incremental_implements_change() {
        let files = [
            (
                "p/IFirst.java",
                "package p;\npublic interface IFirst { void run(String id); }\n",
            ),
            (
                "p/ISecond.java",
                "package p;\npublic interface ISecond { void run(String id); }\n",
            ),
            (
                "p/Worker.java",
                "package p;\npublic class Worker implements IFirst {\n    public void run(String id) {}\n}\n",
            ),
            (
                "p/UsesFirst.java",
                "package p;\npublic class UsesFirst {\n    void go(IFirst f) {\n        f.run(\"a\");\n    }\n}\n",
            ),
            (
                "p/UsesSecond.java",
                "package p;\npublic class UsesSecond {\n    void go(ISecond s) {\n        s.run(\"b\");\n    }\n}\n",
            ),
        ];
        let callers = |store: &Store, repo: &str| -> Vec<(String, String)> {
            let im = store
                .impact_graph(
                    repo,
                    "main",
                    "Worker.run",
                    None,
                    &devctx_store::ImpactOptions::default(),
                )
                .unwrap();
            im.upstream
                .nodes
                .iter()
                .map(|n| (n.symbol.clone(), n.via.clone()))
                .collect()
        };
        let (dir, repo) = graph_repo("dispatch_impl_change", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_with(&store, &dir, true, None);
        assert_eq!(
            callers(&store, &repo),
            [("UsesFirst.go".to_string(), "dispatch".to_string())]
        );
        write(
            &dir,
            "p/Worker.java",
            "package p;\npublic class Worker implements ISecond {\n    public void run(String id) {}\n}\n",
        );
        commit_all(&dir, "p/Worker.java");
        let inc = index_with(&store, &dir, false, None);
        assert_eq!(inc.files_indexed, 1, "{inc:?}");
        let after = callers(&store, &repo);
        assert_eq!(
            after,
            [("UsesSecond.go".to_string(), "dispatch".to_string())]
        );
        // A full index of the same tree answers the same.
        let fresh = Store::open_in_memory(DIM).unwrap();
        index_with(&fresh, &dir, true, None);
        assert_eq!(callers(&fresh, &repo), after);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The incremental link pass leaves the branch exactly as a full one
    /// would (TASK-005 review, MAJOR 4): after a supertype changes, a return
    /// type changes, `@Data` is added and a package moves — each a write of
    /// one file whose effect reaches edges of files not written.
    #[test]
    fn an_incremental_link_pass_equals_a_full_one() {
        let mut files = vec![
            (
                "p/A.java",
                "package p;\npublic class A { public void m() {} }\n",
            ),
            (
                "p/C.java",
                "package p;\npublic class C { public void m() {} }\n",
            ),
            ("p/B.java", "package p;\npublic class B extends A {}\n"),
            (
                "p/X.java",
                "package p;\npublic class X { public void doIt() {} }\n",
            ),
            (
                "p/Y.java",
                "package p;\npublic class Y { public void doIt() {} }\n",
            ),
            (
                "p/R.java",
                "package p;\npublic class R { public X find() { return null; } }\n",
            ),
            (
                "p/D.java",
                "package p;\n@lombok.Data\npublic class D { private String name; }\n",
            ),
            (
                "p/H.java",
                "package p;\npublic class H { public X item; }\n",
            ),
            // Only the inheritance rule reaches `m()` in `B2`: its hint is
            // `bare`, its destination still exists, and `B.java` defines no
            // `m`.
            (
                "p/B2.java",
                "package p;\npublic class B2 extends B {\n    void go() { m(); }\n}\n",
            ),
            // Only the callee token `name` (a word a hint also uses) reaches
            // `doIt` after `name()`, a static import from `Util.java`.
            (
                "p/Util.java",
                "package p;\npublic class Util { public static X name() { return null; } }\n",
            ),
            (
                "q/V.java",
                "package q;\nimport static p.Util.name;\nclass V {\n    void go() {\n        \
                 name().doIt();\n    }\n}\n",
            ),
            (
                "q/U.java",
                "package q;\nimport p.B;\nimport p.R;\nimport p.D;\nimport p.H;\n\
                 class U {\n    void go(B b, R r, D d, H h) {\n        b.m();\n        \
                 r.find().doIt();\n        d.getName();\n        h.item.doIt();\n    }\n}\n",
            ),
        ];
        // TASK-017: what a class `implements` decides which interface's
        // default method a caller of another file reaches (and where
        // dispatch climbs from).
        files.extend([
            (
                "p/I1.java",
                "package p;\npublic interface I1 { default void helper() {} }\n",
            ),
            (
                "p/I2.java",
                "package p;\npublic interface I2 { default void helper() {} }\n",
            ),
            (
                "p/K.java",
                "package p;\npublic class K implements I1 { public void work() {} }\n",
            ),
            (
                "q/W.java",
                "package q;\nimport p.K;\nclass W {\n    void go(K k) {\n        k.helper();\n    }\n}\n",
            ),
        ]);
        // TypeScript (TASK-006): a barrel's named re-export, an alias of the
        // `tsconfig`, a barrel re-exporting through another barrel.
        files.extend([
            ("web/libs/a/svc.ts", TS_SVC),
            ("web/libs/b/svc.ts", TS_SVC),
            (
                "web/libs/barrel/index.ts",
                "export { Svc } from '../a/svc';\n",
            ),
            (
                "web/app/c.ts",
                "import { Svc } from '../libs/barrel';\nexport class C {\n  \
                 constructor(private s: Svc) {}\n  go(): void {\n    this.s.run();\n  }\n}\n",
            ),
            ("tsconfig.base.json", TS_CONFIG_A),
            (
                "web/app/d.ts",
                "import { Svc } from '@acme/svc';\nexport function go(s: Svc): void {\n  \
                 s.run();\n}\n",
            ),
            ("web/libs/deep/index.ts", "export * from './inner';\n"),
            (
                "web/libs/deep/inner.ts",
                "export { Svc } from '../a/svc';\n",
            ),
            // TASK-006 review: a package import whose name a written file
            // starts to define (M3), and a chain typed by an importer's
            // return type (m1).
            (
                "package.json",
                "{ \"dependencies\": { \"rxjs\": \"7\" } }\n",
            ),
            (
                "web/m3/f.ts",
                "import { map } from 'rxjs';\nexport function useMap(): void {\n  map();\n}\n",
            ),
            ("web/m3/util.ts", "export function other(): void {}\n"),
            ("web/m2/x.ts", TS_FOO),
            ("web/m2/y.ts", TS_FOO),
            ("web/m2/w.ts", "export { Foo } from './x';\n"),
            (
                "web/m2/a.ts",
                "import { Foo } from './w';\nexport class A {\n  pipe(): Foo {\n    \
                 return new Foo();\n  }\n}\n",
            ),
            (
                "web/m2/h.ts",
                "import { A } from './a';\nexport function get(): A {\n  return new A();\n}\n",
            ),
            (
                "web/m2/c.ts",
                "import { get } from './h';\nexport function go(): void {\n  get().pipe().g();\n}\n",
            ),
            ("web/m1/x.ts", TS_FOO),
            ("web/m1/y.ts", TS_FOO),
            ("web/m1/w.ts", "export { Foo } from './x';\n"),
            (
                "web/m1/a.ts",
                "import { Foo } from './w';\nexport class A {\n  f(): Foo {\n    \
                 return new Foo();\n  }\n}\n",
            ),
            (
                "web/m1/c.ts",
                "import { A } from './a';\nexport function go(a: A): void {\n  a.f().g();\n}\n",
            ),
            (
                "web/app/e.ts",
                "import { Svc } from '../libs/deep';\nexport function go(s: Svc): void {\n  \
                 s.run();\n}\n",
            ),
        ]);
        // Python (TASK-007): a package `__init__.py` re-exporting a class,
        // and a module from a distribution the manifest comes to declare.
        files.extend([
            ("py/pkg/__init__.py", "from .a import Svc\n"),
            ("py/pkg/a.py", PY_SVC),
            ("py/pkg/b.py", PY_SVC),
            (
                "py/app.py",
                "from pkg import Svc\n\n\ndef go():\n    Svc().run()\n",
            ),
            ("requirements.txt", "httpx\n"),
            // Review P2: `import pkg.sub` binds `pkg`; a script beside a
            // directory that becomes a package.
            ("pp/pkg/__init__.py", "from .inner.x import f\n"),
            ("pp/pkg/inner/x.py", "def f():\n    pass\n"),
            ("pp/pkg/inner/y.py", "def f():\n    pass\n"),
            ("pp/pkg/sub.py", "def s():\n    pass\n"),
            ("pp/use.py", "import pkg.sub\n\n\ndef go():\n    pkg.f()\n"),
            ("sc/helper.py", "def assist():\n    pass\n"),
            ("sc/run.py", "import helper\n\nhelper.assist()\n"),
            (
                "py/net.py",
                "import requests\n\n\ndef go():\n    requests.get(\"u\")\n",
            ),
        ]);
        // Rust (TASK-007): a `pub use` re-export of the crate root, and a
        // crate whose `Cargo.toml` comes to make it the workspace's.
        files.extend([
            ("rs/Cargo.toml", "[package]\nname = \"rlib\"\n"),
            (
                "rs/src/lib.rs",
                "pub mod a;\npub mod b;\npub mod c;\npub mod e;\npub mod f;\npub mod g;\npub mod h;\npub mod ops2;\npub mod ops3;\npub mod pre;\npub mod sub;\npub mod util2;\npub use a::Svc;\n",
            ),
            // A method in an `impl` of another file; a glob of a prelude
            // that re-exports a type from `std`.
            (
                "rs/src/ops2.rs",
                "use crate::a::Svc;\n\nimpl Svc {\n    pub fn save(&self) {}\n}\n",
            ),
            (
                "rs/src/e.rs",
                "use crate::a::Svc;\n\npub fn go(s: Svc) {\n    s.save();\n}\n",
            ),
            ("rs/src/pre.rs", "pub use std::collections::HashMap as Map;\n"),
            // Second review: a missing derivable (external, `medium`) until an
            // `impl` of another file defines it; a function re-exported from
            // `std` until the module defines its own; a `const` of an
            // importer typed by a re-exported type.
            ("rs/src/ops3.rs", "use crate::a::Svc;\n"),
            (
                "rs/src/g.rs",
                "use crate::a::Svc;\n\npub fn go() {\n    Svc::default();\n}\n",
            ),
            ("rs/src/util2.rs", "pub use std::mem::take;\n"),
            (
                "rs/src/h.rs",
                "pub fn go(v: &mut i32) {\n    crate::util2::take(v);\n}\n",
            ),
            ("rs/src/sub/mod.rs", "pub mod k;\npub mod m;\n"),
            (
                "rs/src/sub/k.rs",
                "use crate::Svc;\n\npub const C: Svc = Svc;\n",
            ),
            (
                "rs/src/sub/m.rs",
                "use super::k::C;\n\npub fn go() {\n    C.run();\n}\n",
            ),
            (
                "rs/src/f.rs",
                "use crate::pre::*;\n\npub fn go() {\n    Map::new();\n}\n",
            ),
            ("rs/src/a.rs", RS_SVC),
            ("rs/src/b.rs", RS_SVC),
            (
                "rs/src/c.rs",
                "use crate::Svc;\n\npub fn go(s: Svc) {\n    s.run();\n}\n",
            ),
            (
                "rw/app/Cargo.toml",
                "[package]\nname = \"rapp\"\n[dependencies]\nralpha = { path = \"../alpha\" }\n",
            ),
            (
                "rw/app/src/main.rs",
                "use ralpha::Thing;\n\nfn main() {\n    Thing::go();\n}\n",
            ),
            (
                "rw/alpha/src/lib.rs",
                "pub struct Thing;\n\nimpl Thing {\n    pub fn go() {}\n}\n",
            ),
        ]);
        // Go (TASK-007): the `module` of `go.mod` decides which imports are
        // the repository's.
        files.extend([
            ("gox/go.mod", "module example.com/m\n"),
            (
                "gox/store/store.go",
                "package store\n\nfunc New() int {\n\treturn 1\n}\n",
            ),
            (
                "gox/app/app.go",
                "package app\n\nimport \"example.com/m/store\"\n\nfunc Go() {\n\tstore.New()\n}\n",
            ),
        ]);
        files.extend(LINK_FILLER);
        files.extend([
            ("g1.py", "def g1():\n    pass\n"),
            ("g2.py", "def g2():\n    pass\n"),
            ("g3.py", "def g3():\n    pass\n"),
            ("g4.py", "def g4():\n    pass\n"),
        ]);
        // Each step: the file written, its new text, and the edge of a file
        // not written whose answer must change — so no scenario passes by
        // changing nothing.
        let steps: [(&str, &str, &str, &str, i32); 25] = [
            (
                "p/B.java",
                "package p;\npublic class B extends C {}\n",
                "q/U.java",
                "B.m",
                8,
            ),
            (
                "p/R.java",
                "package p;\npublic class R { public Y find() { return null; } }\n",
                "q/U.java",
                "doIt",
                9,
            ),
            // `@lombok.Data` removed: the getter's field (medium) becomes
            // undecided — a resolved row the mode (c) does not pick.
            (
                "p/D.java",
                "package p;\npublic class D { private String name; }\n",
                "q/U.java",
                "D.getName",
                10,
            ),
            // The type of a member receiver's field changes.
            (
                "p/H.java",
                "package p;\npublic class H { public Y item; }\n",
                "q/U.java",
                "doIt",
                11,
            ),
            // TASK-017 (e): a class's `implements` changes: a call of another
            // file to an inherited default method follows it.
            (
                "p/K.java",
                "package p;\npublic class K implements I2 { public void work() {} }\n",
                "q/W.java",
                "K.helper",
                5,
            ),
            // A supertype's `extends` changes: a subtype's bare call follows.
            (
                "p/B.java",
                "package p;\npublic class B extends C {}\n",
                "p/B2.java",
                "m",
                3,
            ),
            // The return type of a statically imported `name()` changes.
            (
                "p/Util.java",
                "package p;\npublic class Util { public static Y name() { return null; } }\n",
                "q/V.java",
                "doIt",
                5,
            ),
            // A barrel re-exports `Svc` from the other library: only the
            // importers rule reaches `c.ts` (the barrel defines no name).
            (
                "web/libs/barrel/index.ts",
                "export { Svc } from '../b/svc';\n",
                "web/app/c.ts",
                "Svc.run",
                5,
            ),
            // The alias moves: no source file changes, only the config rule
            // (a full pass under another `tsconfig`) reaches `d.ts`.
            (
                "tsconfig.base.json",
                TS_CONFIG_B,
                "web/app/d.ts",
                "Svc.run",
                3,
            ),
            // The inner barrel of a barrel moves: the importers rule, through
            // the outer barrel, reaches `e.ts`.
            (
                "web/libs/deep/inner.ts",
                "export { Svc } from '../b/svc';\n",
                "web/app/e.ts",
                "Svc.run",
                3,
            ),
            // The barrel behind `A.f()`'s return type moves: `c.ts` imports
            // only `A` (an importer of the barrel), and only an importer's
            // names (`f`) reach its `a.f().g()`.
            (
                "web/m1/w.ts",
                "export { Foo } from './y';\n",
                "web/m1/c.ts",
                "g",
                3,
            ),
            // A package `__init__.py` re-exports `Svc` from the other module:
            // it defines no name, so only the importers rule reaches `app.py`.
            (
                "py/pkg/__init__.py",
                "from .b import Svc\n",
                "py/app.py",
                "run",
                5,
            ),
            // The manifest declares `requests`: no source changes, only the
            // environment rule (a full pass) makes `requests.get` external.
            (
                "requirements.txt",
                "httpx\nrequests\n",
                "py/net.py",
                "get",
                5,
            ),
            // `pkg/__init__.py` re-exports `f` from the other module: only
            // `pkg/__init__.py` in the reach of `import pkg.sub` reaches
            // `use.py`.
            (
                "pp/pkg/__init__.py",
                "from .inner.y import f\n",
                "pp/use.py",
                "f",
                5,
            ),
            // `sc/` becomes a package: the script's own directory is no
            // root any more, and only the rule that a written
            // `__init__.py` reopens its directory reaches `run.py`.
            ("sc/__init__.py", "X = 1\n", "sc/run.py", "assist", 3),
            // The crate root re-exports `Svc` from the other module: it
            // defines no `Svc`, so only the importers rule (a `use` of the
            // crate root) reaches `c.rs`.
            (
                "rs/src/lib.rs",
                "pub mod a;\npub mod b;\npub mod c;\npub mod e;\npub mod f;\npub mod g;\npub mod h;\npub mod ops2;\npub mod ops3;\npub mod pre;\npub mod sub;\npub mod util2;\npub use b::Svc;\n",
                "rs/src/c.rs",
                "Svc.run",
                4,
            ),
            // Review, performance: in a Rust importer only the rows that
            // name what its `use`s bind are reopened; the `use` row itself is
            // one of them.
            (
                "rs/src/lib.rs",
                "pub mod a;\npub mod b;\npub mod c;\npub mod e;\npub mod f;\npub mod g;\npub mod h;\npub mod ops2;\npub mod ops3;\npub mod pre;\npub mod sub;\npub mod util2;\npub use b::Svc;\n",
                "rs/src/c.rs",
                "crate::Svc",
                1,
            ),
            // Second review N1(a): `impl Default` in another file; the call
            // was external (`inherited` `medium`): only its name reaches it.
            (
                "rs/src/ops3.rs",
                "use crate::a::Svc;\n\nimpl Default for Svc {\n    fn default() -> Self {\n        Svc\n    }\n}\n",
                "rs/src/g.rs",
                "Svc.default",
                4,
            ),
            // N1(b): the module defines its own `take` instead of re-exporting
            // `std`'s: a path through a repository module, reopened by name.
            (
                "rs/src/util2.rs",
                "pub fn take(_v: &mut i32) -> i32 {\n    0\n}\n",
                "rs/src/h.rs",
                "crate::util2::take",
                2,
            ),
            // N2: an importer's `const` is typed by the re-exported type.
            (
                "rs/src/lib.rs",
                "pub mod a;\npub mod b;\npub mod c;\npub mod e;\npub mod f;\npub mod g;\npub mod h;\npub mod ops2;\npub mod ops3;\npub mod pre;\npub mod sub;\npub mod util2;\npub use b::Svc;\n",
                "rs/src/sub/m.rs",
                "C.run",
                4,
            ),
            // Review: an `impl` of another file loses `save`; `e.rs` has no
            // `use` of it, and only its gone destination reaches it.
            (
                "rs/src/ops2.rs",
                "use crate::a::Svc;\n\nimpl Svc {}\n",
                "rs/src/e.rs",
                "Svc.save",
                4,
            ),
            // Review, performance: an external path call is not reopened by
            // name; when the glob it reaches it through re-exports a
            // repository type instead, the importers rule still does.
            (
                "rs/src/pre.rs",
                "pub use crate::a::Svc as Map;\n",
                "rs/src/f.rs",
                "Map.new",
                4,
            ),
            // Review: an importer's `pipe(): Foo` types a chain like any
            // other name with a declared return (the generic-name filter no
            // longer drops it).
            ("web/m2/w.ts", "export { Foo } from './y';\n", "web/m2/c.ts", "g", 3),
            // A `Cargo.toml` makes `ralpha` a crate of the workspace: no
            // source changes, only the environment rule resolves the path.
            (
                "rw/alpha/Cargo.toml",
                "[package]\nname = \"ralpha\"\n",
                "rw/app/src/main.rs",
                "Thing.go",
                4,
            ),
            // The module is renamed: no source changes, only the environment
            // rule makes the import another module's (external).
            (
                "gox/go.mod",
                "module example.com/other\n",
                "gox/app/app.go",
                "New",
                6,
            ),
        ];
        // Each scenario on a branch of its own, so every one is judged (and
        // reported) whatever the others do.
        let mut failures: Vec<String> = Vec::new();
        for (i, (file, text, seen_in, dst, line)) in steps.into_iter().enumerate() {
            let (dir, repo) = graph_repo(&format!("linkeq{i}"), &files);
            let store = Store::open_in_memory(DIM).unwrap();
            index_with(&store, &dir, true, None);
            assert!(full_link_changes(&store, &repo, &dir).is_empty());
            let watched = |store: &Store| {
                store
                    .file_symbol_edges(&repo, "main", seen_in)
                    .unwrap()
                    .into_iter()
                    .find(|e| {
                        (e.kind == "calls" || e.kind == "imports")
                            && e.dst_name == dst
                            && e.line == line
                    })
                    .unwrap_or_else(|| panic!("no {dst} at {seen_in}:{line}"))
            };
            let before = watched(&store);
            write(&dir, file, text);
            commit_all(&dir, file);
            let inc = index_with(&store, &dir, false, None);
            // A manifest may be no indexed file of its own (`.toml`, `.txt`).
            let unindexed = [".toml", ".txt", "go.mod", ".cfg"]
                .iter()
                .any(|e| file.ends_with(e));
            if !unindexed {
                assert_eq!(inc.files_indexed, 1, "{file}: {inc:?}");
            }
            let after = watched(&store);
            if before == after {
                failures.push(format!(
                    "{file}: the watched edge did not change: {after:?}"
                ));
            }
            let diff = full_link_changes(&store, &repo, &dir);
            if !diff.is_empty() {
                failures.push(format!("{file}: the incremental pass left {diff:#?}"));
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    const PY_SVC: &str = "class Svc:\n    def run(self):\n        pass\n";
    const RS_SVC: &str = "pub struct Svc;\n\nimpl Svc {\n    pub fn run(&self) {}\n}\n";
    const TS_FOO: &str = "export class Foo {\n  g(): void {}\n}\n";
    const TS_SVC: &str = "export class Svc {\n  run(): void {}\n}\n";
    const TS_CONFIG_A: &str =
        "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@acme/svc\": [\"web/libs/a/svc.ts\"] } } }\n";
    const TS_CONFIG_B: &str =
        "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@acme/svc\": [\"web/libs/b/svc.ts\"] } } }\n";

    /// TASK-006 review, M4: a `tsconfig.base.json` left unreadable (a merge
    /// conflict) relinks nothing and loses no alias; a readable one again is
    /// a changed environment, relinked in full.
    #[test]
    fn an_unreadable_tsconfig_keeps_the_last_good_one() {
        let mut files = vec![
            ("web/libs/a/svc.ts", TS_SVC),
            ("web/libs/b/svc.ts", TS_SVC),
            ("tsconfig.base.json", TS_CONFIG_A),
            (
                "web/app/d.ts",
                "import { Svc } from '@acme/svc';\nexport function go(s: Svc): void {\n  \
                 s.run();\n}\n",
            ),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("linkenv", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_with(&store, &dir, true, None);
        let watched = |store: &Store| {
            store
                .file_symbol_edges(&repo, "main", "web/app/d.ts")
                .unwrap()
                .into_iter()
                .find(|e| e.kind == "calls" && e.dst_name == "Svc.run")
                .unwrap()
        };
        let good = watched(&store);
        assert_eq!(good.confidence.as_deref(), Some("high"), "{good:?}");
        write(
            &dir,
            "tsconfig.base.json",
            &format!("<<<<<<< HEAD\n{TS_CONFIG_A}=======\n{TS_CONFIG_B}>>>>>>> other\n"),
        );
        commit_all(&dir, "conflict");
        let inc = index_with(&store, &dir, false, None);
        assert_eq!(inc.edges_linked, 0, "no relink: {inc:?}");
        assert_eq!(watched(&store), good, "the alias is kept");
        write(&dir, "tsconfig.base.json", TS_CONFIG_B);
        commit_all(&dir, "resolved");
        index_with(&store, &dir, false, None);
        let moved = watched(&store);
        assert_ne!(
            moved.dst_id, good.dst_id,
            "the new alias relinks: {moved:?}"
        );
        assert_eq!(moved.confidence.as_deref(), Some("high"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TASK-006 second review, mi1: a `package.json` that is no JSON (an
    /// Nx generator's template) committed after a good index, together with
    /// a moved alias: the alias is seen, the broken manifest costs only
    /// itself.
    #[test]
    fn a_broken_manifest_does_not_freeze_the_aliases() {
        let mut files = vec![
            ("web/libs/a/svc.ts", TS_SVC),
            ("web/libs/b/svc.ts", TS_SVC),
            ("tsconfig.base.json", TS_CONFIG_A),
            (
                "web/app/d.ts",
                "import { Svc } from '@acme/svc';\nexport function go(s: Svc): void {\n  \
                 s.run();\n}\n",
            ),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("linkmanifest", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_with(&store, &dir, true, None);
        let watched = |store: &Store| {
            store
                .file_symbol_edges(&repo, "main", "web/app/d.ts")
                .unwrap()
                .into_iter()
                .find(|e| e.kind == "calls" && e.dst_name == "Svc.run")
                .unwrap()
        };
        let before = watched(&store);
        write(
            &dir,
            "tools/generators/x/files/package.json",
            "{ \"name\": \"<%= name %>\" ",
        );
        write(&dir, "tsconfig.base.json", TS_CONFIG_B);
        commit_all(&dir, "generator template and a moved alias");
        index_with(&store, &dir, false, None);
        let after = watched(&store);
        assert_ne!(after.dst_id, before.dst_id, "the moved alias: {after:?}");
        assert_eq!(after.confidence.as_deref(), Some("high"));
        assert!(full_link_changes(&store, &repo, &dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TASK-006 second review, mi2: a file that starts defining `map` does
    /// not change an import of `map` from a declared package (it stays
    /// external), and the incremental pass equals a full one.
    #[test]
    fn a_repository_homonym_does_not_unmake_a_package_import() {
        let mut files = vec![
            (
                "package.json",
                "{ \"dependencies\": { \"rxjs\": \"7\" } }\n",
            ),
            (
                "web/m3/f.ts",
                "import { map } from 'rxjs';\nexport function useMap(): void {\n  map();\n}\n",
            ),
            ("web/m3/util.ts", "export function other(): void {}\n"),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("linkhomonym", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_with(&store, &dir, true, None);
        write(
            &dir,
            "web/m3/util.ts",
            "export function other(): void {}\nexport function map(): void {}\n",
        );
        commit_all(&dir, "a homonym");
        index_with(&store, &dir, false, None);
        let rows = store
            .file_symbol_edges(&repo, "main", "web/m3/f.ts")
            .unwrap();
        for dst in ["rxjs#map", "map"] {
            let e = rows.iter().find(|e| e.dst_name == dst).unwrap();
            assert_eq!(
                (e.external, e.confidence.as_deref()),
                (Some(true), Some("high")),
                "{e:?}"
            );
        }
        assert!(full_link_changes(&store, &repo, &dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TASK-007: the `Cargo.toml`, `go.mod` and Python manifests are part of
    /// the link environment. One left unreadable (a merge conflict) relinks
    /// nothing; a changed one relinks the branch in full, though no source
    /// file changed.
    #[test]
    fn a_changed_manifest_relinks_and_an_unreadable_one_does_not() {
        let mut files = vec![
            ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("go.mod", "module example.com/m\n"),
            ("requirements.txt", "requests\n"),
            ("caller.py", "def go():\n    print(1)\n"),
        ];
        files.extend(LINK_FILLER);
        let (dir, _repo) = graph_repo("linkcargo", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_with(&store, &dir, true, None);
        for (file, broken) in [
            ("crates/a/Cargo.toml", "[package]\n<<<<<<< HEAD\n"),
            ("go.mod", "<<<<<<< HEAD\nmodule x\n"),
            ("requirements.txt", "<<<<<<< HEAD\nrequests\n"),
        ] {
            write(&dir, file, broken);
            commit_all(&dir, "conflict");
            let inc = index_with(&store, &dir, false, None);
            assert_eq!(inc.edges_linked, 0, "{file}: no relink: {inc:?}");
        }
        for (file, text) in [
            (
                "crates/a/Cargo.toml",
                "[package]\nname = \"a\"\n[dependencies]\nserde = \"1\"\n",
            ),
            ("go.mod", "module example.com/renamed\n"),
            ("requirements.txt", "requests\nhttpx\n"),
        ] {
            write(&dir, file, text);
            commit_all(&dir, file);
            let inc = index_with(&store, &dir, false, None);
            assert!(inc.edges_linked > 0, "{file}: a full relink: {inc:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A link pass cut short (a stop, a crash) is owed: the next run does it
    /// in full, though its files are unchanged and it writes none (MAJOR 5).
    #[test]
    fn a_link_pass_cut_short_is_done_by_the_next_run() {
        let mut files = vec![
            ("src/a/Caller.java", LINK_CALLER),
            ("src/b/Helper.java", LINK_HELPER),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("linkcut", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_with(&store, &dir, true, None);
        write(
            &dir,
            "src/a/Caller.java",
            &LINK_CALLER.replace("go(", "walk("),
        );
        commit_all(&dir, "rename go");
        let stop = CancelAt::new(Some("link"), usize::MAX);
        let cut = index_with(&store, &dir, false, Some(&stop));
        assert!(cut.cancelled, "{cut:?}");
        let unlinked = |store: &Store| {
            store
                .file_symbol_edges(&repo, "main", "src/a/Caller.java")
                .unwrap()
                .into_iter()
                .filter(|e| e.kind != "contains" && e.resolution.is_none())
                .count()
        };
        assert!(unlinked(&store) > 0, "the cut left the file unlinked");
        let next = index_with(&store, &dir, false, None);
        assert_eq!(next.files_indexed, 0, "nothing to reindex: {next:?}");
        assert_eq!(unlinked(&store), 0, "the owed link pass did not run");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A build whose link rules differ from the ones that linked the branch
    /// relinks it in full on its next run, changed files or not (MAJOR 6).
    #[test]
    fn a_new_link_version_relinks_the_branch() {
        let mut files = vec![
            ("src/a/Caller.java", LINK_CALLER),
            ("src/b/Helper.java", LINK_HELPER),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("linkver", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_with(&store, &dir, true, None);
        let good = store
            .file_symbol_edges(&repo, "main", "src/a/Caller.java")
            .unwrap();
        // What an older rule wrote: resolved, so no incremental selection
        // would pick it again.
        let stale: Vec<_> = good
            .iter()
            .filter(|e| e.kind != "contains")
            .map(|e| devctx_store::StoredSymbolEdge {
                resolution: Some("unique_name".into()),
                confidence: Some("medium".into()),
                dst_id: Some(e.src_id),
                external: Some(false),
                ..e.clone()
            })
            .collect();
        store
            .replace_files_linked_edges(&repo, "main", &[("src/a/Caller.java", stale)])
            .unwrap();
        let repo_path = repo_path_of(&dir);
        store
            .set_index_meta(&repo_path, "main", crate::link::LINK_VERSION_META_KEY, "0")
            .unwrap();
        index_with(&store, &dir, false, None);
        assert_eq!(
            store
                .file_symbol_edges(&repo, "main", "src/a/Caller.java")
                .unwrap(),
            good
        );
        assert_eq!(
            store
                .get_index_meta(&repo_path, "main", crate::link::LINK_VERSION_META_KEY)
                .unwrap()
                .as_deref(),
            Some(crate::link::LINK_VERSION)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dropped call is kept, marked `discarded` (MAJOR 7): when a written
    /// file defines its name, the incremental pass reopens it by name.
    #[test]
    fn a_discarded_call_is_kept_and_reopens_by_name() {
        let mut files = vec![
            ("src/a/Caller.java", LINK_CALLER),
            ("src/b/Helper.java", LINK_HELPER),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("linkdisc", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_with(&store, &dir, true, None);
        let vanish = |store: &Store| {
            store
                .file_symbol_edges(&repo, "main", "src/a/Caller.java")
                .unwrap()
                .into_iter()
                .find(|e| e.dst_name == "vanish")
                .expect("the dropped call is kept")
        };
        let first = vanish(&store);
        assert_eq!(first.resolution.as_deref(), Some("discarded"));
        assert_eq!((first.dst_id, first.external), (None, Some(false)));
        write(
            &dir,
            "src/b/Ghost.java",
            "package b;\npublic class Ghost { public void vanish() {} }\n",
        );
        commit_all(&dir, "a vanish");
        index_with(&store, &dir, false, None);
        assert_eq!(vanish(&store).resolution.as_deref(), Some("name_only"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rows the incremental pass re-resolves on its own, whatever was
    /// written: the undecided ones (mode c), never the discarded.
    fn undecided_live(store: &Store, repo: &str) -> usize {
        store
            .branch_linkable_edges(repo, "main")
            .unwrap()
            .iter()
            .filter(|e| {
                e.dst_id.is_none()
                    && !e.external.unwrap_or(false)
                    && e.resolution.as_deref() != Some(crate::link::DISCARDED)
            })
            .count()
    }

    /// Discarded rows are reopened by name, not by mode (c): a run that
    /// writes an unrelated file leaves them alone (follow-up 4).
    #[test]
    fn discarded_rows_wait_for_their_name() {
        let mut files = vec![
            ("src/a/Caller.java", LINK_CALLER),
            ("src/b/Helper.java", LINK_HELPER),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("linkwait", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_with(&store, &dir, true, None);
        assert_eq!(store.branch_discarded_calls(&repo, "main").unwrap(), 1);
        let expected = undecided_live(&store, &repo);
        write(&dir, "f1.py", "def f1():\n    return 1\n");
        commit_all(&dir, "unrelated");
        let inc = index_with(&store, &dir, false, None);
        assert_eq!(inc.files_indexed, 1, "{inc:?}");
        assert_eq!(inc.edges_linked, expected, "{inc:?}");
        assert_eq!(inc.edges_discarded, 1, "the branch's total: {inc:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The words of a hint (`typed`, `field`, `name`…) are no names: a
    /// written file defining a symbol `field` reopens nothing by them
    /// (follow-up 2).
    #[test]
    fn hint_keywords_do_not_reopen_edges() {
        let mut files = vec![
            ("src/a/Caller.java", LINK_CALLER),
            ("src/b/Helper.java", LINK_HELPER),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("linkkw", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_with(&store, &dir, true, None);
        let expected = undecided_live(&store, &repo);
        write(&dir, "k.py", "field = 1\nname = 2\ntyped = 3\n");
        commit_all(&dir, "keywords as names");
        let inc = index_with(&store, &dir, false, None);
        assert_eq!(inc.files_indexed, 1, "{inc:?}");
        assert_eq!(inc.edges_linked, expected, "{inc:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A call dropped for its arity (an untyped receiver, and no method of
    /// that name takes those arguments) is reopened when a written file adds
    /// the overload that does.
    #[test]
    fn a_call_discarded_for_its_arity_reopens_with_the_overload() {
        let caller = "package a;\nimport java.util.List;\npublic class Caller {\n    \
                      void go(List<String> xs) {\n        xs.forEach(x -> x.vanish(1, 2));\n    }\n}\n";
        let mut files = vec![
            ("src/a/Caller.java", caller),
            (
                "src/b/Ghost.java",
                "package b;\npublic class Ghost { public void vanish() {} }\n",
            ),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("linkarity", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_with(&store, &dir, true, None);
        let vanish = |store: &Store| {
            store
                .file_symbol_edges(&repo, "main", "src/a/Caller.java")
                .unwrap()
                .into_iter()
                .find(|e| e.dst_name == "vanish")
                .unwrap()
        };
        assert_eq!(vanish(&store).resolution.as_deref(), Some("discarded"));
        write(
            &dir,
            "src/b/Ghost.java",
            "package b;\npublic class Ghost {\n    public void vanish() {}\n    \
             public void vanish(int a, int b) {}\n}\n",
        );
        commit_all(&dir, "the overload");
        index_with(&store, &dir, false, None);
        assert_eq!(vanish(&store).resolution.as_deref(), Some("name_only"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is no source (a README) is no graph write: it does not
    /// wake the link pass (MINOR).
    #[test]
    fn a_non_graph_file_does_not_wake_the_link_pass() {
        let mut files = vec![
            ("src/a/Caller.java", LINK_CALLER),
            ("src/b/Helper.java", LINK_HELPER),
        ];
        files.extend(LINK_FILLER);
        let (dir, _repo) = graph_repo("linkmd", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        let full = index_with(&store, &dir, true, None);
        assert!(full.edges_linked > 0, "{full:?}");
        write(&dir, "README.md", "# notes\n");
        commit_all(&dir, "docs");
        let docs = index_with(&store, &dir, false, None);
        assert_eq!(docs.files_indexed, 1, "{docs:?}");
        assert_eq!(
            docs.edges_linked, 0,
            "the link pass ran for a README: {docs:?}"
        );
        std::fs::remove_file(dir.join("README.md")).unwrap();
        commit_all(&dir, "no docs");
        let gone = index_with(&store, &dir, false, None);
        assert_eq!(gone.files_deleted, 1, "{gone:?}");
        assert_eq!(
            gone.edges_linked, 0,
            "the link pass ran for a deleted README: {gone:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DD-6 mode (b) by name: an edge that is resolved and points at a
    /// symbol that still exists is re-resolved when a written file adds a
    /// symbol named like its destination — here a second `utilFn`, which
    /// makes the unique name ambiguous. Only `c.ts` is indexed; `a.ts`'s
    /// row is neither dangling nor undecided, so nothing but the name picks
    /// it.
    #[test]
    fn an_added_homonym_reopens_a_resolved_edge_by_name() {
        // A free name of a script nothing imports (TypeScript keeps rule 7 for
        // it; Python no longer does, TASK-007: a name Python cannot see is
        // undecided).
        let mut files = vec![
            ("a.ts", "export function go(): void {\n  utilFn();\n}\n"),
            ("b.ts", "export function utilFn(): void {}\n"),
        ];
        files.extend(LINK_FILLER);
        let (dir, repo) = graph_repo("linkname", &files);
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let edge = |store: &Store| {
            store
                .file_symbol_edges(&repo, "main", "a.ts")
                .unwrap()
                .into_iter()
                .find(|e| e.kind == "calls" && e.dst_name == "utilFn")
                .unwrap()
        };
        let first = edge(&store);
        assert_eq!(first.resolution.as_deref(), Some("unique_name"));
        assert_eq!(first.confidence.as_deref(), Some("medium"));
        assert!(first.dst_id.is_some());

        write(&dir, "c.ts", "export function utilFn(): void {}\n");
        commit_all(&dir, "a homonym");
        let inc = index_branch(&store, &dir, "main", false);
        assert_eq!(inc.files_indexed, 1, "{inc:?}");
        let now = edge(&store);
        assert_eq!(now.resolution.as_deref(), Some("name_only"), "{now:?}");
        assert_eq!((now.dst_id, now.confidence.as_deref()), (None, Some("low")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_relation_of_a_file_is_an_edge() {
        let src = "\
package com.example;

import java.util.List;

public class A extends B implements C {
    private List<D> ds = new ArrayList<>();

    public A(D d) {
        helper();
    }

    void run() {
        helper();
    }
}
";
        let (dir, repo) = graph_repo("kinds", &[("src/A.java", src)]);
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let syms = store.file_symbols(&repo, "main", "src/A.java").unwrap();
        let edges = store
            .file_symbol_edges(&repo, "main", "src/A.java")
            .unwrap();
        let kinds: std::collections::BTreeSet<&str> =
            edges.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds.into_iter().collect::<Vec<_>>(),
            [
                "calls",
                "contains",
                "implements",
                "imports",
                "inherits",
                "instantiates",
                "references"
            ]
        );
        let file = symbol(&syms, "src/A.java");
        let class = symbol(&syms, "A");
        let ctor = symbol(&syms, "A.A");
        assert_eq!(ctor.kind, "constructor");
        assert_eq!(ctor.signature.as_deref(), Some("public A(D d)"));
        assert!(syms
            .iter()
            .all(|s| s.package.as_deref() == Some("com.example")));
        assert_eq!(class.exported, Some(true));
        assert_eq!(symbol(&syms, "A.run").exported, Some(false));
        let one = |kind: &str, dst: &str| {
            edges
                .iter()
                .find(|e| e.kind == kind && e.dst_name == dst)
                .unwrap_or_else(|| panic!("no {kind} {dst} in {edges:#?}"))
        };
        assert_eq!(one("imports", "java.util.List").src_id, file.id);
        assert_eq!(one("inherits", "B").src_id, class.id);
        assert_eq!(one("implements", "C").src_id, class.id);
        let field = symbol(&syms, "A.ds");
        assert_eq!(one("instantiates", "ArrayList").src_id, field.id);
        assert_eq!(one("references", "List").src_id, field.id);
        assert_eq!(one("references", "D").src_id, field.id);
        // The constructor is a symbol: its call and its parameter's type
        // come from it.
        assert!(edges
            .iter()
            .any(|e| e.kind == "calls" && e.dst_name == "helper" && e.src_id == ctor.id));
        assert!(edges
            .iter()
            .any(|e| e.kind == "references" && e.dst_name == "D" && e.src_id == ctor.id));
        // Containment is resolved in the file; nothing else is yet.
        let contains: Vec<_> = edges.iter().filter(|e| e.kind == "contains").collect();
        assert_eq!(
            contains.len(),
            syms.len() - 1,
            "one per symbol but the file's"
        );
        let top = one("contains", "A");
        assert_eq!((top.src_id, top.dst_id), (file.id, Some(class.id)));
        assert_eq!(one("contains", "A.run").src_id, class.id);
        // Intra-file only, and marked as such: `structural`, not DD-7's
        // `same_file` (rule 6), so gold-edge precision does not count it.
        let ids: std::collections::HashSet<u64> = syms.iter().map(|s| s.id).collect();
        for c in &contains {
            assert_eq!(c.resolution.as_deref(), Some("structural"), "{c:?}");
            assert!(ids.contains(&c.src_id) && c.dst_id.is_some_and(|d| ids.contains(&d)));
        }
        // Everything else went through the link pass (TASK-005): decided,
        // with a confidence, whatever the answer.
        assert!(edges
            .iter()
            .filter(|e| e.kind != "contains")
            .all(|e| e.confidence.is_some() && e.resolution.is_some()));
        assert_eq!(one("calls", "helper").confidence.as_deref(), Some("low"));
        assert_no_orphans(&store, &repo, "main", &["src/A.java"]);
        // `graph_edges` stays calls only, in its 0.9.0 shape.
        let old = store.graph_edges(&repo, "main", None, None, 0).unwrap();
        assert!(old.iter().all(|e| e.kind == "calls"), "{old:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A deleted file takes its symbols and edges with it.
    #[test]
    fn deleting_a_file_drops_its_graph_rows() {
        let (dir, repo) = graph_repo(
            "delete",
            &[
                ("a.py", "def a():\n    b()\n"),
                ("b.py", "def b():\n    a()\n"),
            ],
        );
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        // Two symbols per file; per file a call and a `contains`.
        assert_eq!(store.graph_row_counts(&repo, "main").unwrap(), (4, 4));
        std::fs::remove_file(dir.join("b.py")).unwrap();
        commit_all(&dir, "drop b");
        let r = index_branch(&store, &dir, "main", false);
        assert_eq!(r.files_deleted, 1, "{r:?}");
        assert!(store
            .file_symbols(&repo, "main", "b.py")
            .unwrap()
            .is_empty());
        assert!(store
            .file_symbol_edges(&repo, "main", "b.py")
            .unwrap()
            .is_empty());
        assert_eq!(store.graph_row_counts(&repo, "main").unwrap(), (2, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Dropping a branch takes its graph rows and nobody else's.
    #[test]
    fn dropping_a_branch_drops_its_graph_rows() {
        let dir = two_branch_repo("graph_drop");
        let repo = dir.file_name().unwrap().to_string_lossy().to_string();
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        index_branch(&store, &dir, "feature", true);
        let main = store.graph_row_counts(&repo, "main").unwrap();
        assert!(main.0 > 0);
        assert_eq!(store.graph_row_counts(&repo, "feature").unwrap(), main);
        store
            .drop_branch(&repo, &repo_path_of(&dir), "feature")
            .unwrap();
        assert_eq!(store.graph_row_counts(&repo, "feature").unwrap(), (0, 0));
        assert_eq!(store.graph_row_counts(&repo, "main").unwrap(), main);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An index stamped by extractor v1 (0.9.0) has no symbol graph: it reads
    /// stale, an incremental run does not hide that, and `--full` fills the
    /// tables and re-stamps it.
    #[test]
    fn a_v1_index_reads_stale_until_a_full_run_fills_the_graph() {
        let (dir, repo) = graph_repo("v1", &[("a.py", "def a():\n    b()\n")]);
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let repo_path = repo_path_of(&dir);
        // What 0.9.0 left: its own stamp, no rows in the new tables.
        store
            .set_index_meta(&repo_path, "main", "extractor", "v1-0123456789abcdef")
            .unwrap();
        store.delete_file_graph(&repo, "main", "a.py").unwrap();
        let now = extractor_fingerprint();
        let version = format!("v{}-", devctx_parse::EXTRACTOR_VERSION);
        assert!(now.starts_with(&version), "{now}");
        assert!(store
            .extractor_stale(&repo_short_of(&dir), &repo_path, "main", &now)
            .unwrap());

        write(&dir, "b.py", "def b():\n    pass\n");
        commit_all(&dir, "add b");
        let inc = index_branch(&store, &dir, "main", false);
        assert!(inc.extractor_stale, "an incremental run must not hide it");
        assert!(store
            .file_symbols(&repo, "main", "a.py")
            .unwrap()
            .is_empty());

        let full = index_branch(&store, &dir, "main", true);
        assert!(!full.extractor_stale);
        assert!(!store
            .extractor_stale(&repo_short_of(&dir), &repo_path, "main", &now)
            .unwrap());
        assert_eq!(store.file_symbols(&repo, "main", "a.py").unwrap().len(), 2);
        assert_eq!(
            store
                .file_symbol_edges(&repo, "main", "a.py")
                .unwrap()
                .iter()
                .filter(|e| e.kind == "calls")
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A downgraded 0.9.0 reads `graph_edges` with the queries it always had
    /// (the read side of `graph.rs` is unchanged since 0.9.0): the table must
    /// stay what that release wrote — qualified sources, one row per pair, no
    /// row without a source — next to the new tables it does not know.
    #[test]
    fn graph_edges_stays_readable_by_0_9_0() {
        let (dir, repo) = graph_repo(
            "compat",
            &[(
                "svc.py",
                "class Svc:\n    def run(self):\n        self.helper()\n        self.helper()\n\n    def helper(self):\n        pass\n\nSvc().run()\n",
            )],
        );
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let old = store.graph_edges(&repo, "main", None, None, 0).unwrap();
        assert_eq!(old.len(), 1, "{old:?}");
        assert_eq!(
            (old[0].source.as_str(), old[0].target.as_str()),
            ("Svc.run", "Svc.helper")
        );
        let refs = store.find_references(&repo, "main", "Svc.helper").unwrap();
        assert_eq!(refs.len(), 1);
        let impact = store
            .impact_analysis(&repo, "main", "Svc.helper", 2)
            .unwrap();
        assert_eq!(impact.upstream, vec![("Svc.run".to_string(), 1)]);
        // The new tables hold the occurrences 0.9.0 folded or dropped.
        let edges = store.file_symbol_edges(&repo, "main", "svc.py").unwrap();
        let calls = edges.iter().filter(|e| e.kind == "calls").count();
        assert_eq!(calls, 4, "{edges:?}"); // helper ×2, Svc, run at module level
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The 0.9.0 assert of [`graph_edges_stays_readable_by_0_9_0`] on
    /// `impact_analysis`, over the symbol graph of the same index (TASK-009):
    /// two calls from one method are one caller.
    #[test]
    fn impact_over_the_symbol_graph_keeps_the_0_9_0_answer() {
        let (dir, repo) = graph_repo(
            "compat_impact",
            &[(
                "svc.py",
                "class Svc:\n    def run(self):\n        self.helper()\n        self.helper()\n\n    def helper(self):\n        pass\n\nSvc().run()\n",
            )],
        );
        let store = Store::open_in_memory(DIM).unwrap();
        index_branch(&store, &dir, "main", true);
        let opts = devctx_store::ImpactOptions {
            depth: 2,
            ..Default::default()
        };
        let im = store
            .impact_graph(&repo, "main", "Svc.helper", None, &opts)
            .unwrap();
        let upstream: Vec<(String, usize)> = im
            .upstream
            .nodes
            .iter()
            .map(|n| (n.symbol.clone(), n.depth))
            .collect();
        // Depth 2 reaches the module-level `Svc().run()` too (its source is
        // the file symbol), which 0.9.0's `graph_edges` never held.
        assert_eq!(upstream[0], ("Svc.run".to_string(), 1));
        assert_eq!(upstream[1..], [("svc.py".to_string(), 2)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
