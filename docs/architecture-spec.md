# Architecture Specification

> 🇬🇧 Referenced from crate-level docs across the workspace by section number
> (`See docs/architecture-spec.md §4`). **Section numbers are stable** — add
> sections, do not renumber them.

What DevCtxEngine is: one Rust binary over one DuckDB file. This document
describes the shipped system, not a plan.

---

## 1. Decisions that shape everything else

| Topic | Decision |
|---|---|
| Runtime | **One Rust binary.** Parsing, chunking, embedding, reranking, storage and MCP all in-process. The only external program invoked is `git`. |
| ML layer | **fastembed + `ort` (ONNX Runtime)** for local models; OpenAI / Voyage / custom over HTTP. |
| Database | **One DuckDB file per project** — vectors (VSS extension) and relational tables (graph, routes, memories, index state) together. |
| Vector width | **Parameterised, not fixed.** The `vector FLOAT[N]` column is sized per database from the active model. |
| Sharing | **Local-first.** Each repository owns its store. Only the project registry and global/group memories live centrally. |

Rationale for each, with costs, is in [Design Decisions](08-design-decisions.md).

## 2. Crate layout

```
DevCtxEngine/
├─ Cargo.toml                 # [workspace]
└─ crates/
   ├─ devctx-cli/             # the `devctx` binary — clap
   ├─ devctx-core/            # shared types, config (.devctx/config.yaml), errors
   ├─ devctx-store/           # DuckDB: vectors (VSS) + graph + routes + memories + state
   ├─ devctx-embed/           # fastembed/ort local + OpenAI/Voyage/custom; model registry
   ├─ devctx-parse/           # tree-sitter grammars + route extractors
   ├─ devctx-chunk/           # semantic chunker + memory chunker
   ├─ devctx-rerank/          # cross-encoder reranking (opt-in)
   ├─ devctx-search/          # retrieval orchestration: vector / keyword / hybrid
   ├─ devctx-memory/          # remember + recall, and the memory↔graph junction
   ├─ devctx-summarize/       # extractive (default) + optional providers
   ├─ devctx-index/           # pipeline: git diff → parse → chunk → embed → store
   ├─ devctx-mcp/             # MCP server (rmcp)
   ├─ devctx-central/         # project registry + shared memories
   ├─ devctx-api/             # HTTP API (axum)
   └─ devctx-tui/             # terminal UI (ratatui)
```

Only five are extension points; see [Extending the System](06-extending-the-system.md).

## 3. Chunking

Handled by `devctx-chunk`. Chunks are cut on symbol boundaries from a
tree-sitter parse and **never split a symbol in half**.

Levels: `file`, `class`, `doc`, `function`, `block`.

| Setting | Default |
|---|---|
| `max_chunk_tokens` | 512 |
| `min_chunk_tokens` | 64 |
| `large_function_threshold` | 1024 |

Behaviours that are load-bearing:

- **Context header.** `function` and `block` chunks are prefixed
  `# file > class > method` before embedding, so an anonymous-looking body
  carries its location into the vector.
- **Small symbols group.** Anything under `min_chunk_tokens` merges with its
  neighbours rather than being embedded alone.
- **Doc chunks are conditional.** A doc comment that only restates the symbol
  name produces no chunk.
- **Large functions split on blank lines**, snapping to a blank line rather than
  cutting mid-statement.
- **Raw text** (unsupported languages) is chunked with overlap.
- **`content_hash`** is `sha256(text)[:16]`. This drives incremental skip and
  branch-copy dedup.

Token counts are estimated at ~4 characters per token.

## 4. Data design

One DuckDB database per project, with VSS loaded when available.

### 4.1 `vectors`

Holds every embedded chunk: text, level, symbol name and type, line range,
`content_hash`, `repo`, `branch`, and the `FLOAT[N]` vector itself.

Two derived indexes sit over it, both **opt-in and both rebuild-on-demand**:

- **HNSW** (VSS extension) — `storage.hnsw`, metric `cosine` or `ip`.
- **FTS/BM25** — `storage.fts`, over `vectors.text`.

Both are **dropped before a bulk index and rebuilt after**. DuckDB maintains
HNSW on every insert (8× slowdown measured), and cannot maintain FTS across row
deletions at all.

### 4.2 Relational tables

- `graph_edges(source, target, kind, source_file, target_file, line, repo, branch, metadata)`
  — UNIQUE(source, target, kind, repo, branch, source_file). **`kind` is always
  `calls`.** Feeds `impact_analysis` (BFS up/down) and `get_references`.
- `routes(framework, http_method, path, handler_*, file, line, repo, branch, indexed_at)`
  — UNIQUE(framework, http_method, path, repo, branch).
