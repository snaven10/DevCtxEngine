# DevAI — Rust workspace

Rust + DuckDB rewrite of DevAI. See [`../docs/rust-rewrite-plan.md`](../docs/rust-rewrite-plan.md)
for the architecture and phased plan.

> **Status: F5 done.** `devai init`, `index` and `search` work end-to-end: the
> incremental pipeline indexes a repo and semantic search returns ranked results
> using a real local model. The legacy Go + Python tree in the repo root remains
> the reference implementation until parity is reached.

```bash
devai init --name myproj
devai index                       # git diff → parse → chunk → embed → store
devai search "connect to a database" --limit 5
devai search "greet a user" --format json
devai remember "We chose Postgres for JSONB" --type decision --topic db-engine
devai recall "which database did we pick"
devai memory-stats
devai mcp                         # MCP server over stdio (for AI agents/editors)
devai mcp configure --client cursor --scope project   # register in an AI client
```

## Crates

| crate | responsibility | phase |
|-------|----------------|-------|
| `devai-core`  | shared types, `.devai/config.yaml` schema, errors | F0 |
| `devai-cli`   | the `devai` binary (clap) | F0 |
| `devai-store` | DuckDB store: vectors (brute-force cosine) + relational schema | F1 |
| `devai-embed` | embeddings: local (fastembed/ort) + OpenAI/Voyage/custom | F2 |
| `devai-parse` | tree-sitter symbols/imports/call-edges + framework route extractors | F3 |
| `devai-chunk` | semantic multi-level chunker (file/class/function/block) | F3 |
| `devai-index` | pipeline: git diff → parse → chunk → embed → store (incremental) | F4 |
| `devai-rerank` | cross-encoder reranking (fastembed BGE) + no-op fallback | F5 |
| `devai-search` | search orchestration: vector / keyword / hybrid (RRF) + rerank | F8 |
| `devai-mcp` | MCP server (rmcp, stdio): search / read_file / index_repo / index_status | F6 |
| `devai-memory` | memory engine: remember (dedup) + recall (intro/chunk blend) | F7 |
| `devai-summarize` | summarization: extractive (default) + OpenAI + local flan-t5 | F9 |
| `devai-api` | HTTP REST API (axum) reusing the MCP engine, Bearer-token auth | F9 |
| `devai-tui` | interactive terminal UI (ratatui): live vector/keyword/hybrid search | F9 |

All rewrite phases (F0–F9) are complete.

The `flan-t5` feature (off by default) adds a local abstractive summarizer via
candle — build with `--features flan-t5` (heavy; downloads the model on first use).

The `devai-embed` `local` feature (default) pulls in `fastembed`/`ort`; build
with `--no-default-features` for an API-only build where the ONNX Runtime binary
can't be fetched.

## Build

```bash
cd rust
cargo build
cargo test
cargo run -p devai-cli -- status
```
