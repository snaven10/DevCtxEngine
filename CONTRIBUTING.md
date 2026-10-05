# Contributing

## Running the tests

```bash
TMPDIR=/var/tmp cargo test --workspace
```

The build compiles DuckDB from source (20-25 minutes on 8 cores, once per
profile); see `AGENTS.md`.

### The test model cache

Tests that embed text need an embedding model (~90 MB). They share one cache
instead of downloading it per test, and it is never the real
`~/.local/share/devctx/models`, which a running `devctx serve` uses.

`.cargo/config.toml` sets `DEVCTX_MODEL_CACHE` to `<workspace>/.devctx-test-models`
(git-ignored) for every process cargo starts. It lives outside `target/` so that
`cargo clean` does not delete the models and so that it does not depend on
`CARGO_TARGET_DIR`, which `[env]` cannot follow. The first run that needs a model
downloads it there.

* An explicit `DEVCTX_MODEL_CACHE` in your environment wins over the config.
* **`cargo run` gets the variable too**: a development binary started through
  cargo uses `.devctx-test-models`, not your real models. Set
  `DEVCTX_MODEL_CACHE=~/.local/share/devctx/models` (or run the built binary
  directly) when you want the real ones.
