# DevAI — Rust workspace

Rust + DuckDB rewrite of DevAI. See [`../docs/rust-rewrite-plan.md`](../docs/rust-rewrite-plan.md)
for the architecture and phased plan.

> **Status: F2 in progress.** Config, DuckDB store and embeddings exist. The
> legacy Go + Python tree in the repo root remains the reference implementation
> until parity is reached.

## Crates

| crate | responsibility | phase |
|-------|----------------|-------|
| `devai-core`  | shared types, `.devai/config.yaml` schema, errors | F0 |
| `devai-cli`   | the `devai` binary (clap) | F0 |
| `devai-store` | DuckDB store: vectors (brute-force cosine) + relational schema | F1 |
| `devai-embed` | embeddings: local (fastembed/ort) + OpenAI/Voyage/custom | F2 |

Planned (later phases): `devai-parse`, `devai-chunk`, `devai-rerank`,
`devai-summarize`, `devai-index`, `devai-mcp`, `devai-api`, `devai-tui`.

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
