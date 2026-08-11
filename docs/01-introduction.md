# Introduction

> Back to [README](../README.md)
> 🇪🇸 [Leer en español](es/01-introduccion.md)

---

## What is DevCtxEngine?

DevCtxEngine is a **context engine for AI agents**. It gives coding assistants —
Claude Code, Cursor, custom agents — structured, semantic understanding of your
codebase instead of forcing them to work through a keyhole of individual file
reads.

It is not a search tool, a linter, or another indexer you have to babysit. It is
the layer between your code and your agent that turns raw source files into
navigable, queryable, persistent knowledge — and keeps that knowledge across
sessions and across projects.

**It is to AI agents what an IDE is to humans.** An IDE gives you project-wide
search, go-to-definition, find-all-references and persistent workspace state.
Without it you are `cat`-ing files in a terminal, which is exactly what agents do
by default.

---

## The problem

- **Keyhole vision.** Agents see one file at a time. They cannot hold a module in
  working memory, let alone trace a call chain across packages.
- **No structural awareness.** `grep` finds text. It does not know that
  `handleAuth` is a method on `AuthMiddleware` called from three route files.
- **Amnesia.** Every session starts from zero. The agent that spent twenty
  minutes understanding your auth flow yesterday remembers none of it today —
  and an agent working in a second repository never learns what the first one
  taught it.
- **Context waste.** Without targeted retrieval, agents dump whole files into the
  context window. Half the tokens go to irrelevant code; the important parts get
  truncated.

---

## Core capabilities

**Semantic search.** Ask in natural language and get ranked code, chunked along
symbol boundaries rather than arbitrary line windows. Vector, keyword (BM25) or
hybrid.

**Symbol graph.** Call and import edges extracted from the AST, so "what breaks
if I change this" is a query rather than a guess.

**Persistent memory.** Decisions, insights and gotchas survive sessions,
deduplicated so saving the same thing twice does not accumulate noise. A memory
is either private to its project or **global** — shared with every project on the
machine, so a lesson learned once is available everywhere.

**Cross-project awareness.** A registry of every repository you have set up, so an
agent working in one knows the others exist, can search their code, and can recall
what was learned there.

**Framework routes.** HTTP routes and their handlers, extracted for Spring,
Quarkus, Nest, Express and others.

**MCP integration.** All of it exposed as tools an agent can call, plus an HTTP
API, a terminal UI and a web dashboard over the same engine.

---

## Quick start

### Install

```bash
cargo build --release
cp target/release/devctx ~/.local/bin/
```

### Set up a repository

```bash
cd ~/code/myproject
devctx init                        # writes .devctx/config.yaml, registers the project
devctx index                       # first run downloads the embedding model
```

### Search

```bash
devctx search "where do we validate the auth token"
devctx search "retry logic" --hybrid --limit 5
devctx impact handleAuth           # transitive callers and callees
```

### Remember

```bash
devctx remember "sessions expire after 24h, see auth/session.rs" --type decision
devctx remember "always verify webhook signatures" --scope global
devctx recall "how long do sessions last"
```

### Keep it fresh

```bash
devctx hooks install               # re-index after each commit
devctx watch                       # or, re-index files as you save them
```

### Connect an agent

```bash
devctx mcp configure --client claude-code --scope project
```

---

## How it works, in thirty seconds

`devctx index` asks git what changed, parses those files with tree-sitter into
symbols and call edges, chunks them along symbol boundaries, embeds each chunk
with a local ONNX model, and stores the vectors, graph and per-file hashes in one
DuckDB file inside the repository. Unchanged files are skipped by hash, so
re-indexing is cheap enough to run after every commit.

A query embeds your question, finds the nearest chunks, optionally reranks them
with a cross-encoder, and returns ranked results with file, line range and symbol.

Because DuckDB allows a single writing process, the first command that needs the
database starts a small server that owns it; everything else — other CLI
invocations, agent sessions, the TUI, the dashboard — routes to that server
instead of fighting for the lock, and the model stays loaded between calls.

What is worth sharing between projects — the registry and global memories — lives
in one central store outside any repository. Everything else stays with its
project.

---

## Documentation map

| Read this | For |
|---|---|
| [Architecture](02-architecture.md) | How the pieces fit and why |
| [Configuration](11-configuration.md) | Both config files, environment variables, MCP clients |
| [The Central Store](12-central-store.md) | Registry, global memories, the daemon |
| [Keeping the index fresh](13-keeping-the-index-fresh.md) | Hooks, watch, reindex, exclusions |
| [Agent workflow](04-agent-workflow.md) | How an agent should use the tools |
| [Models & tuning](09-models-and-tuning.md) | Choosing an embedding model |
| [Design decisions](08-design-decisions.md) | Trade-offs, with the reasoning |
