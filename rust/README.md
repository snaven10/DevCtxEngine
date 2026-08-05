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
devai mcp                         # MCP server over stdio (for AI agents/editors)
```

## Crates

| crate | responsibility | phase |
|-------|----------------|-------|
| `devai-core`  | shared types, `.devai/config.yaml` schema, errors | F0 |
| `devai-cli`   | the `devai` binary (clap) | F0 |
| `devai-store` | DuckDB store: vectors (brute-force cosine) + relational schema | F1 |
| `devai-embed` | embeddings: local (fastembed/ort) + OpenAI/Voyage/custom | F2 |
| `devai-parse` | tree-sitter symbols/imports (py/js/ts/go/java/rust) + lang registry | F3 |
| `devai-chunk` | semantic multi-level chunker (file/class/function/block) | F3 |
| `devai-index` | pipeline: git diff → parse → chunk → embed → store (incremental) | F4 |
| `devai-rerank` | cross-encoder reranking (fastembed BGE) + no-op fallback | F5 |
| `devai-mcp` | MCP server (rmcp, stdio): search / read_file / index_repo / index_status | F6 |

Planned (later phases): `devai-summarize`, `devai-api`, `devai-tui`.

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
