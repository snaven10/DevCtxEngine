//! `devctx-central` — the central store: one project registry plus the memory
//! that is worth carrying between repositories.
//!
//! Per-project databases stay exactly as they are: vectors, call graph, routes
//! and project-local memories. What lives here instead is the knowledge that has
//! no single owner — which repositories exist and where, and (from F2 on) the
//! globally-scoped memories any of them can contribute to and read back.
//!
//! Because this file is shared by every project, it must have a single writer.
//! `devctx serve --central` is that writer; short-lived commands may open it
//! directly while no daemon is running.

pub mod client;
pub mod config;
pub mod error;
pub mod paths;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use devctx_core::config::{Embeddings, Project, ProjectConfig};
use devctx_embed::{create_provider, EmbedSettings, EmbeddingProvider};
use devctx_memory::{RecallQuery, RecalledMemory, RememberRequest, RememberResult};
use devctx_store::{Memory, ProjectRecord, Store};

pub use client::{CentralClient, ServeInfo};
pub use config::{CentralConfig, Defaults};
pub use error::{CentralError, Result};
pub use paths::{CentralPaths, HOME_ENV};

/// Inputs for registering a repository.
#[derive(Debug, Clone)]
pub struct RegisterRequest {
    /// Repository root (need not be canonical).
    pub root: PathBuf,
    /// Explicit project name; defaults to the config's name, then the directory.
    pub name: Option<String>,
    /// Free-text description, so an agent can pick a project without opening it.
    pub description: String,
    /// Comma-separated tags.
    pub tags: String,
    /// Write a `.devctx/config.yaml` from the central defaults when the
    /// repository has none, instead of refusing.
    pub create_config: bool,
    /// Caller-provided timestamp.
    pub now: String,
}

impl Default for RegisterRequest {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            name: None,
            description: String::new(),
            tags: String::new(),
            create_config: false,
            now: now_stamp(),
        }
    }
}

/// The central store: config, registry and (later) global memories.
pub struct Central {
    paths: CentralPaths,
    config: CentralConfig,
    store: Store,
    /// Built on first use. The daemon must start without paying a model load —
    /// registry work needs no embedder at all, and most sessions never touch a
    /// global memory.
    embedder: Mutex<Option<Arc<dyn EmbeddingProvider>>>,
}

impl Central {
    /// Open the central store at its resolved location, creating it if needed.
    pub fn open() -> Result<Self> {
        Self::open_with(CentralPaths::resolve()?)
    }

    /// Open a central store rooted at `dir` (config, database and discovery file
    /// together). This is what tests use to stay off the real user directories.
    pub fn open_in(dir: &Path) -> Result<Self> {
        Self::open_with(CentralPaths::rooted_at(dir))
    }

    fn open_with(paths: CentralPaths) -> Result<Self> {
        let config = CentralConfig::load_or_default(&paths.config)?;
        let dim = config.memory_dimension();
        let store = Store::open(&paths.db, dim)?;

        // Materialize the defaults on first run so the knobs — which memory model
        // the global vector space is pinned to, what new projects inherit — are
        // discoverable in a file rather than only in the docs. Best-effort: an
        // unwritable config directory is not a reason to refuse to work.
        if !paths.config.exists() {
            let _ = config.save(&paths.config);
        }

        // The schema is created with IF NOT EXISTS, so an existing database keeps
        // whatever width it was built with. Catch a changed `memory.model` here,
        // where the message can explain it, rather than at the first write.
        if let Some(found) = store.stored_dimension()? {
            if found != dim {
                return Err(CentralError::DimensionMismatch {
                    path: paths.db.clone(),
                    found,
                    expected: dim,
                });
            }
        }

        Ok(Self {
            paths,
            config,
            store,
            embedder: Mutex::new(None),
        })
    }

    /// Resolved on-disk locations.
    pub fn paths(&self) -> &CentralPaths {
        &self.paths
    }

