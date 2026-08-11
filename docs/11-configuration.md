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

state_dir: ''                  # empty => .devctx/state/ inside the repo
language: en                   # en | es — UI and summary language

embeddings:
  provider: local              # local | openai | voyage | custom
  model: minilm-l6             # registry key; see docs/09
  model_dir: ''                # directory of a user-defined ONNX model
  offline: auto                # auto | "true" | "false"

storage:
  db_path: ''                  # empty => {state_dir}/index.duckdb
  hnsw: false                  # approximate vector index (needs the VSS extension)
  fts: false                   # BM25 keyword index (needs the FTS extension)

indexing:
  exclude: []                  # .gitignore-style patterns; see docs/13

reranking:
  enabled: true
  model: bge-base              # bge-base | bge-v2-m3 (multilingual)

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
    enabled: true
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
| `OPENAI_API_KEY` / `VOYAGE_API_KEY` | Credentials for the API embedding providers. |

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

## 6. Quick recap

| Want to… | Do |
|---|---|
| Change a project's model | Edit `embeddings.model`, then `devctx index --full` |
| Keep files out of the index | `.gitignore`, or `indexing.exclude` for tracked files |
| Move the index out of the repo | Set `state_dir` (or `storage.db_path`) |
| Move models off the system disk | `DEVCTX_MODEL_CACHE` |
| See what a project is configured with | `devctx projects show <name>` |
| Share a lesson between projects | `devctx remember … --scope global` |
