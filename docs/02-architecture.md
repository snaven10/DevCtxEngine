# Architecture

> 🇪🇸 [Leer en español](es/02-arquitectura.md)

DevCtxEngine is a single Rust binary over DuckDB. Everything — indexing,
embeddings, search, the MCP server, the HTTP API, the TUI — is one process image
with no runtime dependency beyond git.

---

## 1. Shape

A Cargo workspace of focused crates, each owning one concern:

| Crate | Owns |
|---|---|
| `devctx-core` | Shared types, the config schema, path resolution, rank fusion |
| `devctx-store` | DuckDB: vectors, call graph, routes, memories, index state, registry |
| `devctx-parse` | tree-sitter: symbols, imports, call edges, framework routes |
| `devctx-chunk` | Semantic chunking: file / class / function / block, plus memory windows |
| `devctx-embed` | Embeddings: local ONNX via fastembed, or OpenAI / Voyage / custom |
| `devctx-rerank` | Cross-encoder reranking, with a no-op fallback |
| `devctx-index` | The pipeline: select → parse → chunk → embed → store |
| `devctx-search` | Vector / keyword / hybrid retrieval and reranking |
| `devctx-memory` | remember (dedup, revise) and recall (intro + chunk blend) |
| `devctx-summarize` | Extractive by default; OpenAI or local flan-t5 optionally |
| `devctx-central` | The central store: project registry, global memories, daemon client |
| `devctx-mcp` | MCP server (stdio) and the tool implementations everything reuses |
| `devctx-api` | HTTP API (axum) over those same implementations, plus the daemon |
| `devctx-tui` | Terminal UI (ratatui) |
| `devctx-cli` | The `devctx` binary |

The dependency direction is strict: `core` at the bottom, `cli` at the top,
nothing pointing back down. `devctx-mcp` holds the tool bodies (`do_search`,
`do_recall`, …) and `devctx-api` calls the same functions, so the MCP server, the
HTTP API and the CLI cannot drift apart — there is one implementation of each
operation, not three.

## 2. One writer per database

DuckDB permits a single read-write process per file. That constraint shapes the
whole runtime.

```
   MCP session      CLI command       TUI          web dashboard
        |                |             |                |
        +--------+-------+------+------+--------+-------+
                          |  HTTP (loopback)
                 devctx serve  ...........  owns index.duckdb
                          |                  keeps the model warm
                 .devctx/state/index.duckdb
```

The first command that needs the database spawns a server in the background,
advertises it in `.devctx/state/serve.json`, and routes to it. Every later
command — from any process — finds that file and routes too. Nothing ever fights
the lock, several agent sessions can share one project, and you can query while
an index runs. The server idles out after 15 minutes.

When no server can be started, commands open the store directly. That is correct
for a lone command and keeps the tool usable in constrained environments;
`DEVCTX_NO_AUTOSERVE=1` forces it.

The **central store** follows the same pattern with one difference: it is a
singleton, shared by every project, so `devctx serve --central` is the only
writer and a second one is refused rather than raced. See
[The Central Store](12-central-store.md).

## 3. Indexing

```
  work tree ──► select ──► parse ──► chunk ──► embed ──► store
                  │          │         │         │         │
             git diff,   tree-sitter  file/    ONNX or   vectors +
             untracked,  symbols +    class/   API       graph +
             or explicit call edges   function           routes +
             paths                    /block             file_state
```

**Selection** is the only part that consults git: the diff since the last indexed
commit, plus untracked files git does not ignore. A full run lists the whole work
tree; an explicit path list skips git entirely, which is what a file watcher
needs — a save moves no commit, so a commit diff would be empty.

**Skipping** happens per file by content hash. Re-indexing an unchanged file
costs a read and a hash, not an embedding. This is what makes the post-commit
hook and the watcher cheap enough to run constantly.

