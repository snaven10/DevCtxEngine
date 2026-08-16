# Interactive init and memory transfer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `devctx init` ask the questions whose wrong answers are expensive and silent, and let memories move between machines without ever losing one.

**Architecture:** Three independent pieces, in order of what unblocks what. First the storage defaults, which are wrong today and cost nothing to fix. Then export/import, which needs two new read paths in the store. Last the wizard, which reads the registry to show what the machine already does. Each lands working and testable on its own.

**Tech Stack:** Rust 2021, DuckDB via `duckdb` crate, `serde_json` for JSONL, `ureq` for downloads, `clap` for the CLI.

**Spec:** `docs/proposals/interactive-init.md`

## Global Constraints

- Build and test **offline**: `cargo build --offline`, `cargo test --offline`. Without it cargo hangs silently reaching for the crates registry.
- `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings` must be clean **immediately before each commit**, not earlier. Formatting and then editing again is how CI went red three times.
- A rebuilt binary does not take effect until running servers stop: `devctx serve --stop` and `devctx serve --central --stop` before any manual verification.
- Never write to a `.duckdb` with anything but the engine's own code. Writing through another DuckDB build left the ART indexes inconsistent once already.
- Comments explain *why*, in the style of the surrounding code. No comment that restates the line below it.
- Conventional commits. No AI attribution in commit messages.

---

### Task 1: Correct the storage defaults for new projects

`devctx init` writes `hnsw: false` and `metric: ''`. HNSW was measured at 84 ms → 49 ms with recall@10 of 100%, so a new repository should not start on the slow path; and an empty metric string teaches the reader nothing about what the field accepts.