    /// The central configuration.
    pub fn config(&self) -> &CentralConfig {
        &self.config
    }

    /// The underlying store (memories and vectors live here too).
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Settings for the embedder that owns the global vector space.
    pub fn memory_embed_settings(&self) -> EmbedSettings {
        // `model_dir` must be carried through: a user-defined ONNX model (e.g.
        // Granite) cannot be loaded without it, and dropping it here made the
        // central embedder depend on `DEVCTX_MODEL_DIR` being exported into
        // whatever process happened to auto-spawn the daemon.
        let model_dir = if self.config.memory.model_dir.is_empty() {
            self.config.defaults.embeddings.model_dir.clone()
        } else {
            self.config.memory.model_dir.clone()
        };
        EmbedSettings::from_config(&Embeddings {
            provider: self.config.memory.provider.clone(),
            model: self.config.memory.model.clone(),
            model_dir,
            ..Default::default()
        })
    }

    /// Whether a project's embeddings land in the same vector space as the
    /// central memory, in which case a caller can reuse the embedder it has
    /// already loaded instead of paying for a second one. This is the common
    /// case, and the reason global memory usually costs no extra memory.
    pub fn shares_vector_space(&self, e: &Embeddings) -> bool {
        e.provider == self.config.memory.provider && e.model == self.config.memory.model
    }

    /// The central memory embedder, built (and cached) on first use.
    pub fn embedder(&self) -> Result<Arc<dyn EmbeddingProvider>> {
        let mut guard = self.embedder.lock().expect("central embedder lock");
        if let Some(e) = guard.as_ref() {
            return Ok(e.clone());
        }
        let e: Arc<dyn EmbeddingProvider> =
            Arc::from(create_provider(&self.memory_embed_settings())?);
        *guard = Some(e.clone());
        Ok(e)
    }

    /// Store a globally-scoped memory, deduplicated across every repository.
    pub fn remember(&self, req: &RememberRequest) -> Result<RememberResult> {
        let embedder = self.embedder()?;
        let mut req = req.clone();
        // A memory reaching the central store is shared: either with one
        // group of repositories, or with everything. Anything else is
        // normalized to global.
        if !devctx_memory::is_group(&req.scope) || req.group.is_empty() {
            req.scope = devctx_memory::SCOPE_GLOBAL.to_string();
        }
        Ok(devctx_memory::remember(
            &self.store,
            embedder.as_ref(),
            &req,
        )?)
    }