**State** lives in two tables keyed by `(repo_path, branch)`: `index_state` (the
last indexed commit, the model and its dimension) and `file_state` (per-file
hash, language, symbol and chunk counts). A model change is detected here and
forces a full re-index rather than mixing incompatible vectors.

## 4. Storage

One DuckDB file per project holds everything:

| Table | Holds |
|---|---|
| `vectors` | Chunk embeddings (`FLOAT[n]`) plus their metadata — the only dimension-bound table |
| `graph_edges` | Call and import edges, for impact analysis |
| `routes` | Framework-extracted HTTP routes and their handlers |
| `memories` | Saved decisions, insights and notes |
| `index_state`, `file_state` | Incremental bookkeeping |
| `projects` | The registry — populated only in the central store |

Vector search is `array_cosine_distance` over `FLOAT[n]`, a core DuckDB function
needing no extension. Two optional extensions add an HNSW index (VSS) for
approximate search and a BM25 index (FTS) for keyword search; both degrade to
brute force when unavailable rather than failing.

The vector column's width is fixed when the table is created, which is why
changing an embedding model means re-indexing, and why the central store refuses
to open if its configured memory model no longer matches what is on disk.

## 5. Retrieval

Three modes, one path:

- **Vector** — embed the query, cosine search, rerank.
- **Keyword** — BM25 over chunk text, no model needed.
- **Hybrid** — both, fused by reciprocal rank.

Fusion is by **rank**, never by score, and the same helper does it everywhere
(`devctx_core::fuse_by_rank`). Scores from a vector similarity and a BM25 weight
are not comparable; neither are two vector scores from different embedding
models, which is what makes rank fusion the right primitive for blending a
project's memories with the shared global ones.

Reranking runs a cross-encoder over the top candidates. It is the slowest stage,
so the TUI skips it for responsiveness and `--no-rerank` disables it.

## 6. Memory

A memory is stored twice: as an intro vector covering title plus content, and as
sliding body windows for long ones. Recall blends the two —
`α·intro + (1-α)·best_chunk` — so a long memory is found by any part of it
without a short one being drowned out.

Deduplication is by normalised content hash, or by topic key when given, so
saving the same thing twice bumps a counter instead of adding a row.

Scope decides the destination: `local` memories stay in the project, `global`
ones go to the central store where every project can recall them. Global identity
deliberately excludes the contributing project, so the same lesson learned in two
repositories converges on one memory — with the origin kept as provenance.

## 7. Interfaces

| Surface | Transport | Notes |
|---|---|---|
| CLI | — | Routes to the server; falls back to direct |
| MCP | stdio JSON-RPC | What agents use; routes to the server too |
| HTTP API | axum, loopback | Optional Bearer token; the dashboard is served from it |
| TUI | ratatui | Four views; long work on a worker thread |
| Web | HTTP | Self-contained page, vendored offline |

All of them reach the same `do_*` functions. Adding an operation means writing it
once and exposing it, not implementing it per surface.

## 8. Where things live

```
<repo>/.devctx/
  config.yaml          project config (worth committing)
  .gitignore           keeps state/ out of git
  state/
    index.duckdb       this project's index
    serve.json         the running server, if any

~/.local/share/devctx/
  central.duckdb       registry + global memories
  serve.json           the central daemon
  models/              downloaded models, shared by every project

~/.config/devctx/
  config.yaml          central config
```

## 9. What is deliberately not here

**No network service.** Everything binds to loopback and exists to arbitrate a
file lock, not to serve remote clients.

**No background daemon by default.** Servers are spawned on demand and idle out.
The central scheduler that re-indexes on a timer is opt-in.

**No shared vector store across projects.** An earlier design pointed every
repository at one database. Per-project stores mean re-indexing one repository
never blocks another, each may use a different model, and no search needs a repo
filter to be readable. Only what has no single owner — the registry and global
memories — is shared.

**No Python.** Earlier versions ran a Python ML sidecar over JSON-RPC. Parsing,
chunking, embedding and reranking are all in-process now; the only external
program invoked is `git`.
