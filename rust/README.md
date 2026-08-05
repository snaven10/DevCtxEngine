# DevAI — Rust workspace

Rust + DuckDB rewrite of DevAI. See [`../docs/rust-rewrite-plan.md`](../docs/rust-rewrite-plan.md)
for the architecture and phased plan.

> **Status: F4 done.** Config, DuckDB store, embeddings, tree-sitter parsers,
> the semantic chunker and the incremental indexing pipeline exist and are
> tested end-to-end. The legacy Go + Python tree in the repo root remains the
> reference implementation until parity is reached.

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

Planned (later phases): `devai-rerank`, `devai-summarize`, `devai-mcp`,
`devai-api`, `devai-tui`.

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