    /// Recall globally-scoped memories, optionally narrowed to the repository
    /// that contributed them.
    pub fn recall(
        &self,
        query: &str,
        repo: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RecalledMemory>> {
        self.recall_in(devctx_memory::GLOBAL_PROJECT, query, repo, limit)
    }

    /// Recall from one reserved key of the central store: the global space, or
    /// a single group's (`@group:<name>`).
    pub fn recall_in(
        &self,
        project_key: &str,
        query: &str,
        repo: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RecalledMemory>> {
        let embedder = self.embedder()?;
        Ok(devctx_memory::recall(
            &self.store,
            embedder.as_ref(),
            &RecallQuery {
                query,
                project: Some(project_key),
                repo,
                limit,
            },
        )?)
    }

    /// The most recently updated global memories (no query).
    pub fn recent_memories(&self, limit: usize) -> Result<Vec<Memory>> {
        self.shared_memories(limit)
    }

    /// Live memories across every shared space — the global one and every
    /// group. `limit` of zero means all of them.
    ///
    /// Not just `@global`: a machine where every memory belongs to one product
    /// keeps them all under `@group:<name>`, and asking only for the global
    /// space answered "none" over thousands of rows.
    pub fn shared_memories(&self, limit: usize) -> Result<Vec<Memory>> {
        Ok(self.store.shared_memories(limit)?)
    }

    /// Counts of global memories, total and per type.
    pub fn memory_stats(&self) -> Result<devctx_store::MemoryStats> {
        Ok(devctx_memory::memory_stats(
            &self.store,
            devctx_memory::GLOBAL_PROJECT,
        )?)
    }

    /// Register a repository, or update its entry if it is already known.
    ///
    /// Re-registering the same path never creates a second row: the existing
    /// entry is carried forward, preserving its registration time and index
    /// statistics, and renamed if the caller asked for a different name.
    pub fn register(&self, req: &RegisterRequest) -> Result<ProjectRecord> {
        // A relative path is meaningless here: the daemon's working directory is
        // not the caller's, so resolving one would register the wrong repository
        // without anyone noticing. Callers resolve before they hand it over.
        if req.root.is_relative() {
            return Err(CentralError::RelativePath(req.root.clone()));
        }
        let root =
            std::fs::canonicalize(&req.root).map_err(|e| CentralError::Io(e, req.root.clone()))?;
        let config_path = root.join(devctx_core::CONFIG_FILE_NAME);

        let cfg = if config_path.is_file() {
            ProjectConfig::load(&config_path)?
        } else if req.create_config {
            let cfg = self.project_config_for(&root, req.name.as_deref());
            write_project_config(&config_path, &cfg)?;
            cfg
        } else {
            return Err(CentralError::NotInitialized(root.clone()));
        };

        let path_str = root.to_string_lossy().into_owned();
        let name = req
            .name
            .clone()
            .filter(|n| !n.is_empty())
            .or_else(|| Some(cfg.project.name.clone()).filter(|n| !n.is_empty()))
            .unwrap_or_else(|| dir_name(&root));

        // A different repository already holds this name: refuse rather than
        // silently repointing it.
        if let Some(existing) = self.store.get_project(&name)? {
            if existing.path != path_str {
                return Err(CentralError::NameTaken {
                    name,
                    path: existing.path,
                });
            }
        }

        // This repository may already be registered — possibly under another
        // name, if the caller is renaming it.
        let previous = self.store.find_project_by_path(&path_str)?;
        if let Some(prev) = &previous {
            if prev.name != name {
                self.store.delete_project(&prev.name)?;
            }
        }

        let record = ProjectRecord {
            name,
            path: path_str,
            config_path: config_path.to_string_lossy().into_owned(),
            db_path: cfg.db_path().to_string_lossy().into_owned(),
            embed_provider: cfg.embeddings.provider.clone(),
            embed_model: cfg.embeddings.model.clone(),
            embed_dim: devctx_embed::dimension_for(&cfg.embeddings.provider, &cfg.embeddings.model)
                as i64,
            description: pick(&req.description, previous.as_ref().map(|p| &p.description)),
            tags: pick(&req.tags, previous.as_ref().map(|p| &p.tags)),
            // Index statistics belong to whoever indexes; carry them across.
            last_commit: previous
                .as_ref()
                .map(|p| p.last_commit.clone())
                .unwrap_or_default(),
            last_branch: previous
                .as_ref()
                .map(|p| p.last_branch.clone())
                .unwrap_or_default(),
            last_indexed_at: previous
                .as_ref()
                .map(|p| p.last_indexed_at.clone())
                .unwrap_or_default(),
            file_count: previous.as_ref().map(|p| p.file_count).unwrap_or(0),
            symbol_count: previous.as_ref().map(|p| p.symbol_count).unwrap_or(0),
            chunk_count: previous.as_ref().map(|p| p.chunk_count).unwrap_or(0),
            registered_at: previous
                .as_ref()
                .map(|p| p.registered_at.clone())
                .unwrap_or_else(|| req.now.clone()),
            updated_at: req.now.clone(),
            active: true,
        };
        self.store.upsert_project(&record)?;
        Ok(record)
    }

    /// Record what an indexing run produced, locating the project by its
    /// repository path so the caller need not know its registered name.
    ///
    /// Returns `false` when the repository is not registered — indexing an
    /// unregistered repo is perfectly legal, it just has nowhere to report.
    pub fn record_index(
        &self,
        repo_path: &str,
        stats: &devctx_store::ProjectIndexStats,
        now: &str,
    ) -> Result<bool> {
        let Some(project) = self.store.find_project_by_path(repo_path)? else {
            return Ok(false);
        };
        Ok(self
            .store
            .update_project_index_stats(&project.name, stats, now)?)
    }

    /// Re-read a registered project's config from disk and update its row — the
    /// way an edit to `.devctx/config.yaml` reaches the registry.
    pub fn refresh(&self, name: &str, now: &str) -> Result<ProjectRecord> {
        let existing = self
            .store
            .get_project(name)?
            .ok_or_else(|| CentralError::UnknownProject(name.to_string()))?;
        self.register(&RegisterRequest {
            root: PathBuf::from(&existing.path),
            name: Some(existing.name.clone()),
            description: existing.description.clone(),
            tags: existing.tags.clone(),
            create_config: false,
            now: now.to_string(),
        })
    }

    /// List registered projects by name.
    pub fn list(&self, include_inactive: bool) -> Result<Vec<ProjectRecord>> {
        Ok(self.store.list_projects(include_inactive)?)
    }

    /// Fetch one project by name.
    pub fn get(&self, name: &str) -> Result<Option<ProjectRecord>> {
        Ok(self.store.get_project(name)?)
    }

    /// Hide a project from the default listing, keeping its row.
    pub fn deactivate(&self, name: &str, now: &str) -> Result<bool> {
        Ok(self.store.set_project_active(name, false, now)?)
    }

    /// Drop a project's row entirely. The repository itself is left untouched.
    pub fn remove(&self, name: &str) -> Result<bool> {
        Ok(self.store.delete_project(name)?)
    }

    /// A project config seeded from the central defaults.
    fn project_config_for(&self, root: &Path, name: Option<&str>) -> ProjectConfig {
        ProjectConfig {
            project: Project {
                name: name
                    .filter(|n| !n.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| dir_name(root)),
                path: root.to_string_lossy().into_owned(),
                // Group membership is a deliberate statement about a product,
                // not something a new repository should inherit by accident.
                group: String::new(),
            },
            embeddings: self.config.defaults.embeddings.clone(),
            reranking: self.config.defaults.reranking.clone(),
            ..Default::default()
        }
    }
}

/// Write a project config, creating `.devctx/` as needed.
pub fn write_project_config(path: &Path, cfg: &ProjectConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CentralError::Io(e, parent.to_path_buf()))?;
        ignore_state_dir(parent);
    }
    let yaml = serde_yaml::to_string(cfg)?;
    std::fs::write(path, yaml).map_err(|e| CentralError::Io(e, path.to_path_buf()))
}

