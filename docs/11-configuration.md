# Configuration

> 🇪🇸 [Leer en español](es/11-configuracion.md)

There are two config files. The project one describes a repository; the central
one describes this machine.

---

## 1. Project config — `.devctx/config.yaml`

Written by `devctx init` (or `devctx projects add --init`) at the repository
root. Found by walking up from the working directory, so any subdirectory works.

```yaml
project:
  name: myproj                 # the name agents refer to it by
  path: /home/you/code/myproj  # absolute repository root
  group: ''                    # repositories of one product that share memories

state_dir: ''                  # empty => .devctx/state/ inside the repo
language: en                   # en | es — UI and summary language

embeddings:
  provider: local              # local | openai | voyage | custom
  model: minilm-l6             # registry key; see docs/09
  model_dir: ''                # directory of a user-defined ONNX model
  offline: auto                # auto | "true" | "false"

storage:
  db_path: ''                  # empty => {state_dir}/index.duckdb
  hnsw: true                   # approximate vector index (needs the VSS extension)
  metric: cosine               # cosine | ip — ip needs unit-normalized vectors
  fts: false                   # BM25 keyword index (needs the FTS extension)

indexing:
  exclude: []                  # .gitignore-style patterns; see docs/13
  branches: []                 # tracked branches; empty => whatever is checked out

reranking:
  enabled: false               # opt-in; see docs/08 ADR-15 for the measurements
  model: bge-base              # bge-base | bge-v2-m3 | jina-turbo | custom
  model_dir: ''                # your own ONNX cross-encoder
  pool: 100                    # candidates shown to the cross-encoder

summarization:
  provider: extractive         # extractive | openai | noop
  require_local: true          # block non-local providers
  target_tokens: 200
  model: gpt-4o-mini           # for API providers
```

**Where the database ends up.** `storage.db_path` wins; then
`{state_dir}/index.duckdb`; then `.devctx/state/index.duckdb` under the project
path. `devctx init` leaves both empty, so the index lives inside the repository —
and writes `.devctx/.gitignore` with `state/` so it is not committed. The config
beside it *is* worth tracking.

**Changing the embedding model** changes the vector width, which is fixed when
the database is created. Indexing detects the mismatch and re-indexes from
scratch rather than corrupting the store.

## 2. Central config — `~/.config/devctx/config.yaml`

