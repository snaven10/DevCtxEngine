# Setting up DevCtxEngine — instructions for an agent

You are setting DevCtxEngine up on a machine, for a human who wants semantic
code search and a memory that survives between sessions. Follow this in order.
Every step says how to verify it, because several failures here are **silent** —
they produce a working-looking system that returns nonsense.

Read the whole file before starting. If a step's verification fails, stop and
report it rather than continuing; each step depends on the one before.

---

## 0. What you are building

```
<repo>/.devctx/state/index.duckdb     code vectors + graph + routes + local memories
~/.local/share/devctx/central.duckdb  project registry + group and global memories
~/.config/devctx/config.yaml          the machine's defaults
```

One database per repository, one shared. Memories live in three tiers: `local`
(this repo), `group` (the repositories of one product), `global` (everything).

## 1. Build and install

```bash
cargo build --release
install -m755 target/release/devctx ~/.local/bin/devctx
devctx --help
```

The build compiles DuckDB from source: **20–25 minutes** on 8 cores, and it is
recompiled per profile, so `cargo test` pays it again. It is not hung.

If `cargo build` prints nothing and burns no CPU, it is waiting on the network
for the crates registry. Add `--offline` when the dependencies are already
vendored.

## 2. Choose the embedding model — decide this before indexing

Changing it later means re-indexing everything from scratch, so decide now.

| model | languages | needs files on disk? |
|---|---|---|
| `minilm-l6` (built-in default) | English | no — downloaded on first use |
| `bge-small`, `bge-base` | English | no |
| `ml-minilm` | multilingual | no |
| `ml-granite` | multilingual, best on CPU | **yes** |

**If the code, comments or memories are not in English, `minilm-l6` is the wrong
choice** and nothing will tell you: it produces valid embeddings of Spanish text,
just poor ones.

`ml-granite` and `ml-granite-lg` are *user-defined ONNX* models: nothing
downloads them for you. The directory must contain the ONNX file plus
`tokenizer.json` and `config.json`:

```
<model_dir>/
  onnx/model_quint8_avx2.onnx     (or model.onnx)
  tokenizer.json
  config.json
  special_tokens_map.json         (optional)
  tokenizer_config.json           (optional)
```

Fetch them from the HuggingFace repo named in `docs/09-models-and-tuning.md`
and put them wherever you like — `~/.local/share/devctx/models/ml-granite` is
the convention. **Without `model_dir` set, loading fails with a clear error**;
this one is not silent.

## 3. Set the machine's defaults

Write `~/.config/devctx/config.yaml` **before** creating any project — `init`
copies these into every new one:

```yaml
memory:                                  # pins the vector space of shared memories
  provider: local
  model: ml-granite
  model_dir: /home/you/.local/share/devctx/models/ml-granite
defaults:                                # what a new project inherits
  embeddings:
    provider: local
    model: ml-granite
    model_dir: /home/you/.local/share/devctx/models/ml-granite
    offline: auto
  reranking:
    enabled: false                       # see §7
reindex:
  every_seconds: 0
```

`memory.model` **cannot be changed** once the central store holds memories: it
is the vector space they live in. Changing it is a re-migration, not an edit.

## 4. Register the repositories

```bash
cd ~/code/api && devctx init --group myproduct
cd ~/code/web && devctx init --group myproduct
devctx projects list
```

`--group` is what puts several repositories into one memory tier. Omit it for a
repository that stands alone. **Verify** the config it wrote inherited your
model:

```bash
grep -A3 'embeddings:' .devctx/config.yaml    # must show your model, not minilm-l6
```

## 5. Migrate memories from an old DevAI install

Only if there is a `devai` install to migrate. Old memories carry two scopes;
where they land depends on the current project's config:

| old scope | lands in |
|---|---|
| `shared` / `global` | the **group** tier if the repo declares `group:`, else **global** |
| `local` / `personal` | the store of the repo you run this from |

So **declare the group first** (§4) or everything shared becomes global.

```bash
cd ~/code/api
devctx serve --stop && devctx serve --central --stop   # migrate opens the stores directly
devctx migrate --from ~/.local/share/devai/state/index.db --keep-project --dry-run
devctx migrate --from ~/.local/share/devai/state/index.db --keep-project
```

Every memory is **re-embedded**, so this takes roughly a minute per 45 memories
and slows as it goes (each new memory is deduplicated against all the previous
ones). Two thousand memories is about 45 minutes. Do not interrupt it.

The old vectors cannot be reused even when the model has the same name: the
older implementation produces different vectors for identical text (measured:
cosine 0.76–0.87 against the new ones, where identical would be 1.00). Mixing
them ranks everything wrongly.

**Verify**, from a *different* repository of the group:

```bash
devctx recall "something you know is in there"
```

Results should be tagged `[group]` and be about what you asked. If they are
unrelated, the query and the stored vectors are in different spaces — check
that `memory.model` matches what the migration used.

## 6. Index

```bash
cd ~/code/api
devctx serve --stop
DEVCTX_NO_AUTOSERVE=1 devctx index          # see below for why
devctx status                               # "indexed": true, "up_to_date": true
```

Reckon **an hour per ~1400 files** on 8 cores. Two things to know:

- The work happens inside the server, and the client's HTTP read times out at
  about an hour. **The error is the client giving up, not the index failing** —
  poll `devctx status` instead of believing it.
- Building the HNSW index only happens on the direct path, hence
  `DEVCTX_NO_AUTOSERVE=1`. Without it the index is never created and searches
  fall back to a full scan (~5× slower).

Enable HNSW first, in `.devctx/config.yaml`:

```yaml
storage:
  hnsw: true
  metric: cosine     # `ip` is cheaper but only valid for normalized embeddings
```

## 7. Reranking: leave it off unless you measure otherwise

`reranking.enabled: true` costs **~180 seconds per search** with `bge-v2-m3`
(a 2.2 GB cross-encoder over a pool of 100 candidates), against **1–2 seconds**
with it off. It does improve ordering. It is not worth three minutes.

If you want to try it, lower `reranking.pool` first.

## 8. Wire it to the AI client

```bash
devctx mcp configure --client claude --scope project
```

Or by hand, in `.mcp.json`:

```json
{ "mcpServers": { "devctx": { "command": "/home/you/.local/bin/devctx", "args": ["mcp"] } } }
```

No environment variables are needed — the config carries the model path. The
server starts unbound when launched outside a repository; the agent calls
`list_projects` and `use_project` to bind one.

## 9. Verify the whole thing

```bash
devctx projects list                  # real file counts, not "never indexed"
devctx search "some behaviour you know exists"
devctx recall "something you wrote down"
```

---

## Things that will bite you

**A rebuilt binary does not take effect until the server restarts.** The server
holds the loaded code; `devctx serve --stop` after every reinstall, or you will
spend an hour measuring the old build.

**`DEVCTX_MAX_OUTPUT_TOKENS` must be set on the process that runs the server**,
not on the client. The MCP inherits its environment to the server it spawns, so
putting it in the MCP entry works — but not if a server is already up with a
different value.

**Two `model_dir` keys exist**, under `embeddings:` and under `reranking:`, for
models of completely different kinds. Setting the reranking one to an embedding
model makes every search fail with `no output named 'logits'`.

**Killing processes by matching their command line kills the shell doing the
matching**, because its own command line contains the pattern. Use
`devctx serve --stop`.