/// Keep the index out of git.
///
/// `state/` holds a DuckDB file that grows to megabytes and changes on every
/// index, so a plain `git add -A` after `devctx init` would commit a binary
/// database — and keep committing it. The config beside it *is* worth tracking,
/// so the ignore lives inside `.devctx/` and names only `state/`.
///
/// Best-effort, and never overwrites an existing file: this is a courtesy, not
/// a reason to fail an otherwise good init.
fn ignore_state_dir(devctx_dir: &Path) {
    let path = devctx_dir.join(".gitignore");
    if path.exists() {
        return;
    }
    let _ = std::fs::write(
        &path,
        "# The index is a build artefact: large, binary, and rebuilt from the repo.\nstate/\n",
    );
}

/// Prefer a caller-supplied value, falling back to what was already stored, so
/// a plain re-registration does not wipe a description set earlier.
fn pick(incoming: &str, previous: Option<&String>) -> String {
    if !incoming.is_empty() {
        return incoming.to_string();
    }
    previous.cloned().unwrap_or_default()
}

fn dir_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string())
}

/// One registry row as JSON — the shape every client (CLI, HTTP, MCP) reads.
///
/// Lives here rather than in the transport so the direct and routed paths are
/// guaranteed to render identically.
pub fn project_json(p: &ProjectRecord) -> serde_json::Value {
    serde_json::json!({
        "name": p.name,
        "path": p.path,
        "config_path": p.config_path,
        "db_path": p.db_path,
        "embed_provider": p.embed_provider,
        "embed_model": p.embed_model,
        "embed_dim": p.embed_dim,
        "description": p.description,
        "tags": p.tags,
        "last_commit": p.last_commit,
        "last_branch": p.last_branch,
        "last_indexed_at": p.last_indexed_at,
        "file_count": p.file_count,
        "symbol_count": p.symbol_count,
        "chunk_count": p.chunk_count,
        "active": p.active,
    })
}

