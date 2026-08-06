# DevCtxEngine

A local-first code context engine in Rust + DuckDB: incremental semantic
indexing, hybrid search, an MCP server, an HTTP API and a TUI. See
[`docs/rust-rewrite-plan.md`](docs/rust-rewrite-plan.md) for the architecture and
phased plan.

> **Status: complete.** `devctx init`, `index` and `search` work end-to-end: the
> incremental pipeline indexes a repo and semantic search returns ranked results
> using a real local model. All rewrite phases (F0–F9) are done.

```bash
devctx init --name myproj
devctx index                       # git diff → parse → chunk → embed → store
devctx search "connect to a database" --limit 5
devctx search "greet a user" --format json
devctx remember "We chose Postgres for JSONB" --type decision --topic db-engine
devctx recall "which database did we pick"
devctx memory-stats
devctx mcp                         # MCP server over stdio (for AI agents/editors)
devctx mcp configure --client cursor --scope project   # register in an AI client
devctx tui                         # terminal UI: search + call-graph + memories (F1/F2/F3)
devctx web                         # web dashboard: interactive call-graph + memories
devctx serve                       # long-lived server that owns the DB (server mode)
```

**Server mode.** DuckDB allows a single read-write process. Run `devctx serve`
and it becomes the sole owner of the database; every other `devctx` command
discovers it (via `.devctx/state/serve.json`) and routes over HTTP instead of
opening the file, so concurrent CLI/editor/web use never hits a lock. When no
server is running, commands open the store directly as usual.

`devctx web` serves a self-contained dashboard (call-graph via a vendored,
offline cytoscape build + a memories browser) and opens it in your browser.
`devctx tui` is the terminal equivalent, with three views switched by F1/F2/F3.

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
| `devctx-tui` | interactive terminal UI (ratatui): live vector/keyword/hybrid search | F9 |

All rewrite phases (F0–F9) are complete.

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
