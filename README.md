# DevCtxEngine

A local-first code context engine in Rust + DuckDB: incremental semantic
indexing, hybrid search, an MCP server, an HTTP API and a TUI. See
[`docs/rust-rewrite-plan.md`](docs/rust-rewrite-plan.md) for the architecture and
phased plan.

> **This is the Rust rewrite of [snaven10/devai-context-engine](https://github.com/snaven10/devai-context-engine)**
> — the original Go + Python implementation. See [Lineage](#lineage) for what
> changed and why.

```bash
devctx init --name myproj
devctx index                       # git diff → parse → chunk → embed → store
devctx search "connect to a database" --limit 5
devctx remember "We chose Postgres for JSONB" --type decision --topic db-engine
devctx recall "which database did we pick"
devctx mcp                         # MCP server over stdio (for AI agents/editors)
devctx mcp configure --client cursor --scope project   # register in an AI client
devctx tui                         # terminal UI: search, call-graph, memories, projects
devctx web                         # web dashboard: interactive call-graph + memories
```

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/snaven10/DevCtxEngine/main/install.sh | sh
```

Linux x86_64 and macOS arm64; anywhere else, `cargo build --release` (it
compiles DuckDB from source, so allow 20–25 minutes). The script installs the
binary to `~/.local/bin` and stops there — it writes no configuration and
downloads no model, because those are decisions, and the embedding model in
particular cannot be changed after indexing without redoing it.

### Let an agent set it up

The steps that follow — choosing a model, registering repositories, migrating
memories from an older DevAI install, indexing — are written out in
[`AGENTS.md`](AGENTS.md) for a coding agent to carry out. Paste this to yours:

> Set up DevCtxEngine on this machine for me. Read
> https://raw.githubusercontent.com/snaven10/DevCtxEngine/main/AGENTS.md and
> follow it in order, verifying each step the way it says to.
>
> Before you start, ask me: what language are the code and comments mostly in
> (that decides the embedding model, and changing it later means re-indexing
> everything); which repositories to register, and whether they are one product
> that should share a memory group; and whether there is an existing `devai`
> install whose memories should be migrated.
>
> Several failures in this system are silent — a model whose name matches but
> whose vectors do not, an index that is never built, a reranker pointed at the
> wrong model. Do not report a step as done because a command exited zero; run
> the verification the document gives and show me its output.

## Across projects

Each repository keeps its own index. What is shared lives in a **central store**:
a registry of every project, and the memory worth carrying between them — so an
agent working in one repository knows the others exist and can recall what was
learned there.

```bash
devctx projects add ~/code/api     # register a repository (init does this too)
devctx projects list               # name · model · index freshness · path

devctx remember "always verify webhook signatures" --scope global
devctx recall "how do I validate a webhook"        # this project + the shared ones
devctx recall "..." --scope global --repo api      # only what `api` contributed
```

A global memory saved from one repository is recalled from any other, and the
same lesson saved twice converges on one memory rather than two. Anything left
`local` — the default — never leaves its project.

Memories move between machines as JSONL, which any version can read:

```bash
devctx memories export --scope group > product.jsonl   # one product's memories
devctx memories import product.jsonl                   # only ever adds
```

Import never overwrites: content already present is skipped, and a memory whose
topic key belongs to a different local one is kept beside it rather than
replacing it.

Over MCP this is `list_projects`, `search_project`, and `scope` on
`recall`/`remember`. See [The Central Store](docs/12-central-store.md).

## Keeping the index fresh

```bash
devctx hooks install               # re-index after each commit
devctx watch                       # re-index files as they are saved
devctx reindex --all               # every registered project
```

The index mirrors the **work tree**, not the last commit: a file you have written
but not committed is indexed like any other, and a full re-index does not throw it
away. See [Keeping the index fresh](docs/13-keeping-the-index-fresh.md).

## Server mode

DuckDB allows a single read-write process. Run `devctx serve`
and it becomes the sole owner of the database; every other `devctx` command
discovers it (via `.devctx/state/serve.json`) and routes over HTTP instead of
opening the file, so concurrent CLI/editor/web use never hits a lock. When no
server is running, commands open the store directly as usual.

Every DB command (`search`, `recall`, `remember`, `summarize`, `index`,
`impact`, `status`, `memory-stats`, `routes`), the TUI, the web dashboard **and
the MCP server** route through one shared server, **auto-spawning** it on first
use. The server is the single owner of the DB, so nothing ever fights the lock —
you can run several Claude Code sessions (each an MCP client), the web dashboard,
the TUI and CLI commands against the same project at once, and query while an
`index` runs (readers see a consistent snapshot). The embedding model stays warm,
so repeated commands return in milliseconds. The daemon idles out after 15
minutes; stop it explicitly with `devctx serve --stop`, or disable auto-spawn
with `DEVCTX_NO_AUTOSERVE=1`.

The central store is a singleton and follows the same pattern: `devctx serve
--central` owns it, auto-spawned on demand.

`devctx web` serves a self-contained dashboard (call-graph via a vendored,
offline cytoscape build + a memories browser) and opens it in your browser.
`devctx tui` is the terminal equivalent, with four views on F1–F4: search, graph,
memories (with a scope selector) and projects — where you can register and index
a repository without leaving the UI.

Because the server holds the loaded code, a rebuilt binary does not take effect
until the running server is restarted (`devctx serve --stop`).

## Crates

| crate | responsibility | phase |
|-------|----------------|-------|
| `devctx-core`  | shared types, `.devctx/config.yaml` schema, errors | F0 |
| `devctx-cli`   | the `devctx` binary (clap) | F0 |
| `devctx-store` | DuckDB store: vectors (brute-force cosine) + relational schema | F1 |
| `devctx-embed` | embeddings: local (fastembed/ort) + OpenAI/Voyage/custom | F2 |
| `devctx-parse` | tree-sitter symbols/imports/call-edges + framework route extractors | F3 |
| `devctx-chunk` | semantic multi-level chunker (file/class/function/block) | F3 |
| `devctx-index` | pipeline: git diff → parse → chunk → embed → store (incremental) | F4 |
| `devctx-rerank` | cross-encoder reranking (fastembed BGE) + no-op fallback | F5 |
| `devctx-search` | search orchestration: vector / keyword / hybrid (RRF) + rerank | F8 |
| `devctx-mcp` | MCP server (rmcp, stdio): search / read_file / index_repo / index_status | F6 |
| `devctx-memory` | memory engine: remember (dedup) + recall (intro/chunk blend) | F7 |
| `devctx-summarize` | summarization: extractive (default) + OpenAI + local flan-t5 | F9 |
| `devctx-api` | HTTP REST API (axum) reusing the MCP engine, Bearer-token auth | F9 |
| `devctx-tui` | interactive terminal UI (ratatui): search, graph, memories, projects | F9 |
| `devctx-central` | central store: project registry, global memories, daemon client | — |

## Documentation

- [Configuration](docs/11-configuration.md) — project and central config, environment variables, MCP clients
- [The Central Store](docs/12-central-store.md) — registry, global memories, the daemon
- [Keeping the index fresh](docs/13-keeping-the-index-fresh.md) — hooks, watch, reindex, exclusions
- [Architecture](docs/02-architecture.md) · [Models & tuning](docs/09-models-and-tuning.md) · [Design decisions](docs/08-design-decisions.md)

🇪🇸 [Documentación en español](docs/es/README.md)

The `flan-t5` feature (off by default) adds a local abstractive summarizer via
candle — build with `--features flan-t5` (heavy; downloads the model on first use).

The `devctx-embed` `local` feature (default) pulls in `fastembed`/`ort`; build
with `--no-default-features` for an API-only build where the ONNX Runtime binary
can't be fetched.

## Build

```bash
cargo build
cargo test
cargo run -p devctx-cli -- status
```

## Lineage

DevCtxEngine is a ground-up rewrite of
**[snaven10/devai-context-engine](https://github.com/snaven10/devai-context-engine)**
(*DevAI*), which remains the reference implementation. The commit history here
carries over from that project, so the migration is visible in the log rather
than squashed away.

**What DevAI was** — a hybrid, ~20k LOC across two runtimes:

| Layer | Size | Responsibility |
|-------|------|----------------|
| Go | ~10.4k LOC | Thin orchestrator: CLI (cobra), MCP server (21 tools), HTTP API, TUI (Bubble Tea), config/storage routing |
| Python (`devai_ml`) | ~9.7k LOC | The actual work: embeddings, chunking, tree-sitter parsers, retrieval/reranking, summarization, stores |

The real contract between them was a **JSON-RPC 2.0 bridge over stdio** — about
27 methods — with Python running as a sidecar process.

**What the rewrite changes:**

- **One binary, no bridge.** The JSON-RPC-over-stdio hop and the Python sidecar
  are gone; every call is now in-process. That removes the interpreter startup,
  the respawn watchdog, the 120s timeout, and all the cross-process
  serialization.
- **One database.** DuckDB replaces LanceDB + Qdrant + SQLite — vectors (VSS)
  and relational tables (graph, routes, memories, index state) live in a single
  file.
- **Rust ML stack.** `fastembed-rs` + `ort` (ONNX Runtime) for local models;
  OpenAI/Voyage/custom over HTTP. Embedding dimension is parameterized rather
  than pinned at 384.
- **Incremental, with parity.** Built module by module, each phase verified
  against the Go/Python binary as the reference.

The full reasoning, the dependency mapping, and the phase breakdown live in
[`docs/rust-rewrite-plan.md`](docs/rust-rewrite-plan.md).

## License

MIT — see [LICENSE](LICENSE).