/// Seconds since the Unix epoch, as a string — the timestamp format the rest of
/// the store uses.
pub fn now_stamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that cleans up after itself.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("devctx_central_{tag}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn join(&self, p: &str) -> PathBuf {
            self.0.join(p)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A repo directory with a `.devctx/config.yaml` already in place.
    fn init_repo(root: &Path, name: &str, model: &str) {
        let cfg = ProjectConfig {
            project: Project {
                name: name.to_string(),
                path: root.to_string_lossy().into_owned(),
                group: String::new(),
            },
            embeddings: devctx_core::config::Embeddings {
                model: model.to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        std::fs::create_dir_all(root).unwrap();
        write_project_config(&root.join(devctx_core::CONFIG_FILE_NAME), &cfg).unwrap();
    }

    fn req(root: PathBuf) -> RegisterRequest {
        RegisterRequest {
            root,
            now: "100".into(),
            ..Default::default()
        }
    }

    #[test]
    fn opens_with_the_configured_memory_dimension() {
        let tmp = Tmp::new("open");
        let central = Central::open_in(&tmp.0).unwrap();
        assert_eq!(central.store().dimension(), 384);
        assert!(central.paths().db.is_file());
        assert!(central.list(true).unwrap().is_empty());

        // The defaults are written out on first run so they can be edited.
        let written = std::fs::read_to_string(&central.paths().config).unwrap();
        assert!(written.contains("minilm-l6"), "got: {written}");
        assert!(written.contains("defaults:"), "got: {written}");
    }

    #[test]
    fn an_existing_config_is_never_overwritten() {
        let tmp = Tmp::new("keepconfig");
        let mut cfg = CentralConfig::default();
        cfg.defaults.embeddings.model = "ml-granite".into();
        cfg.save(&tmp.join("config.yaml")).unwrap();

        let central = Central::open_in(&tmp.0).unwrap();
        assert_eq!(central.config().defaults.embeddings.model, "ml-granite");
        let back = CentralConfig::load_or_default(&tmp.join("config.yaml")).unwrap();
        assert_eq!(back.defaults.embeddings.model, "ml-granite");
    }

    #[test]
    fn changing_the_memory_model_is_refused_not_corrupted() {
        let tmp = Tmp::new("dim");
        Central::open_in(&tmp.0).unwrap(); // creates a 384-wide store

        let mut cfg = CentralConfig::default();
        cfg.memory.model = "bge-base".into(); // 768
        cfg.save(&tmp.join("config.yaml")).unwrap();

        match Central::open_in(&tmp.0) {
            Err(CentralError::DimensionMismatch {
                found, expected, ..
            }) => {
                assert_eq!(found, 384);
                assert_eq!(expected, 768);
            }
            Err(other) => panic!("expected a dimension mismatch, got {other}"),
            Ok(_) => panic!("opening with a changed memory model must fail"),
        }
    }

    #[test]
    fn registers_a_repo_and_records_its_model() {
        let tmp = Tmp::new("register");
        let repo = tmp.join("alpha");
        init_repo(&repo, "alpha", "bge-base");

        let central = Central::open_in(&tmp.join("home")).unwrap();
        let rec = central.register(&req(repo.clone())).unwrap();

        assert_eq!(rec.name, "alpha");
        assert_eq!(rec.embed_model, "bge-base");
        assert_eq!(rec.embed_dim, 768);
        assert!(rec.active);
        assert_eq!(rec.registered_at, "100");
        assert!(rec.db_path.ends_with("index.duckdb"));
        assert_eq!(central.list(false).unwrap().len(), 1);
    }

    #[test]
    fn refuses_an_uninitialized_repo_unless_asked_to_create_one() {
        let tmp = Tmp::new("uninit");
        let repo = tmp.join("bare");
        std::fs::create_dir_all(&repo).unwrap();

        let central = Central::open_in(&tmp.join("home")).unwrap();
        assert!(matches!(
            central.register(&req(repo.clone())).unwrap_err(),
            CentralError::NotInitialized(_)
        ));

        // With create_config the project config is written from central defaults.
        let mut cfg = CentralConfig::default();
        cfg.defaults.embeddings.model = "ml-granite".into();
        cfg.save(&tmp.join("home/config.yaml")).unwrap();
        let central = Central::open_in(&tmp.join("home")).unwrap();

        let rec = central
            .register(&RegisterRequest {
                create_config: true,
                ..req(repo.clone())
            })
            .unwrap();
        assert_eq!(rec.name, "bare");
        assert_eq!(rec.embed_model, "ml-granite", "inherited central defaults");
        assert!(repo.join(devctx_core::CONFIG_FILE_NAME).is_file());
    }

    #[test]
    fn re_registering_the_same_path_updates_in_place() {
        let tmp = Tmp::new("reregister");
        let repo = tmp.join("alpha");
        init_repo(&repo, "alpha", "minilm-l6");
        let central = Central::open_in(&tmp.join("home")).unwrap();

        central.register(&req(repo.clone())).unwrap();
        central
            .store()
            .update_project_index_stats(
                "alpha",
                &devctx_store::ProjectIndexStats {
                    commit: "c0ffee".into(),
                    branch: "main".into(),
                    files: 7,
                    symbols: 20,
                    chunks: 55,
                },
                "150",
            )
            .unwrap();

        let again = central
            .register(&RegisterRequest {
                description: "the alpha service".into(),
                now: "200".into(),
                ..req(repo.clone())
            })
            .unwrap();

        assert_eq!(central.list(true).unwrap().len(), 1, "no duplicate row");
        assert_eq!(again.registered_at, "100", "registration time preserved");
        assert_eq!(again.updated_at, "200");
        assert_eq!(again.last_commit, "c0ffee", "index stats preserved");
        assert_eq!(again.file_count, 7);
        assert_eq!(again.description, "the alpha service");

        // An empty description on a later re-registration must not wipe it.
        let third = central
            .register(&RegisterRequest {
                now: "300".into(),
                ..req(repo.clone())
            })
            .unwrap();
        assert_eq!(third.description, "the alpha service");
    }

    #[test]
    fn renaming_moves_the_row_instead_of_duplicating_it() {
        let tmp = Tmp::new("rename");
        let repo = tmp.join("alpha");
        init_repo(&repo, "alpha", "minilm-l6");
        let central = Central::open_in(&tmp.join("home")).unwrap();
        central.register(&req(repo.clone())).unwrap();

        let renamed = central
            .register(&RegisterRequest {
                name: Some("alpha-svc".into()),
                now: "200".into(),
                ..req(repo.clone())
            })
            .unwrap();

        assert_eq!(renamed.name, "alpha-svc");
        assert_eq!(central.list(true).unwrap().len(), 1);
        assert!(central.get("alpha").unwrap().is_none());
        assert_eq!(renamed.registered_at, "100", "history follows the rename");
    }

    #[test]
    fn a_name_taken_by_another_repo_is_refused() {
        let tmp = Tmp::new("collision");
        let a = tmp.join("one/shared-name");
        let b = tmp.join("two/shared-name");
        init_repo(&a, "shared-name", "minilm-l6");
        init_repo(&b, "shared-name", "minilm-l6");

        let central = Central::open_in(&tmp.join("home")).unwrap();
        central.register(&req(a.clone())).unwrap();

        match central.register(&req(b.clone())).unwrap_err() {
            CentralError::NameTaken { name, .. } => assert_eq!(name, "shared-name"),
            other => panic!("expected NameTaken, got {other}"),
        }

        // An explicit name resolves it.
        let rec = central
            .register(&RegisterRequest {
                name: Some("shared-name-two".into()),
                ..req(b)
            })
            .unwrap();
        assert_eq!(rec.name, "shared-name-two");
        assert_eq!(central.list(true).unwrap().len(), 2);
    }

    #[test]
    fn refresh_picks_up_an_edited_project_config() {
        let tmp = Tmp::new("refresh");
        let repo = tmp.join("alpha");
        init_repo(&repo, "alpha", "minilm-l6");
        let central = Central::open_in(&tmp.join("home")).unwrap();
        central.register(&req(repo.clone())).unwrap();

        init_repo(&repo, "alpha", "bge-base"); // rewrite the config on disk
        let rec = central.refresh("alpha", "400").unwrap();
        assert_eq!(rec.embed_model, "bge-base");
        assert_eq!(rec.embed_dim, 768);
        assert_eq!(rec.updated_at, "400");

        assert!(matches!(
            central.refresh("ghost", "400").unwrap_err(),
            CentralError::UnknownProject(_)
        ));
    }

    /// A relative path resolved by the store rather than the caller would point
    /// at whatever repository the daemon happens to be sitting in.
    /// The index is a binary artefact that must not end up in git.
    #[test]
    fn init_keeps_the_index_out_of_git() {
        let tmp = Tmp::new("gitignore");
        let repo = tmp.join("alpha");
        std::fs::create_dir_all(&repo).unwrap();
        let central = Central::open_in(&tmp.join("home")).unwrap();
        central
            .register(&RegisterRequest {
                create_config: true,
                ..req(repo.clone())
            })
            .unwrap();

        let ignore = repo.join(".devctx/.gitignore");
        let body = std::fs::read_to_string(&ignore).unwrap();
        assert!(body.contains("state/"), "got: {body}");
        assert!(
            !body.contains("config.yaml"),
            "the config is worth tracking: {body}"
        );

        // An existing ignore file is the user's; leave it alone.
        std::fs::write(&ignore, "mine\n").unwrap();
        central
            .register(&RegisterRequest {
                create_config: true,
                ..req(repo.clone())
            })
            .unwrap();
        assert_eq!(std::fs::read_to_string(&ignore).unwrap(), "mine\n");
    }

    #[test]
    fn a_relative_path_is_refused() {
        let tmp = Tmp::new("relative");
        let central = Central::open_in(&tmp.join("home")).unwrap();
        match central.register(&RegisterRequest {
            root: PathBuf::from("./somewhere"),
            ..req(PathBuf::from("."))
        }) {
            Err(CentralError::RelativePath(p)) => assert_eq!(p, PathBuf::from("./somewhere")),
            other => panic!("expected a relative-path refusal, got {other:?}"),
        }
    }

    #[test]
    fn deactivate_and_remove() {
        let tmp = Tmp::new("lifecycle");
        let repo = tmp.join("alpha");
        init_repo(&repo, "alpha", "minilm-l6");
        let central = Central::open_in(&tmp.join("home")).unwrap();
        central.register(&req(repo.clone())).unwrap();

        assert!(central.deactivate("alpha", "200").unwrap());
        assert!(central.list(false).unwrap().is_empty());
        assert_eq!(central.list(true).unwrap().len(), 1);

        assert!(central.remove("alpha").unwrap());
        assert!(central.list(true).unwrap().is_empty());
        assert!(!central.remove("alpha").unwrap());
    }
}