- `memories(...)` — deduplicated by `topic_key`, else by normalized content hash.
- `memory_symbol_references(memory_id, symbol, file, line, repo, branch, source)`
  — the memory↔code junction. `source` is `files-field`, `content-mention` or
  `inference`.
- `index_state` / `file_state` — incremental indexing (content-hash skip) and
  model-change detection.

### 4.3 The WAL rule

**Every path that ends a process checkpoints first.**

DuckDB replays the WAL on open, but a replayed append does not restore ART index
entries — the structure behind every `PRIMARY KEY` and `UNIQUE` here. The table
then holds rows the index never saw, and the next `DELETE` touching them aborts
permanently. Re-indexing cannot repair it, because re-indexing starts by
deleting. `devctx repair` rebuilds each table from its rows.

## 5. Embeddings

`devctx-embed` holds a `ModelRegistry` keyed by model name, carrying dimension
as data.

- **Local (fastembed/ort):** MiniLM-L6/L12 (384), BGE-small (384) / base (768),
  multilingual MiniLM (384) / mpnet (768), Granite (384) and Granite-lg (768).
  All L2-normalized. Models fastembed does not ship are loaded as user-defined
  ONNX (`model.onnx` + `tokenizer.json`).
- **API:** OpenAI, Voyage, custom (`POST {endpoint}/embed`).
- **RAM guards:** `DEVCTX_EMBED_MAX_CHARS` (4096) and `DEVCTX_EMBED_BATCH_SIZE`
  (32). They interact — one long chunk pads its whole batch.

**Dimension change forces a full reindex.** If the active model's width differs
from `index_state`, the `vector FLOAT[N]` column is recreated and everything is
re-embedded, memories included.

## 6. Retrieval

`devctx-search` centralises what CLI and MCP both need. Three modes:

- **Vector** — cosine nearest neighbours.
- **Keyword** — BM25 via `match_bm25`.
- **Hybrid** — Reciprocal Rank Fusion of both: `Σ 1/(k + rank)`, k = 60, ranks
  1-based, deduplicated by point id. **Degrades to vector-only when no FTS index
  exists**, rather than failing.

Reranking is optional and off by default. The candidate pool is
`max(limit, reranker.pool, 20)` — `reranking.pool` defaults to 100, and 20 is
the floor used when no reranker runs. The pool is both the ceiling on what
reranking can fix and the entirety of its cost. Any reranker failure degrades to retriever
order.

### Memory recall

Recall blends an intro vector with body-chunk vectors and fuses **by rank, never
by score**, across every applicable tier. Fetch depth is `limit × 8`, minimum 40.

## 7. Privacy and locality

- **Summarization defaults to extractive**, which runs locally and preserves
  identifiers verbatim. `require_local: true` **blocks non-local providers
  outright**, so a config edit cannot silently start shipping code to a third
  party.
- **No network service.** MCP is stdio; `api` and `web` bind to loopback. They
  exist to arbitrate a file lock, not to serve remote clients.
- **No shared vector store.** Per-project stores mean one repository's reindex
  never blocks another, each may use a different model, and no search needs a
  repo filter to be correct.

## 8. Concurrency and process model

DuckDB permits **one writer per file**, which is the constraint the rest of this
section exists to satisfy.

- **`devctx serve`** owns the connection. CLI commands and MCP sessions route to
  it. Spawned on demand, idles out.
- **Handshake** is a `serve.json` file. A server must only remove **its own**
  (pid-checked) — a failing server that deletes the file strands a healthy one.
- **`run_blocking`** keeps synchronous store work off the async executor in the
  MCP layer.

## 9. Branch model

Chunks, edges and routes are all keyed `(repo, branch)`.

- Tracked branches are **declared** in `indexing.branches`, not inferred. A
  repository with worktrees has several branches live at once, and guessing a
  base from the git graph fails silently in the ordinary two-siblings case.
- The **first entry is the default**: what `index` targets without `--branch`,
  and what search falls back to.
- An **empty list means "whatever is checked out"** — correct for a single-branch
  repository.
- Indexing a second branch **copies rows whose `content_hash` matches** rather
  than re-embedding. Measured 95–96% copy rate across three repositories.
- Indexing is **worktree-independent**: run it from any worktree, it updates the
  one index.

**Known caveat:** changing `indexing.exclude` is not reflected in `content_hash`,
so branch-copy dedup can carry rows the new exclusions would drop. Run
`index --full` after changing it.

## 10. Testing

```bash
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
```

`--offline` is required — dependencies are vendored, DuckDB included.

Two rules this codebase learned the hard way:

- **Fixtures are cleaner than reality.** Multi-branch indexing passed 22 tests
  while broken in all three real paths, because the fixtures had one index per
  branch, no server, and no HNSW index.
- **Assert the effect, not the shape.** A test asserting a query returned `Some`
  passed while the pipeline copied nothing. A counting embedder — measuring how
  many texts were actually embedded — caught it at once.
