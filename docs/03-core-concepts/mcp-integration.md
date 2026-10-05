# MCP Integration

> 🇪🇸 [Leer en español](../es/03-conceptos-fundamentales/integracion-mcp.md)

DevCtxEngine exposes its capabilities as tools over the
[Model Context Protocol](https://modelcontextprotocol.io/), so any MCP client —
Claude Code, Claude Desktop, Cursor — can use them without per-client work.

---

## Setup

```bash
devctx mcp configure                          # Claude Code, project scope
devctx mcp configure --client cursor
devctx mcp configure --client claude-desktop --scope global
devctx mcp configure --remove
```

| Client | Written to |
|---|---|
| `claude-code` | `.mcp.json` (project) or `~/.claude.json` (global) |
| `claude-desktop` | `claude_desktop_config.json` (global only) |
| `cursor` | `.cursor/mcp.json` |

`--name` changes the key under `mcpServers` (default `devctx`).

## Transport

**stdio.** The client spawns `devctx mcp` as a child process and speaks JSON-RPC
2.0 over stdin/stdout. No HTTP, no ports, no authentication — the trust boundary
is the process boundary.

Parsing, chunking, embedding and reranking are Rust, in the same binary — there
is no sidecar and no second runtime. But **`devctx mcp` itself never opens the
database.** It is a thin, lazy client of `devctx serve`, the one process that
owns the project's DuckDB file; see [Lifecycle](#lifecycle-and-reliability).

## Project binding

**A globally-registered MCP server starts in whatever directory the client was
launched from.** That is rarely the repository you mean, and often no repository
at all — so the server resolves one, in this order:

| | Rule | What it looks like |
|---|---|---|
| 1 | `--project <path>` | You named a root. Nothing overrides it. |
| 2 | Walk upwards | The working directory is inside a repository. |
| 3 | **Descend into the registry** | The working directory *contains* registered projects — a workspace root. |
| 4 | Unbound | Nothing matched; the error says what was found and why it was not enough. |

Rule 3 is what makes a multi-repository workspace work. Started from a directory
holding several registered projects, the server binds:

- **one project**, if only one lives under it;
- **the whole group**, if every one of them declares the same `project.group`.

```
$ cd ~/work/acme && devctx mcp
Bound to group ACME (11 projects, default acme-api) — resolved from /home/you/work/acme
```

Bound to a group, `remember` defaults to `scope: group` rather than `local`: the
session is attached to a product, and `local` would bury the memory in whichever
member the descent picked. An explicit `scope` always wins.

Projects that share a directory but not a group leave the server unbound on
purpose — binding one of them would be a guess. Give them the same
`project.group` and the descent takes over.

### Working across repositories

The server's working directory is fixed when the process starts and never
changes, so a binding resolved at startup cannot follow you as you move between
repositories. Instead, the code tools take an optional `project`: a registered
project name, or any path inside one.

```
search(query: "retry policy", project: "acme-worker")
search(query: "retry policy", project: "~/work/acme/acme-worker/src/main.rs")
read_file(path: "~/work/acme/acme-web/src/app.ts")     # resolved from the path itself
```

It resolves **that call only** and never changes the session binding. When the
answer came from an inferred project rather than one you named, the result
carries `resolved_project` so you can tell which repository replied.

`use_project` still exists, and is now what its name says: an explicit override
that moves the session, useful when a long stretch of work lives in one place.

## Lifecycle and reliability

DuckDB allows one writer per file. If every MCP session opened the database
itself, one stale session would lock out every other process on the repository
until someone killed it by hand. So the MCP does not own anything:

- **Lazy.** The MCP connects to the project's server on the first tool call that
  needs it, starting one in the background if none runs. It holds no `Store`
  between calls and never falls back to opening the file. If no server can be
  reached, the tool fails with `no devctx server for <project>: <cause>` and
  the cause is read from `serve.log`, next to the database.
- **Fail fast on a lock.** If something else holds the database, `devctx` names
  the PID, its command line and age, and the remedy, in under two seconds:
  `the index of <project> is held by PID N (…). If it is a devctx mcp from an
  old session, close that session; otherwise devctx serve --stop.`
- **Reconnects.** After `devctx serve --stop`, an idle exit or a crash, the next
  tool call starts a new server and carries on — `remember` included. A call is
  retried **only when it provably never reached the server** (connection
  refused, connect timeout) or when the server answered `503` with the header
  `X-Devctx-Exiting` (it was shutting down and processed nothing). A read
  timeout, a connection cut after sending or any other HTTP status is never retried, so a write is never done
  twice. After a failed reconnect the failure is remembered for 3 seconds.
- **Exits when its client does.** Closing stdin ends the process: within about
  six seconds even with a tool in flight (rmcp drains responses for up to 5 s,
  then the runtime gets 1 s).
- **Reports a stale binary instead of dying.** `index_status` and
  `list_projects` carry
  `"mcp": {"version": "0.9.0", "binary_replaced": false}`. After you reinstall
  `devctx` while a session is open, `binary_replaced` is `true` and the object
  adds `installed_version` and a `hint`: *restart the AI session to pick it
  up*. The old process keeps working with the code it started with — nothing is
  restarted for you, so **restart your Claude / Cursor sessions after
  upgrading.**

### The server it talks to

`devctx serve` (auto-spawned with `--idle 900`) is the owner of one project's
database. What you can rely on:

| | Behaviour |
|---|---|
| **Idle exit** | After the idle window with no request it freezes writes, checkpoints the WAL and leaves. A running index counts as activity only while it still advances a file (`DEVCTX_INDEX_STALL_SECS`, default 900); a stuck one does not keep the server alive. |
| **Graceful shutdown** | SIGTERM cancels a running index between files (nothing is half-written: each file is one transaction, the recorded commit does not advance), checkpoints and exits. `DEVCTX_SHUTDOWN_GRACE_SECS` can only shorten the 3 s grace. |
| **While exiting** | Requests get `503` + `X-Devctx-Exiting: 1` *before* anything is processed, so a client can safely repeat them against the next server. `serve.json` stays until the final checkpoint is done, so no second server starts against the lock. |
| **Vanished project** | If the project directory is deleted the server exits on its own (`DEVCTX_VANISH_POLL_MS`, default 10 s, three consecutive not-found polls). |
| **Model loads** | A model download that makes no progress for 120 s (`DEVCTX_MODEL_STALL_SECS`; `0` disables) fails with an error that says to restart the server, instead of hanging. |
| **Memory** | Embedding runs one batch at a time, 8 texts per batch (`DEVCTX_EMBED_BATCH_SIZE`); an unused model is dropped after 300 s (`DEVCTX_MODEL_IDLE_SECS`). |

`devctx serve --stop` sends SIGTERM and waits for the process to **exit**, not
just for its `cmdline` to change: it holds off SIGKILL while an index is
cancelling (up to 65 s) or the final checkpoint is running. It only signals a
process it can prove is this project's devctx server:

| Platform | How ownership is verified |
|---|---|
| Linux | `pidfd` (no PID-reuse race) plus `/proc/<pid>/{exe,cmdline,stat}`; the start time stored in `serve.json` must match |
| macOS | `ps -ww -o lstart=,command=` (UTC) and `lsof` for the working directory; waiting for exit uses `kqueue` |
| Windows | **Unsupported**: `--stop` cannot verify or signal the process and says so; it does not delete `serve.json` |

A PID that cannot be verified is never signalled and its `serve.json` is kept;
`serve --stop` then exits non-zero with the manual instructions (`kill <pid>`
and delete the file).

## The tools

24 tools, grouped by what they answer.

### Code

| Tool | Answers |
|---|---|
| `search` | *Where is the code about X?* Modes: `vector` (default), `keyword` (BM25), `hybrid` (RRF). Filters: `language`, `kind`, `include_tests`; see [Search](search.md) |
| `read_file` | The file, optionally a 1-based inclusive line range |
| `read_symbol` | A symbol's definition, code, file, line range and kind — when you know the name. A miss answers with `suggestions` and, for library calls, `external: true` |
| `get_references` | Every call site of a symbol |
| `impact_analysis` | Transitive callers (blast radius) and callees |
| `summarize` | Text down to roughly `max_tokens`, extractive by default so identifiers survive |

The distinction between `search` and `read_symbol` is worth internalising:
`read_symbol` when you know the name and want the thing itself, `search` when
you want code *about an idea*.

### Routes

| Tool | Answers |
|---|---|
| `search_routes` | HTTP routes by method and/or path substring, 20 per page |
| `routes_for_handler` | The routes served by a handler symbol |

Frameworks recognised: FastAPI, Flask, Express, NestJS, Spring, Quarkus,
Angular.

### Memory

| Tool | Answers |
|---|---|
| `remember` | Save a decision/insight/note/bug, deduplicated by topic or content |
| `recall` | Memories relevant to a query, across every tier, each tagged with where it came from |
| `memory_context` | The most recent memories, *with no query* — for recovering after a reset, when you don't yet know what to ask |
| `memories_by_symbol` | Why this symbol is the way it is — what the call graph cannot answer. 5 per page, `content` cut to 600 characters (`full: true` lifts it) |
| `memories_by_file` | The same, for a file — plus `plan_tasks`, the plan tasks (from `plans/`) that mention it |
| `memory_refs` | The inverse: given a memory id, the symbols and files it concerns |
| `memory_stats` | Counts, total and per type |
| `memory_forget` | Permanently delete one. Not reversible. |
| `memory_move` | Move between tiers, or to another project. The id changes. |
| `build_context` | One budgeted brief: known + code + recorded-against-that-code. Takes `project`; in a group it picks the best-matching member (see [Context Builder](context-builder.md)) |

### Plans

| Tool | Answers |
|---|---|
| `plan_status` | Progress on the plans under `plans/` (markdown, source of truth): no argument lists plans — 25 per page, newest first, `active_only: true` keeps only those with unresolved tasks — and names the active one; with `plan`, that plan's ready/in-progress/blocked tasks |

Plans live as hand-written markdown in git, never copied into the store:
`plan_status` reads `plans/PLAN-*/` on every call — from the project, or from
the workspace root when the repositories of a product live under a directory
that holds the shared `plans/` (the result's `plans_root` says which, and why;
the order is in [the agent workflow](../04-agent-workflow.md#where-plans-is-read-from)).
The task format is tolerant — `Status:`/`Estado:`, synonyms, a `skipped`
state — and is documented there too. A plan's own progress table
and its task files can disagree (both are written by hand) — when they do, the
task file wins and the disagreement is reported as a warning, never silently
picked one way. Call it right after a compaction to recover which task is
ready to start.

### Projects and indexing

| Tool | Answers |
|---|---|
| `list_projects` | Every repository tracked: name, path, model, index freshness |
| `use_project` | Bind this session to a project |
| `search_project` | Search a *different* registered project by name |
| `index_repo` | Index: git diff → parse → chunk → embed → store |
| `index_status` | Last-indexed commit and counts for this repo and branch, whether it is current (`up_to_date`), whether the extractor is older than this binary (`extractor_stale`) and the `mcp` version block |

`search_project` is for when the answer lives in a repository other than the one
you are working in — the backend question you hit while editing the frontend.

## Return shapes

Most tools return **JSON**. Two do not:

- `build_context` returns **prose**, because the result is meant to be read
  straight into a model's context and a JSON envelope would spend budget on
  punctuation.
- `summarize` returns text.

### The list tools always answer with an object

`search`, `search_project` and the group-wide `search` answer
`{"results": [...]}`; `search_routes` and `routes_for_handler` answer
`{"routes": [...]}`. **Never a bare array** — earlier versions answered an array
when there was nothing to say and an object otherwise, which made every client
guess. Next to the rows, any of these may appear:

| Field | Meaning |
|---|---|
| `omitted` | `{count, reason, next_offset?}` — what was left out. `reason` is `limit` (the page size) or `budget` (the output budget; wins when both cut). Absent means nothing was left out. (`omitted_for_budget`, which also names the dropped items, is kept one more release.) |
| `total`, `next_offset` | `search_routes` and `plan_status`: rows matching before paging, and the `offset` of the next page (absent on the last) |
| `branch_fallback` | `{current, used, why}` — the answer came from another branch; see [Search](search.md#branch-awareness) |
| `warning` | The index was produced by an older extractor; reindex with `devctx index --full` |

Search hits carry `score`, `file`, `start_line`, `end_line`, `symbol`,
`symbol_type`, `level`, `language`, `kind` (`code` / `test` / `doc` / `config`),
`text`, plus `raw_score` (only when a later stage rewrote `score`: the retriever's
own score) and `anchored: true` (the definition of an identifier named in the
query). `devctx search --format json` prints the same object.

### Pagination

| Tool | Default page | Parameters |
|---|---|---|
| `search_routes` | 20 routes | `limit`, `offset` |
| `plan_status` (list) | 25 plans, newest first | `limit`, `offset`, `active_only` |
| `memories_by_symbol`, `memories_by_file` | 5 memories | `limit`, `offset`, `full` |
| `search` | top-k by `limit` (10); does not page | `limit` |

Pass the previous answer's `next_offset` as `offset` to continue. A memory whose
`content` was cut carries `content_truncated: true` and `content_chars`; its `id`
fetches the rest (`full: true`).

### `project`

The tools that act on code take an optional `project` (registered name, or any
path inside one) that resolves that one call and never changes the binding — now
including `build_context`, `memories_by_symbol` and `memories_by_file`. In a
group session `build_context` also requires it to be a member of the group.

Memory-by-code results (`memories_by_symbol`, `memories_by_file`, `memory_refs`)
always carry `link_sources`, and it is there on purpose: `files-field` and
`content-mention` mean something connected that memory to that code at write
time, while `inference` means only that the words match. A caller weighing how
much to trust a link needs that distinction. `files-field` is also reported when
the junction row is missing but the memory's `files` names the file.

## Discovery

On `tools/list` the server returns all 24 definitions with JSON Schema
parameters, declared upfront. The client validates arguments before each call.
There is no runtime discovery step.

## Other interfaces

The same engine is reachable four other ways, all reading the same store:

```bash
devctx tui        # interactive terminal UI: search, graph, memories
devctx web        # browser dashboard: call graph + memories
devctx api        # HTTP REST API
devctx serve      # long-lived server that owns the DB; other commands route to it
```

`serve` matters for concurrency: DuckDB allows one writer per file, so a
long-lived server owns the database and the CLI — and the MCP — route through it
rather than contending for the lock.

## Upgrading to 0.9

- **Restart your AI sessions.** A running `devctx mcp` keeps the code it started
  with; it will tell you so in `mcp.binary_replaced`.
- **Mixed versions are a transition state, not a configuration.** A 0.8.4 MCP
  against a 0.9 server cannot send the new parameters (`kind`, `include_tests`,
  `offset`, `full`, `project` on `build_context`) and relays the new object
  shapes to a client that may still expect arrays. In the other direction a 0.9
  MCP wraps the bare array an older server answers into the new object, but
  that server ignores `kind` and `include_tests`. Stop old servers with
  `devctx serve --stop` after upgrading.
- **The JSON shapes changed** (objects, not arrays; `results`, not `hits`). See
  the [changelog](../../CHANGELOG.md).
- **The first `devctx index` after upgrading** reconciles the new default
  excludes — see [Keeping the index fresh](../13-keeping-the-index-fresh.md).