**Files:**
- Modify: `crates/devctx-core/src/config.rs` (the `Storage` struct's defaults)
- Test: `crates/devctx-core/src/config.rs` (its own `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `Storage::default()` yields `hnsw: true`, `metric: "cosine"`. Task 6 relies on `init` writing these.

- [ ] **Step 1: Write the failing test**

In `crates/devctx-core/src/config.rs`, inside `mod tests`:

```rust
    /// A new project should start on the fast path. HNSW measured 84 ms → 49 ms
    /// on a 17k-vector store with recall@10 unchanged at 100%, so defaulting it
    /// off means every new repository is slower for no gain anyone chose.
    #[test]
    fn new_projects_default_to_an_indexed_store() {
        let s = Storage::default();
        assert!(s.hnsw, "HNSW should be on by default");
        assert_eq!(s.metric, "cosine", "the metric must name itself");
    }
```

- [ ] **Step 2: Run the test and watch it fail**

```bash
cd ~/personal/DevCtxEngine
cargo test --offline -p devctx-core new_projects_default_to_an_indexed_store
```

Expected: FAIL — `hnsw` is `false` and `metric` is `""`.

- [ ] **Step 3: Give `Storage` a hand-written `Default`**

`Storage` currently derives `Default`. Remove `Default` from its `derive` list and add:

```rust
impl Default for Storage {
    fn default() -> Self {
        Self {
            db_path: String::new(),
            hnsw: true,
            metric: default_metric(),
            fts: false,
        }
    }
}
```

Also change the `hnsw` field's `#[serde(default)]` to `#[serde(default = "default_hnsw")]` and add beside `default_metric`:

```rust
fn default_hnsw() -> bool {
    true
}
```

The serde default matters as much as the struct default: a config file written before this change omits nothing, but one written by hand might, and the two paths must agree.

- [ ] **Step 4: Run the test and watch it pass**

```bash
cargo test --offline -p devctx-core new_projects_default_to_an_indexed_store
```

Expected: PASS.

- [ ] **Step 5: Check nothing else assumed the old defaults**

```bash
cargo test --offline -j 3 2>&1 | grep -E 'test result|FAILED'
```

Expected: every suite `ok`. If a test asserted `hnsw == false`, it was asserting the bug — update it and say so in its comment.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all && cargo clippy --offline --all-targets -j 3 -- -D warnings
git add crates/devctx-core/src/config.rs
git commit -m "fix: new projects default to an HNSW-indexed store

Measured on a 17k-vector store: the vector scan drops from 47 ms to 9 ms
and the whole search from 84 ms to 49 ms, with recall@10 unchanged at 100%
against exact search. Defaulting it off meant every new repository took the
slow path unless someone knew to turn it on. `metric` now says `cosine`
rather than an empty string, which behaved correctly but named nothing."
```

---

### Task 2: Read paths the export needs

Export must read *every* memory of a scope and each one's vector. Neither exists: `recent_memories` takes a limit and there is no vector lookup by id at all.

**Files:**
- Modify: `crates/devctx-store/src/memory.rs` (add `all_memories`)
- Modify: `crates/devctx-store/src/store.rs` (add `vector_by_id`)
- Test: both files' own `mod tests`

**Interfaces:**
- Consumes: `Store::upsert_memory(&Memory)`, `Store::upsert(&[VectorPoint])`.
- Produces:
  - `Store::all_memories(&self, project: &str) -> Result<Vec<Memory>>` — every live memory under that project key, oldest first.
  - `Store::vector_by_id(&self, id: &str) -> Result<Option<Vec<f32>>>` — the embedding, or `None`.

- [ ] **Step 1: Write both failing tests**

In `crates/devctx-store/src/memory.rs`, inside `mod tests`:

```rust
    /// Export needs the whole set, not a page of it: `recent_memories` caps at a
    /// limit, and a cap silently truncates the file someone is trusting to hold
    /// everything.
    #[test]
    fn all_memories_returns_every_live_row_for_a_project() {
        let store = Store::open_in_memory(3).unwrap();
        for i in 0..5 {
            let mut m = mem(&format!("mem_{i}"), "", "note");
            m.project = "proj".into();
            m.created_at = format!("{i}");
            store.upsert_memory(&m).unwrap();
        }
        let mut other = mem("mem_other", "", "note");
        other.project = "elsewhere".into();
        store.upsert_memory(&other).unwrap();

        let got = store.all_memories("proj").unwrap();
        assert_eq!(got.len(), 5, "every row, and only this project's");
        assert_eq!(got[0].id, "mem_0", "oldest first, so an import replays in order");

        store.delete_memory("mem_2", "999").unwrap();
        assert_eq!(store.all_memories("proj").unwrap().len(), 4, "tombstoned rows are not exported");
    }
```

In `crates/devctx-store/src/store.rs`, inside `mod tests`:

```rust
    /// Export carries embeddings so an import between matching machines does not
    /// pay to recompute them — which was measured at 46 minutes for 2090
    /// memories.
    #[test]
    fn a_vector_can_be_read_back_by_id() {
        let store = Store::open_in_memory(3).unwrap();
        store
            .upsert(&[VectorPoint {
                id: "mem_a".into(),
                vector: vec![0.5, 0.25, 0.125],
                text: "hello".into(),
                metadata: Default::default(),
            }])
            .unwrap();

        let got = store.vector_by_id("mem_a").unwrap().expect("stored");
        assert_eq!(got, vec![0.5, 0.25, 0.125]);
        assert!(store.vector_by_id("mem_missing").unwrap().is_none());
    }
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test --offline -p devctx-store all_memories_returns_every_live_row_for_a_project a_vector_can_be_read_back_by_id
```

Expected: FAIL — neither method exists.

- [ ] **Step 3: Implement `all_memories`**

In `crates/devctx-store/src/memory.rs`, beside `recent_memories`:

```rust
    /// Every live memory stored under `project`, oldest first.
    ///
    /// Unlike [`recent_memories`](Self::recent_memories) this takes no limit:
    /// it backs export, where a cap would quietly hand someone a file missing
    /// the rows past it. Oldest first so replaying the file preserves the order
    /// revisions happened in.
    pub fn all_memories(&self, project: &str) -> Result<Vec<Memory>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MEM_COLS} FROM memories
             WHERE project = ? AND deleted_at IS NULL
             ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map(params![project], row_to_memory)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
```

- [ ] **Step 4: Implement `vector_by_id`**

In `crates/devctx-store/src/store.rs`, beside `search`:

```rust
    /// The stored embedding for `id`, or `None` when there is none.
    pub fn vector_by_id(&self, id: &str) -> Result<Option<Vec<f32>>> {
        let mut stmt = self.conn.prepare("SELECT vector FROM vectors WHERE id = ?")?;
        let row: std::result::Result<Vec<Value>, _> =
            stmt.query_row(params![id], |r| r.get::<_, Vec<Value>>(0));
        match row {
            Ok(v) => Ok(Some(to_f32(v))),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
```

`to_f32` is the existing free function at the bottom of `store.rs` that converts DuckDB `Value`s to `f32`; reuse it rather than writing a second conversion.

- [ ] **Step 5: Run both tests and watch them pass**

```bash
cargo test --offline -p devctx-store all_memories_returns_every_live_row_for_a_project a_vector_can_be_read_back_by_id
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all && cargo clippy --offline --all-targets -j 3 -- -D warnings
git add crates/devctx-store/src/memory.rs crates/devctx-store/src/store.rs
git commit -m "feat(store): read every memory of a project, and a vector by id

Export needs both and neither existed: the only listing took a limit, which
would hand someone a file quietly missing everything past it, and vectors
could be searched but never fetched."
```

---

### Task 3: The transfer file format

One module owns what a line of the export looks like, so the reader and writer cannot drift.

**Files:**
- Create: `crates/devctx-cli/src/transfer.rs`
- Modify: `crates/devctx-cli/src/main.rs` (add `mod transfer;`)
- Test: `crates/devctx-cli/src/transfer.rs` (its own `mod tests`)

**Interfaces:**
- Consumes: `devctx_store::Memory` (fields: `id, title, content, memory_type, scope, project, topic_key, tags, author, repo, branch, files, revision_count, duplicate_count, normalized_hash, vector_id, session_id, created_at, updated_at, deleted_at`).
- Produces:
  - `struct TransferLine { memory: Memory, embedding: Option<Embedding> }`
  - `struct Embedding { model: String, dim: usize, vector: Vec<f32> }`
  - `fn to_line(m: &Memory, e: Option<Embedding>) -> Result<String>`
  - `fn from_line(s: &str) -> Result<TransferLine>`

- [ ] **Step 1: Write the failing test**

Create `crates/devctx-cli/src/transfer.rs` containing only:

```rust
//! The on-disk shape of an exported memory: one JSON object per line.
//!
//! JSONL rather than a database file because the case that justifies exporting
//! at all is handing memories to someone on a different release — and a DuckDB
//! file is readable only by the build that wrote it. A text line survives that,
//! greps, diffs, streams, and can be repaired by hand when one row is wrong.

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_store::Memory;

    fn sample() -> Memory {
        Memory {
            id: "mem_a".into(),
            title: "Título con acento".into(),
            content: "línea uno\nlínea dos".into(),
            memory_type: "decision".into(),
            scope: "group".into(),
            project: "@group:REVFA".into(),
            topic_key: "auth".into(),
            tags: "a,b".into(),
            repo: "api".into(),
            normalized_hash: "abc123".into(),
            created_at: "100".into(),
            updated_at: "200".into(),
            ..Default::default()
        }
    }

    /// A line must survive the trip unchanged, newlines and accents included:
    /// memories are prose, and prose is where a lossy encoding shows up as a
    /// corrupted sentence rather than an error.
    #[test]
    fn a_memory_round_trips_through_a_line() {
        let m = sample();
        let line = to_line(&m, None).unwrap();
        assert!(!line.contains('\n'), "one memory per line, always");

        let back = from_line(&line).unwrap();
        assert_eq!(back.memory, m);
        assert!(back.embedding.is_none());
    }

    /// The embedding travels with the model that produced it. Without the name
    /// and width beside it an importer cannot tell a reusable vector from one
    /// that would rank everything wrongly.
    #[test]
    fn an_embedding_travels_with_its_model_and_width() {
        let e = Embedding {
            model: "ml-granite".into(),
            dim: 3,
            vector: vec![0.5, 0.25, 0.125],
        };
        let line = to_line(&sample(), Some(e.clone())).unwrap();
        let back = from_line(&line).unwrap().embedding.expect("carried");
        assert_eq!(back.model, "ml-granite");
        assert_eq!(back.dim, 3);
        assert_eq!(back.vector, e.vector);
    }

    /// A damaged line names itself. An import of a thousand lines that fails
    /// with "invalid JSON" and no more is not something anyone can act on.
    #[test]
    fn a_damaged_line_reports_what_it_could_not_read() {
        let err = from_line("{not json").unwrap_err().to_string();
        assert!(err.contains("line"), "the message must locate the problem: {err}");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cargo test --offline -p devctx-cli transfer 2>&1 | tail -5
```

Expected: the file compiles into nothing, so the tests do not run at all — `transfer` is not yet a module of the crate. That is the failure: a test that never runs is not a passing test. Step 3 wires it, and only then will the compiler report the real gap (`to_line`, `from_line` and `Embedding` do not exist).

- [ ] **Step 3: Wire the module and implement it**

In `crates/devctx-cli/src/main.rs`, beside the other `mod` lines:

```rust
mod transfer;
```

At the top of `crates/devctx-cli/src/transfer.rs`, above the test module:

```rust
use anyhow::{Context, Result};
use devctx_store::Memory;
use serde::{Deserialize, Serialize};

/// The embedding of a memory, with what produced it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Embedding {
    /// Registry key of the model, e.g. `ml-granite`.
    pub model: String,
    /// Vector width. Carried separately because two models can share a name
    /// across implementations and not share a vector space.
    pub dim: usize,
    pub vector: Vec<f32>,
}

/// One exported memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferLine {
    #[serde(flatten)]
    pub memory: Memory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Embedding>,
}

/// Serialize one memory as a single line.
pub fn to_line(m: &Memory, embedding: Option<Embedding>) -> Result<String> {
    let line = TransferLine {
        memory: m.clone(),
        embedding,
    };
    serde_json::to_string(&line).context("serializing a memory")
}

/// Parse one line back.
pub fn from_line(s: &str) -> Result<TransferLine> {
    serde_json::from_str(s).with_context(|| {
        let preview: String = s.chars().take(60).collect();
        format!("reading a line of the export: {preview}…")
    })
}
```

`Memory` must gain `Serialize`/`Deserialize` for this to compile. In `crates/devctx-store/src/memory.rs`, add them to its derive list:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Memory {
```

and add `serde = { workspace = true }` to `crates/devctx-store/Cargo.toml` under `[dependencies]` if it is not already there.

- [ ] **Step 4: Run the tests and watch them pass**

```bash
cargo test --offline -p devctx-cli transfer
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy --offline --all-targets -j 3 -- -D warnings
git add crates/devctx-cli/src/transfer.rs crates/devctx-cli/src/main.rs crates/devctx-store/src/memory.rs crates/devctx-store/Cargo.toml
git commit -m "feat(cli): a line-per-memory transfer format

JSONL rather than a database file: exporting exists so memories can reach
someone on another release, and a DuckDB file is readable only by the build
that wrote it. The embedding travels with its model name and width, which is
what lets an importer tell a reusable vector from one that would rank
everything wrongly."
```

---

### Task 4: The import rule — only ever add

The heart of the feature and the part with teeth. `remember` revises on a topic-key match, replacing content. That is right for amending your own note and wrong for a file from another machine, where it would destroy a local memory the sender never saw.

**Files:**
- Create: `crates/devctx-cli/src/transfer_apply.rs`
- Modify: `crates/devctx-cli/src/main.rs` (add `mod transfer_apply;`)
- Test: `crates/devctx-cli/src/transfer_apply.rs` (its own `mod tests`)

**Interfaces:**
- Consumes: `transfer::TransferLine`, `Store::all_memories`, `Store::upsert_memory`.
- Produces:
  - `enum Outcome { Added, AlreadyPresent, TopicCollision }`
  - `fn decide(incoming: &Memory, existing: &[Memory]) -> Outcome`
  - `struct ImportReport { added: usize, already: usize, collisions: Vec<String> }`

- [ ] **Step 1: Write the failing tests**

Create `crates/devctx-cli/src/transfer_apply.rs`:

```rust
//! What an import does with each incoming memory.
//!
//! Import never overwrites and never deletes: its input comes from somewhere
//! else, so nothing already here may be lost by running it — including running
//! it with the wrong file. That is deliberately *not* `remember`'s rule, which
//! revises on a topic-key match and replaces the content. Correct when you are
//! amending your own note; destructive when the text arrived from another
//! machine.

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_store::Memory;

    fn mem(id: &str, topic: &str, content: &str, hash: &str) -> Memory {
        Memory {
            id: id.into(),
            title: id.into(),
            content: content.into(),
            topic_key: topic.into(),
            normalized_hash: hash.into(),
            project: "@group:REVFA".into(),
            ..Default::default()
        }
    }

    /// Nothing here yet: take it.
    #[test]
    fn an_unseen_memory_is_added() {
        let existing = vec![mem("mem_a", "auth", "a", "h1")];
        let incoming = mem("mem_b", "pdf", "b", "h2");
        assert_eq!(decide(&incoming, &existing), Outcome::Added);
    }

    /// The same content twice is the same memory, whatever its id says: two
    /// machines that both recorded one fact should converge, so importing a
    /// file twice must not double it.
    #[test]
    fn identical_content_is_recognised_however_it_is_labelled() {
        let existing = vec![mem("mem_a", "auth", "same text", "h1")];
        let incoming = mem("DIFFERENT_ID", "auth", "same text", "h1");
        assert_eq!(decide(&incoming, &existing), Outcome::AlreadyPresent);
    }

    /// The case that earns the rule. Two machines learned different things
    /// about one subject. Overwriting loses the local one; skipping loses the
    /// incoming one; keeping both loses neither.
    #[test]
    fn a_topic_collision_keeps_both() {
        let existing = vec![mem("mem_a", "auth", "we use JWT", "h1")];
        let incoming = mem("mem_b", "auth", "we moved to sessions", "h2");
        assert_eq!(decide(&incoming, &existing), Outcome::TopicCollision);
    }

    /// A collision must not leave two memories fighting over one topic: the
    /// incoming copy gives the key up, so the existing memory stays the one
    /// `remember --topic auth` will revise next time.
    #[test]
    fn the_incoming_copy_of_a_collision_gives_up_the_topic_key() {
        let incoming = mem("mem_b", "auth", "we moved to sessions", "h2");
        let stored = prepare(&incoming, Outcome::TopicCollision);
        assert_eq!(stored.topic_key, "", "the local memory keeps the topic");
        assert_eq!(stored.content, "we moved to sessions", "nothing is lost");
    }

    /// An empty topic key is not a collision — most memories have none, and
    /// treating "" as a shared topic would collapse them all into one.
    #[test]
    fn memories_without_a_topic_never_collide() {
        let existing = vec![mem("mem_a", "", "a", "h1")];
        let incoming = mem("mem_b", "", "b", "h2");
        assert_eq!(decide(&incoming, &existing), Outcome::Added);
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test --offline -p devctx-cli transfer_apply 2>&1 | tail -5
```

Expected: FAIL — the module is not wired and `decide`/`prepare`/`Outcome` do not exist.

- [ ] **Step 3: Wire the module and implement the rule**

In `crates/devctx-cli/src/main.rs`:

```rust
mod transfer_apply;
```

At the top of `crates/devctx-cli/src/transfer_apply.rs`, above the tests:

```rust
use devctx_store::Memory;

/// What an import decided about one incoming memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Not here; stored as-is.
    Added,
    /// The same content is already here; skipped.
    AlreadyPresent,
    /// Its topic key belongs to a different memory here. Stored anyway,
    /// without the key, so neither text is lost.
    TopicCollision,
}

/// Decide what to do with `incoming`, given everything already in the target
/// scope. Compares before writing: an import that discovered a conflict
/// half-way through would already have replaced something.
pub fn decide(incoming: &Memory, existing: &[Memory]) -> Outcome {
    // Content identity, not id: two machines that recorded the same fact give
    // it different ids, and importing both should converge on one row.
    if existing
        .iter()
        .any(|e| e.normalized_hash == incoming.normalized_hash)
    {
        return Outcome::AlreadyPresent;
    }
    // Most memories carry no topic key; an empty one is the absence of a claim,
    // not a claim they all share.
    if !incoming.topic_key.is_empty()
        && existing
            .iter()
            .any(|e| e.topic_key == incoming.topic_key)
    {
        return Outcome::TopicCollision;
    }
    Outcome::Added
}

/// The row to store, given the decision. Only a collision changes anything.
pub fn prepare(incoming: &Memory, outcome: Outcome) -> Memory {
    let mut m = incoming.clone();
    if outcome == Outcome::TopicCollision {
        // The local memory keeps the topic, so `remember --topic X` goes on
        // revising the one its author has been revising.
        m.topic_key = String::new();
    }
    m
}

/// What an import did, for the summary it prints.
#[derive(Debug, Default)]
pub struct ImportReport {
    pub added: usize,
    pub already: usize,
    /// Titles of the memories kept alongside an existing topic owner. Titles,
    /// not counts: a collision is usually two people having learned different
    /// things about one subject, which is worth reading.
    pub collisions: Vec<String>,
}

impl ImportReport {
    pub fn record(&mut self, m: &Memory, outcome: Outcome) {
        match outcome {
            Outcome::Added => self.added += 1,
            Outcome::AlreadyPresent => self.already += 1,
            Outcome::TopicCollision => {
                self.added += 1;
                self.collisions.push(if m.title.is_empty() {
                    m.id.clone()
                } else {
                    m.title.clone()
                });
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests and watch them pass**

```bash
cargo test --offline -p devctx-cli transfer_apply
```

Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy --offline --all-targets -j 3 -- -D warnings
git add crates/devctx-cli/src/transfer_apply.rs crates/devctx-cli/src/main.rs
git commit -m "feat(cli): import adds, and never overwrites

Deliberately not remember's rule. remember revises on a topic-key match,
replacing the content — right when amending your own note, destructive when
the text came from another machine and would silently replace a local memory
its sender never saw. A collision keeps both: the incoming copy gives up the
topic key, so the existing memory stays the one future revisions amend, and
the summary names them, because two machines disagreeing about one subject is
worth reading rather than resolving automatically."
```

---

### Task 5: `devctx memories export` and `devctx memories import`

Wire the format and the rule into commands.

**Files:**
- Modify: `crates/devctx-cli/src/main.rs` (the `Command` enum, the dispatch, and two new functions)
- Test: manual verification against the real store (steps below)

**Interfaces:**
- Consumes: `transfer::{to_line, from_line, Embedding}`, `transfer_apply::{decide, prepare, ImportReport}`, `Store::all_memories`, `Store::vector_by_id`, `Store::upsert_memory`, `Store::upsert`, `devctx_memory::group_project`, `CentralPaths::resolve`, `Central::open`.
- Produces: the two subcommands; nothing later depends on their internals.

- [ ] **Step 1: Add the subcommands**

In the `Command` enum in `crates/devctx-cli/src/main.rs`, after `MemoryPurge`:

```rust
    /// Export or import memories as JSONL.
    Memories {
        #[command(subcommand)]
        action: MemoriesAction,
    },
```

and beside the other action enums:

```rust
#[derive(Subcommand)]
enum MemoriesAction {
    /// Write memories to stdout, one JSON object per line.
    Export {
        /// Which memories: `local`, `group`, or `global`.
        #[arg(long, default_value = "local")]
        scope: String,
        /// Within a shared scope, only what this repository contributed.
        #[arg(long)]
        repo: Option<String>,
    },
    /// Read memories from a JSONL file. Only ever adds; never overwrites.
    Import {
        /// The file to read.
        file: PathBuf,
        /// Put every memory in this scope, whatever the file says.
        #[arg(long)]
        scope: Option<String>,
        /// Report what would happen without writing.
        #[arg(long)]
        dry_run: bool,
    },
}
```

In the dispatch `match`:

```rust
        Command::Memories { action } => match action {
            MemoriesAction::Export { scope, repo } => cmd_memories_export(&scope, repo.as_deref()),
            MemoriesAction::Import {
                file,
                scope,
                dry_run,
            } => cmd_memories_import(&file, scope.as_deref(), dry_run),
        },
```

- [ ] **Step 2: Implement export**

Beside `cmd_memory_purge`:

```rust
/// Resolve a scope name to the project key its memories are stored under, and
/// whether that key lives in the central store.
fn scope_key(cfg: &ProjectConfig, scope: &str) -> Result<(String, bool)> {
    match scope {
        "local" => Ok((project_name(cfg), false)),
        "global" => Ok((devctx_memory::GLOBAL_PROJECT.to_string(), true)),
        "group" => {
            if cfg.project.group.is_empty() {
                bail!(
                    "this project declares no group; set `project.group` in its config, \
                     or use --scope local or --scope global"
                );
            }
            Ok((devctx_memory::group_project(&cfg.project.group), true))
        }
        other => bail!("unknown scope `{other}`; expected local, group or global"),
    }
}

/// `devctx memories export` — write a scope's memories to stdout as JSONL.
fn cmd_memories_export(scope: &str, repo: Option<&str>) -> Result<()> {
    let cfg = load_project()?;
    let (key, central) = scope_key(&cfg, scope)?;
    let store = if central {
        Central::open()
            .context(
                "opening the central store (stop a running daemon first: \
                 `devctx serve --central --stop`)",
            )?
            .store()
            .try_clone()?
    } else {
        open_store(&cfg, configured_dimension(&cfg)).context(
            "opening the store (stop a running server first: `devctx serve --stop`)",
        )?
    };

    let model = cfg.embeddings.model.clone();
    let dim = configured_dimension(&cfg);
    let mut written = 0usize;
    for m in store.all_memories(&key)? {
        if let Some(r) = repo {
            if m.repo != r {
                continue;
            }
        }
        let embedding = store.vector_by_id(&m.id)?.map(|vector| transfer::Embedding {
            model: model.clone(),
            dim,
            vector,
        });
        println!("{}", transfer::to_line(&m, embedding)?);
        written += 1;
    }
    // stderr so it does not land in the file being redirected.
    eprintln!("Exported {written} memories from `{key}`.");
    Ok(())
}
```

- [ ] **Step 3: Implement import**

```rust
/// `devctx memories import` — add memories from a JSONL file.
fn cmd_memories_import(file: &Path, scope: Option<&str>, dry_run: bool) -> Result<()> {
    let cfg = load_project()?;
    let raw = std::fs::read_to_string(file)
        .with_context(|| format!("reading {}", file.display()))?;

    // Group by destination first: a file may carry memories of several scopes,
    // and each destination is a different store.
    let mut lines: Vec<transfer::TransferLine> = Vec::new();
    for (n, l) in raw.lines().enumerate() {
        if l.trim().is_empty() {
            continue;
        }
        lines.push(
            transfer::from_line(l).with_context(|| format!("{}:{}", file.display(), n + 1))?,
        );
    }

    let mut by_key: std::collections::BTreeMap<String, Vec<transfer::TransferLine>> =
        Default::default();
    for line in lines {
        let key = match scope {
            Some(s) => scope_key(&cfg, s)?.0,
            None => line.memory.project.clone(),
        };
        by_key.entry(key).or_default().push(line);
    }

    let model = cfg.embeddings.model.clone();
    let dim = configured_dimension(&cfg);
    for (key, incoming) in by_key {
        let central = key.starts_with('@');
        let store = if central {
            Central::open()
                .context(
                    "opening the central store (stop a running daemon first: \
                     `devctx serve --central --stop`)",
                )?
                .store()
                .try_clone()?
        } else {
            open_store(&cfg, dim).context(
                "opening the store (stop a running server first: `devctx serve --stop`)",
            )?
        };

        let mut existing = store.all_memories(&key)?;
        let mut report = transfer_apply::ImportReport::default();
        let mut reused = 0usize;
        let mut reembedded = 0usize;

        for line in incoming {
            let outcome = transfer_apply::decide(&line.memory, &existing);
            report.record(&line.memory, outcome);
            if outcome == transfer_apply::Outcome::AlreadyPresent || dry_run {
                continue;
            }
            let mut m = transfer_apply::prepare(&line.memory, outcome);
            m.project = key.clone();

            // Reuse the embedding only on an exact match. The same model name
            // across two implementations produced vectors 0.76–0.87 apart from
            // each other's — close enough to look right and wrong enough to
            // rank everything incorrectly.
            let vector = match &line.embedding {
                Some(e) if e.model == model && e.dim == dim => {
                    reused += 1;
                    e.vector.clone()
                }
                _ => {
                    reembedded += 1;
                    let embedder = build_embedder(&cfg)?;
                    embedder.embed(&[m.content.clone()])?.remove(0)
                }
            };
            m.vector_id = m.id.clone();
            store.upsert_memory(&m)?;
            store.upsert(&[devctx_core::VectorPoint {
                id: m.id.clone(),
                vector,
                text: m.content.clone(),
                metadata: devctx_core::VectorMetadata {
                    memory_type: m.memory_type.clone(),
                    memory_scope: m.scope.clone(),
                    memory_tags: m.tags.clone(),
                    chunk_level: "memory".to_string(),
                    symbol: m.title.clone(),
                    language: "memory".to_string(),
                    ..Default::default()
                },
            }])?;
            existing.push(m);
        }

        let what = if dry_run { "would import" } else { "imported" };
        println!(
            "{what} {} memories into `{key}` · {} already present",
            report.added, report.already
        );
        if reused > 0 || reembedded > 0 {
            println!("  embeddings: {reused} reused ({model}/{dim}), {reembedded} recomputed");
        }
        if !report.collisions.is_empty() {
            println!("  {} topic collisions kept separately:", report.collisions.len());
            for t in &report.collisions {
                println!("    · {t}");
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Build**

```bash
cargo build --release --offline -j 3 2>&1 | grep -A 6 '^error' | head -20
```

Expected: no output. Fix anything reported before continuing — in particular check the exact name of the embed method (`embed`) and of `VectorMetadata`'s fields against `crates/devctx-core/src/types.rs`.

- [ ] **Step 5: Verify against the real store**

```bash
devctx serve --stop; devctx serve --central --stop
install -m755 target/release/devctx ~/.local/bin/devctx
cd ~/revfa/REVFA_BackEnd

# Export the group and count it against what the store holds.
devctx memories export --scope group > /tmp/revfa.jsonl
wc -l /tmp/revfa.jsonl                 # expect 2090
head -1 /tmp/revfa.jsonl | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["title"], "| embedding:", d.get("embedding",{}).get("model"))'

# Importing what is already there must change nothing.
devctx memories import /tmp/revfa.jsonl --dry-run
```

Expected: `wc -l` reports 2090; the first line shows a title and `ml-granite`; the dry run reports **0 added, 2090 already present**. Anything else means `decide` is not recognising the store's own rows and must be fixed before committing.

- [ ] **Step 6: Verify the additive rule with a real collision**

```bash
cd ~/revfa/REVFA_BackEnd
devctx remember "local version of a shared topic" --title "COLLISION LOCAL" --topic collision-test --scope group
devctx memories export --scope group | grep -c COLLISION      # expect 1

python3 - <<'EOF'
import json
line = None
for l in open('/tmp/revfa.jsonl'):
    d = json.loads(l)
    if d['topic_key']:
        d.update(id='mem_collision_probe', title='COLLISION INCOMING',
                 content='a different thing about the same topic',
                 normalized_hash='probe_hash_unique', topic_key='collision-test')
        line = json.dumps(d); break
open('/tmp/collide.jsonl','w').write(line + '\n')
EOF

devctx serve --central --stop
devctx memories import /tmp/collide.jsonl
devctx recall "COLLISION" --limit 5 | grep COLLISION
```

Expected: the import reports **1 topic collision kept separately**, and the recall shows **both** `COLLISION LOCAL` and `COLLISION INCOMING`. If either is missing, the rule is broken.

- [ ] **Step 7: Clean up the probes**

```bash
devctx serve --central --stop
devctx memory-forget mem_collision_probe
devctx recall "COLLISION LOCAL" --limit 3      # find its id
devctx memory-forget <the id printed above>
```

- [ ] **Step 8: Commit**

```bash
cargo fmt --all && cargo clippy --offline --all-targets -j 3 -- -D warnings
cargo test --offline -j 3 2>&1 | grep -E 'test result|FAILED'
git add crates/devctx-cli/src/main.rs
git commit -m "feat(cli): memories export and import

Answers the want behind a database per group — handing one product's
memories to someone without the rest — without splitting where they live.

Embeddings are reused only when the model name and width both match, and
recomputed otherwise: re-embedding 2090 memories was measured at 46 minutes,
so discarding usable vectors is expensive, and vectors from another
implementation of the same nominal model scored 0.76-0.87 against their own
re-embedding, so trusting the name alone ranks everything wrongly."
```

---

### Task 6: The init wizard — model, with the machine's own context

**Files:**
- Modify: `crates/devctx-cli/src/models.rs` (replace `prompt`)
- Modify: `crates/devctx-cli/src/main.rs` (`cmd_init` calls it with registry facts)
- Test: `crates/devctx-cli/src/models.rs` (its own `mod tests`)

**Interfaces:**
- Consumes: `devctx_central::Central::list`, `ProjectRecord.embed_model`.
- Produces: `fn prompt(default_key: &str, in_use: &[(String, usize)]) -> Result<Option<String>>` where each tuple is `(model_key, how_many_projects_use_it)`.

- [ ] **Step 1: Write the failing test**

In `crates/devctx-cli/src/models.rs`, add a `mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The line above the table is the whole point of asking: someone setting
    /// up their fifth repository should be told what the other four use, in
    /// one line, before being offered eight alternatives.
    #[test]
    fn the_summary_names_the_models_already_in_use() {
        let s = in_use_summary(&[("ml-granite".to_string(), 4), ("bge-base".to_string(), 1)]);
        assert!(s.contains("ml-granite"), "{s}");
        assert!(s.contains('4'), "the count matters: {s}");
        assert!(s.contains("bge-base"), "every model in use, not just the commonest: {s}");
    }

    /// A first project has nothing to compare against and must not be shown an
    /// empty table header pretending otherwise.
    #[test]
    fn a_first_project_gets_no_summary() {
        assert!(in_use_summary(&[]).is_empty());
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cargo test --offline -p devctx-cli in_use_summary
```

Expected: FAIL — `in_use_summary` does not exist.

- [ ] **Step 3: Implement the summary and extend `prompt`**

In `crates/devctx-cli/src/models.rs`:

```rust
/// One line naming the models this machine already indexes with.
///
/// Empty for a first project: a header over nothing reads as "no other
/// projects use a model", which is a different and wrong claim.
pub fn in_use_summary(in_use: &[(String, usize)]) -> String {
    if in_use.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = in_use
        .iter()
        .map(|(model, n)| {
            let repos = if *n == 1 { "repository" } else { "repositories" };
            format!("{model} ({n} {repos})")
        })
        .collect();
    format!("Already in use on this machine: {}", parts.join(", "))
}
```

Change `prompt`'s signature and body:

```rust
pub fn prompt(default_key: &str, in_use: &[(String, usize)]) -> Result<Option<String>> {
    use std::io::{IsTerminal as _, Write as _};
    if !std::io::stdin().is_terminal() {
        return Ok(None);
    }
    let summary = in_use_summary(in_use);
    if !summary.is_empty() {
        println!("{summary}");
        println!(
            "Matching one of those keeps a single model in memory for processes \
             that touch both a project and the shared memories.\n"
        );
    }
    list(Some(default_key))?;
    print!("\nModel to use [{default_key}]: ");
    std::io::stdout().flush().ok();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Ok(None);
    }
    let key = match line.trim() {
        "" => default_key.to_string(),
        chosen => chosen.to_string(),
    };
    let spec = find_local(&key)
        .ok_or_else(|| anyhow!("unknown model `{key}`; run `devctx models` to see what there is"))?;
    if spec.builtin.is_none() && local_dir(&key).is_none() {
        eprintln!("`{key}` needs its files; fetching them now.");
        download(&key)?;
    }
    Ok(Some(key))
}
```

- [ ] **Step 4: Feed it the registry in `cmd_init`**

In `crates/devctx-cli/src/main.rs`, add beside `central_defaults`:

```rust
/// How many registered projects use each embedding model.
///
/// Read from the registry rather than by opening each project: the registry
/// caches every project's model, and a wizard that opened four databases to
/// print one line would be slow for no reason. It can be stale if someone
/// edited a config by hand — `projects refresh` is the cure, and the wizard
/// says the figure comes from the registry.
fn models_in_use() -> Vec<(String, usize)> {
    let Ok(central) = Central::open() else {
        return Vec::new();
    };
    let Ok(projects) = central.list(false) else {
        return Vec::new();
    };
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for p in projects {
        if !p.embed_model.is_empty() {
            *counts.entry(p.embed_model).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    // Commonest first: the answer someone most likely wants is the one most of
    // their repositories already gave.
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}
```

and change the call in `cmd_init`:

```rust
        None => models::prompt(&defaults.embeddings.model, &models_in_use())?,
```

- [ ] **Step 5: Run the tests and watch them pass**

```bash
cargo test --offline -p devctx-cli in_use_summary
cargo build --release --offline -j 3 2>&1 | grep -A 6 '^error' | head
```

Expected: 2 passed, build clean.

- [ ] **Step 6: Verify both paths by hand**

```bash
devctx serve --central --stop
install -m755 target/release/devctx ~/.local/bin/devctx
mkdir -p /tmp/wiz && cd /tmp/wiz && git init -q .

# Non-interactive: must not prompt, must not hang.
devctx init --name wiz < /dev/null
grep 'model:' .devctx/config.yaml

devctx projects rm wiz; rm -rf /tmp/wiz
```

Expected: it prints the two init lines and exits immediately with `ml-granite`. A hang here means the TTY check is wrong, and an agent following `AGENTS.md` would hang the same way.

Then interactively, in your own terminal (not through this session):

```
cd /tmp && mkdir wiz2 && cd wiz2 && git init -q . && devctx init --name wiz2
```

Expected: the summary line names `ml-granite (4 repositories)`, the table follows, and pressing Enter accepts it.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all && cargo clippy --offline --all-targets -j 3 -- -D warnings
git add crates/devctx-cli/src/models.rs crates/devctx-cli/src/main.rs
git commit -m "feat(cli): init reports what the machine already indexes with

The model question was one prompt with no context. Someone setting up their
fifth repository should be told what the other four use before being offered
eight alternatives — read from the registry, which already caches each
project's model, rather than by opening four databases to print one line."
```

---

### Task 7: The init wizard — storage, group, and the confirmation

**Files:**
- Create: `crates/devctx-cli/src/init_wizard.rs`
- Modify: `crates/devctx-cli/src/main.rs` (`cmd_init` uses it; add `--state-dir` and `--yes`)
- Test: `crates/devctx-cli/src/init_wizard.rs` (its own `mod tests`)

**Interfaces:**
- Consumes: `models::prompt`, `models_in_use`, `devctx_central::Central::list`.
- Produces:
  - `struct Answers { model: Option<String>, state_dir: Option<String>, group: Option<String> }`
  - `fn ask(defaults: &Embeddings, in_use: &[(String, usize)], groups: &[(String, usize)]) -> Result<Answers>`
  - `fn summary(name: &str, a: &Answers, model: &str) -> String`

- [ ] **Step 1: Write the failing tests**

Create `crates/devctx-cli/src/init_wizard.rs`:

```rust
//! The questions `devctx init` asks, and the summary it shows before writing.
//!
//! Every question defaults to what the machine already does, so pressing Enter
//! through all of them reproduces the previous non-interactive behaviour
//! exactly. Skipped entirely without a terminal: an agent following the setup
//! guide runs `init` with no TTY, and a prompt it cannot answer would hang the
//! setup it was told to perform.

#[cfg(test)]
mod tests {
    use super::*;

    /// The summary is the only place the three memory tiers are explained at
    /// the moment someone is choosing between them.
    #[test]
    fn the_summary_explains_where_each_kind_of_memory_will_live() {
        let a = Answers {
            model: Some("ml-granite".into()),
            state_dir: None,
            group: Some("REVFA".into()),
        };
        let s = summary("demo", &a, "ml-granite");
        assert!(s.contains("REVFA"), "{s}");
        assert!(s.contains("local"), "the tiers must be named: {s}");
        assert!(s.contains("group"), "{s}");
        assert!(s.contains("global"), "{s}");
    }

    /// A project in no group must not be shown a group line pretending it has
    /// one, and must still be told where its memories go.
    #[test]
    fn a_project_without_a_group_says_so() {
        let a = Answers {
            model: Some("minilm-l6".into()),
            state_dir: None,
            group: None,
        };
        let s = summary("solo", &a, "minilm-l6");
        assert!(s.contains("none"), "the group line must read as absent: {s}");
        assert!(!s.contains("shared with"), "nothing is shared: {s}");
    }

    /// Groups are offered from what exists, so joining one is a choice from a
    /// list rather than a name that has to be remembered exactly.
    #[test]
    fn known_groups_are_offered() {
        let s = groups_line(&[("REVFA".to_string(), 4)]);
        assert!(s.contains("REVFA"), "{s}");
        assert!(s.contains('4'), "{s}");
        assert!(groups_line(&[]).is_empty(), "nothing to offer on a fresh machine");
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

```bash
cargo test --offline -p devctx-cli init_wizard 2>&1 | tail -5
```

Expected: FAIL — module not wired, `Answers`/`summary`/`groups_line` do not exist.

- [ ] **Step 3: Wire the module and implement it**

In `crates/devctx-cli/src/main.rs`: `mod init_wizard;`

At the top of `crates/devctx-cli/src/init_wizard.rs`:

```rust
use std::io::{IsTerminal as _, Write as _};

use anyhow::Result;
use devctx_core::config::Embeddings;

use crate::models;

/// What the wizard collected. `None` means "leave the default alone".
#[derive(Debug, Default, Clone)]
pub struct Answers {
    pub model: Option<String>,
    pub state_dir: Option<String>,
    pub group: Option<String>,
}

/// One line offering the groups that already exist.
pub fn groups_line(groups: &[(String, usize)]) -> String {
    if groups.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = groups
        .iter()
        .map(|(g, n)| format!("{g} ({n})"))
        .collect();
    format!("Groups on this machine: {}", parts.join(", "))
}

/// Read one line, returning the trimmed answer or the default when empty.
fn ask_line(question: &str, default: &str) -> Option<String> {
    print!("{question} [{default}]: ");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    if std::io::stdin().read_line(&mut s).is_err() {
        return None;
    }
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Ask everything, or nothing when there is no terminal.
pub fn ask(
    defaults: &Embeddings,
    in_use: &[(String, usize)],
    groups: &[(String, usize)],
) -> Result<Answers> {
    if !std::io::stdin().is_terminal() {
        return Ok(Answers::default());
    }
    let model = models::prompt(&defaults.model, in_use)?;

    println!(
        "\nThe index is a build artefact — large, binary, rebuilt from the \
         repository — so it lives inside it by default and is git-ignored."
    );
    let state_dir = ask_line("Index directory (blank = inside the repository)", "repo");
    let state_dir = state_dir.filter(|s| s != "repo");

    println!("\nMemories can be shared between the repositories of one product.");
    let line = groups_line(groups);
    if !line.is_empty() {
        println!("{line}");
    }
    let group = ask_line("Group for this repository", "none");
    let group = group.filter(|g| g != "none");

    Ok(Answers {
        model,
        state_dir,
        group,
    })
}

/// What will be written, in the terms the decisions were made in.
pub fn summary(name: &str, a: &Answers, model: &str) -> String {
    let group = match &a.group {
        Some(g) => format!("{g}  → memories shared with that product's repositories"),
        None => "none".to_string(),
    };
    let index = match &a.state_dir {
        Some(d) => d.clone(),
        None => "./.devctx/state/index.duckdb  (HNSW on)".to_string(),
    };
    let tiers = match &a.group {
        Some(g) => format!(
            "local → this repository · group ({g}) → central store · global → central store"
        ),
        None => "local → this repository · global → central store".to_string(),
    };
    format!(
        "  project   {name}\n  group     {group}\n  model     {model}\n  index     {index}\n  memories  {tiers}"
    )
}

/// Ask for confirmation. `true` without a terminal, since there is nobody to
/// confirm and the caller asked for this by running the command.
pub fn confirm() -> bool {
    if !std::io::stdin().is_terminal() {
        return true;
    }
    print!("\nWrite this? [Y/n]: ");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    if std::io::stdin().read_line(&mut s).is_err() {
        return true;
    }
    !matches!(s.trim().to_ascii_lowercase().as_str(), "n" | "no")
}
```

- [ ] **Step 4: Use it in `cmd_init`**

Add `--state-dir` and `--yes` to the `Init` variant:

```rust
        /// Directory for this project's index. Default: inside the repository.
        #[arg(long)]
        state_dir: Option<String>,
        /// Skip the questions and the confirmation.
        #[arg(long)]
        yes: bool,
```

Thread them through the dispatch and `cmd_init`'s signature, then replace the model-prompt block in `cmd_init` with:

```rust
    let mut defaults = central_defaults();
    let answers = if yes {
        init_wizard::Answers {
            model: model.clone(),
            state_dir: state_dir.clone(),
            group: group.clone(),
        }
    } else {
        let mut a = init_wizard::ask(&defaults.embeddings, &models_in_use(), &groups_in_use())?;
        // Flags win over answers: someone who passed --model meant it.
        a.model = model.clone().or(a.model);
        a.state_dir = state_dir.clone().or(a.state_dir);
        a.group = group.clone().or(a.group);
        a
    };
    if let Some(key) = &answers.model {
        defaults.embeddings = choose_model(key, &defaults.embeddings)?;
    }
    println!(
        "\n{}",
        init_wizard::summary(&name, &answers, &defaults.embeddings.model)
    );
    if !yes && !init_wizard::confirm() {
        println!("Nothing written.");
        return Ok(());
    }
```

and use `answers.group` / `answers.state_dir` when building the `ProjectConfig`:

```rust
        project: Project {
            name: name.clone(),
            path: root.to_string_lossy().into_owned(),
            group: answers.group.clone().unwrap_or_default(),
        },
        state_dir: answers.state_dir.clone().unwrap_or_default(),
```

Add `groups_in_use` beside `models_in_use`:

```rust
/// The groups already in use, with how many repositories each holds.
///
/// Read from each registered project's config rather than the registry, which
/// caches the model but not the group.
fn groups_in_use() -> Vec<(String, usize)> {
    let Ok(central) = Central::open() else {
        return Vec::new();
    };
    let Ok(projects) = central.list(false) else {
        return Vec::new();
    };
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for p in projects {
        let Ok(cfg) = ProjectConfig::load(std::path::Path::new(&p.config_path)) else {
            continue;
        };
        if !cfg.project.group.is_empty() {
            *counts.entry(cfg.project.group).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}
```

- [ ] **Step 5: Run the tests and build**

```bash
cargo test --offline -p devctx-cli init_wizard
cargo build --release --offline -j 3 2>&1 | grep -A 6 '^error' | head
```

Expected: 3 passed, build clean.

- [ ] **Step 6: Verify non-interactive first — this is the regression that matters**

```bash
devctx serve --central --stop
install -m755 target/release/devctx ~/.local/bin/devctx
mkdir -p /tmp/wiz3 && cd /tmp/wiz3 && git init -q .
timeout 30 devctx init --name wiz3 < /dev/null; echo "exit=$?"
cat .devctx/config.yaml | grep -E 'group:|model:|hnsw:'
cd /tmp && devctx projects rm wiz3; rm -rf /tmp/wiz3
```

Expected: `exit=0` immediately (not 124, which is the timeout firing), `group: ''`, `model: ml-granite`, `hnsw: true`. A timeout here means an agent running `init` would hang.

Then in your own terminal, interactively: `devctx init --name wiz4` and press Enter through every question. Expect the same config, plus the summary and confirmation.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all && cargo clippy --offline --all-targets -j 3 -- -D warnings
cargo test --offline -j 3 2>&1 | grep -E 'test result|FAILED'
git add crates/devctx-cli/src/init_wizard.rs crates/devctx-cli/src/main.rs
git commit -m "feat(cli): init asks where the index and the memories go

Two of init's decisions are expensive to undo and were made silently: the
embedding model, which fixes the width of every vector, and the group, which
decides who can recall what. Each question defaults to what the machine
already does, so pressing Enter through all of them reproduces the previous
behaviour exactly, and the whole wizard is skipped without a terminal so an
agent running init is never left waiting on a prompt."
```

---

### Task 8: Document it and cut the release

**Files:**
- Modify: `AGENTS.md` (the setup procedure gains the wizard and the transfer commands)
- Modify: `README.md` (the memories section gains export/import)
- Modify: `Cargo.toml` (version)

- [ ] **Step 1: Update `AGENTS.md`**

In §2, after the model table, add:

```markdown
`devctx models` prints this table on the machine itself, marking which model is
configured and which need files. `devctx models --download <model>` fetches
one. `devctx init` asks, offers what the other repositories already use, and
downloads what it must — unless it is run without a terminal, in which case it
takes the machine default and asks nothing.
```

In §5, after the migration section, add:

```markdown
### Moving memories between machines

```bash
devctx memories export --scope group > product.jsonl
devctx memories import product.jsonl --dry-run
devctx memories import product.jsonl
```

Import only ever adds. A memory whose content is already present is skipped; one
whose topic key belongs to a different local memory is kept alongside it rather
than replacing it, and named in the summary. Embeddings in the file are reused
only when the model and width match exactly, and recomputed otherwise — which
takes about a minute per 45 memories.
```

- [ ] **Step 2: Update `README.md`**

In the "Across projects" section, after the `devctx recall` examples:

```markdown
```bash
devctx memories export --scope group > product.jsonl   # hand a product's memories to someone
devctx memories import product.jsonl                   # adds; never overwrites
```
```

- [ ] **Step 3: Bump the version**

In `Cargo.toml`, `[workspace.package]`: `version = "0.2.0"`.

A minor bump, not a patch: `init` behaves differently and there are new commands.

- [ ] **Step 4: Validate everything, in this order**

```bash
cd ~/personal/DevCtxEngine
cargo fmt --all
cargo clippy --offline --all-targets -j 3 -- -D warnings
cargo test --offline -j 3 2>&1 | grep 'test result: ok' | awk '{s+=$4} END {print "TOTAL:",s}'
cargo fmt --all --check && echo "fmt clean at the end"
```

All four must be clean. The last one is not redundant: formatting early and then editing is exactly how CI went red three times.

- [ ] **Step 5: Commit, tag, and watch**

```bash
git add AGENTS.md README.md Cargo.toml
git commit -m "docs: document model selection and memory transfer; v0.2.0"
git push origin main
git tag -a v0.2.0 -m "v0.2.0

An interactive init that reports what the machine already does, memory export
and import as JSONL, and HNSW on by default for new projects."
git push origin v0.2.0
```

Then confirm both workflows go green before calling it done:

```bash
curl -s "https://api.github.com/repos/snaven10/DevCtxEngine/actions/runs?per_page=4" \
  | python3 -c "import json,sys; [print(r['name'], r['head_branch'], r['status'], r['conclusion']) for r in json.load(sys.stdin)['workflow_runs'][:4]]"
```

- [ ] **Step 6: Verify the published artefact**

```bash
mkdir -p /tmp/verify && cd /tmp/verify
DEVCTX_BIN_DIR=$PWD/bin sh -c 'curl -fsSL https://raw.githubusercontent.com/snaven10/DevCtxEngine/main/install.sh | sh'
./bin/devctx --version        # expect 0.2.0, matching the tag
./bin/devctx models | head -3
cd /tmp && rm -rf /tmp/verify
```

A version that does not match the tag means `devctx update` will upgrade to itself forever.

---

## Notes for whoever executes this

- **Tasks 1–5 are independent of 6–7.** If the wizard turns out to need more design, the defaults fix and the transfer commands still ship on their own.
- **The verification steps that use the real store are not optional.** Three bugs in this codebase — a broken index, a silent model fallback, a watchdog killing an index — all passed their unit tests. The manual checks are where those were caught.
- **Stop the servers before any manual check.** A running server holds the old binary and the database file; both will make a correct change look broken.