Machine-wide. Written with defaults the first time anything touches the central
store. Full reference in [The Central Store §6](12-central-store.md#6-configuration).

```yaml
memory:
  provider: local
  model: minilm-l6       # pins the global memory vector space — a constraint,
                         # not a default: it cannot vary per project
defaults:                # what `projects add --init` writes into a new project
  embeddings:
    provider: local
    model: minilm-l6
  reranking:
    enabled: false      # opt-in; see ADR-15 for the measurements
    model: bge-base
reindex:
  every_seconds: 0       # background sweep; 0 = off
```

**Precedence** for anything both files can express:

```
.devctx/config.yaml  ›  central defaults  ›  built-in defaults
```

The central `defaults` are a starting point, copied into a project's config when
it is created. Editing them later does not change existing projects — edit the
project's own config, then `devctx projects refresh <name>` to update the
registry's copy.

## 3. Environment variables

| Variable | Effect |
|---|---|
| `DEVCTX_HOME` | Relocates the central store *and* config under one directory. Primarily for tests and CI. |
| `DEVCTX_MODEL_CACHE` | Where downloaded models are cached. Default: `{data dir}/models`. |
| `DEVCTX_NO_AUTOSERVE` | Never auto-spawn a server; commands open the store directly. |
| `DEVCTX_API_TOKEN` | Bearer token required by `serve` / `api` on every route except `/health`. |
| `DEVCTX_MODEL_DIR` | Directory of a user-defined ONNX model. `embeddings.model_dir` wins over it. |
| `DEVCTX_EMBED_ENDPOINT` | Base URL for the `custom` embedding provider. |
| `DEVCTX_EMBED_DIMENSION` | Vector width for the `custom` provider, which has no registry entry. |
| `DEVCTX_EMBED_MAX_CHARS` | Characters per text fed to the encoder. Default `4096`; `0` disables. Lower it (e.g. `2048`) on a tight machine — it attacks batch padding, which is where the memory spike comes from. |
| `DEVCTX_EMBED_BATCH_SIZE` | Texts per encoder batch. Default `32`. |
| `DEVCTX_DB_MEMORY_LIMIT` | DuckDB per-process memory budget, any DuckDB size literal. Default `2GB`. |
| `DEVCTX_DB_THREADS` | DuckDB worker threads. Default `4`. |
| `DEVCTX_MODEL_IDLE_SECS` | How long an unused model is kept loaded. Default `300`; `0` keeps it for the life of the process. |
| `DEVCTX_MAX_OUTPUT_TOKENS` | Cap on a whole-file `read_file` with no line range. Default `8000`; `0` disables. |
| `DEVCTX_NO_UPDATE_CHECK` | Opt out of the background release check. |
| `DEVCTX_LANG` | Language of the grouped `--help` summary (`en` / `es`). |
| `OPENAI_API_KEY` / `VOYAGE_API_KEY` | Credentials for the API embedding providers. |

### Installer-only

Read by `install.sh` / `install.ps1`, not by the binary — they apply before
`devctx` exists.

| Variable | Effect |
|---|---|
| `DEVCTX_BIN_DIR` | Where to install. Default `~/.local/bin` (Linux/macOS), `%LOCALAPPDATA%\devctx\bin` (Windows). |
| `DEVCTX_VERSION` | Install a specific tag. Without it, the latest release. |
| `DEVCTX_REPO` | Repository to download from. Default `snaven10/DevCtxEngine`; for a fork. |

### Why the database limits exist

DuckDB defaults to 80% of system memory. That is correct for one process on one
box and wrong here, because every project gets its own store: three servers on a
16 GB laptop is 38 GB of intent, and the kernel's OOM killer arrives long before
DuckDB feels any pressure.

Worse, that killer does not pick the greedy process — it picks the highest
`oom_score`, which on a systemd session is usually the user's own desktop
services. The visible symptom is a dead panel and closed windows, not a slow
query. A modest per-process budget costs a spill to disk on the largest queries
and buys a machine that stays usable.

`DEVCTX_UPDATE_AVAILABLE` also exists, but is set *by* the CLI for its own
subprocesses rather than by you.

`$XDG_DATA_HOME` and `$XDG_CONFIG_HOME` are honoured when set.

## 4. Registering with an AI client

```bash
devctx mcp configure --client claude-code --scope project
devctx mcp configure --client cursor --scope global
devctx mcp configure --client claude-desktop --scope global
devctx mcp configure --client claude-code --remove
devctx mcp configure --client cursor --show      # print without writing
```

| Client | Project scope | Global scope |
|---|---|---|
| `claude-code` | `.mcp.json` | `~/.claude.json` |
| `cursor` | `.cursor/mcp.json` | `~/.cursor/mcp.json` |
| `claude-desktop` | — | `claude_desktop_config.json` |

The entry is written into `mcpServers` alongside whatever is already there.
`--env KEY=VALUE` (repeatable) adds environment entries.

Project-scoped files land in the repository — check whether you want them
committed before doing so.

## 5. Server mode

DuckDB allows one writing process per database file, so `devctx serve` becomes
the sole owner of a project's store and every other command routes to it over
HTTP. It is spawned automatically on first use and idles out after 15 minutes.

```bash
devctx serve                 # foreground, this project
devctx serve --stop
devctx serve --central       # the central store instead; see docs/12
DEVCTX_NO_AUTOSERVE=1 devctx search "…"    # open the store directly instead
```

Because the server holds the loaded code, **a rebuilt binary does not take effect
until the running server is restarted** — `devctx serve --stop` before testing a
change.

## 6. Changing the config while a server is running

**A running server holds the config it started with.** `serve` takes a
`ProjectConfig` by value at startup and keeps it for the life of the process;
nothing re-reads the file. Since `devctx index` routes to that server, editing
`.devctx/config.yaml` and re-indexing **does nothing, and says nothing** — the
run succeeds with the old settings.

Measured on a real repository: adding two generated SQL dumps to
`indexing.exclude` and re-indexing produced byte-identical counts (216 files,
3,253 chunks). After stopping the server first, the same run gave 214 files and
2,002 chunks — 1,251 chunks of noise gone, and not one symbol lost, because the
excluded files were `INSERT` statements that declared nothing.

So after editing the config:

```bash
devctx serve --stop     # checkpoints, then exits
devctx index --full     # spawns a fresh server that reads the new config
```

`devctx projects refresh <name>` updates the **registry's** copy of the config.
It does not restart a running server, so it does not help here either.

## 7. Quick recap

| Want to… | Do |
|---|---|
| Change a project's model | Edit `embeddings.model`, then `devctx index --full` |
| Keep files out of the index | `.gitignore`, or `indexing.exclude` for tracked files |
| Move the index out of the repo | Set `state_dir` (or `storage.db_path`) |
| Move models off the system disk | `DEVCTX_MODEL_CACHE` |
| See what a project is configured with | `devctx projects show <name>` |
| Share a lesson between projects | `devctx remember … --scope global` |
