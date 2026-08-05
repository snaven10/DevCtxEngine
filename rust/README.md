# DevAI — Rust workspace

Rust + DuckDB rewrite of DevAI. See [`../docs/rust-rewrite-plan.md`](../docs/rust-rewrite-plan.md)
for the architecture and phased plan.

> **Status: F0 (scaffolding).** Only `devai-core` (config) and a skeleton CLI
> (`init`, `status`) exist so far. The legacy Go + Python tree in the repo root
> remains the reference implementation until parity is reached.

## Crates

| crate | responsibility |
|-------|----------------|
| `devai-core` | shared types, `.devai/config.yaml` schema, errors |
| `devai-cli`  | the `devai` binary (clap) |

Planned (later phases): `devai-store` (DuckDB+VSS), `devai-embed`
(fastembed/ort), `devai-parse`, `devai-chunk`, `devai-rerank`,
`devai-summarize`, `devai-index`, `devai-mcp`, `devai-api`, `devai-tui`.

## Build

```bash
cd rust
cargo build
cargo test
cargo run -p devai-cli -- status
```
