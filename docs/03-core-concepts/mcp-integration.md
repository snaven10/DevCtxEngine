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

The server runs entirely in-process: parsing, chunking, embedding and reranking
are Rust, in the same binary. There is no sidecar and no second runtime.

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

## The tools

23 tools, grouped by what they answer.

### Code

| Tool | Answers |
|---|---|
| `search` | *Where is the code about X?* Modes: `vector` (default), `keyword` (BM25), `hybrid` (RRF) |
| `read_file` | The file, optionally a 1-based inclusive line range |
| `read_symbol` | A symbol's definition, code, file, line range and kind — when you know the name |
| `get_references` | Every call site of a symbol |
| `impact_analysis` | Transitive callers (blast radius) and callees |
| `summarize` | Text down to roughly `max_tokens`, extractive by default so identifiers survive |

The distinction between `search` and `read_symbol` is worth internalising:
`read_symbol` when you know the name and want the thing itself, `search` when
you want code *about an idea*.

### Routes

| Tool | Answers |
|---|---|
| `search_routes` | HTTP routes by method and/or path substring |
| `routes_for_handler` | The routes served by a handler symbol |

Frameworks recognised: FastAPI, Flask, Express, NestJS, Spring, Quarkus,
Angular.

### Memory

| Tool | Answers |
|---|---|
| `remember` | Save a decision/insight/note/bug, deduplicated by topic or content |
| `recall` | Memories relevant to a query, across every tier, each tagged with where it came from |
| `memory_context` | The most recent memories, *with no query* — for recovering after a reset, when you don't yet know what to ask |
| `memories_by_symbol` | Why this symbol is the way it is — what the call graph cannot answer |
| `memories_by_file` | The same, for a file |
| `memory_refs` | The inverse: given a memory id, the symbols and files it concerns |
| `memory_stats` | Counts, total and per type |
| `memory_forget` | Permanently delete one. Not reversible. |
| `memory_move` | Move between tiers, or to another project. The id changes. |
| `build_context` | One budgeted brief: known + code + recorded-against-that-code |

### Projects and indexing

| Tool | Answers |
|---|---|
| `list_projects` | Every repository tracked: name, path, model, index freshness |
| `use_project` | Bind this session to a project |
| `search_project` | Search a *different* registered project by name |
| `index_repo` | Index: git diff → parse → chunk → embed → store |
| `index_status` | Last-indexed commit and counts for this repo and branch, and whether it is current |

`search_project` is for when the answer lives in a repository other than the one
you are working in — the backend question you hit while editing the frontend.

## Return shapes

Most tools return **JSON**. Two do not:

- `build_context` returns **prose**, because the result is meant to be read
  straight into a model's context and a JSON envelope would spend budget on
  punctuation.
- `summarize` returns text.

Memory-by-code results (`memories_by_symbol`, `memories_by_file`, `memory_refs`)
always carry `link_sources`, and it is there on purpose: `files-field` and
`content-mention` mean something connected that memory to that code at write
time, while `inference` means only that the words match. A caller weighing how
much to trust a link needs that distinction.

## Discovery

On `tools/list` the server returns all 23 definitions with JSON Schema
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
long-lived server owns the database and the CLI routes through it rather than
contending for the lock.
