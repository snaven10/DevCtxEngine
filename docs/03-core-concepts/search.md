# Semantic Code Search

> 🇪🇸 [Leer en español](../es/03-conceptos-fundamentales/busqueda.md)

Finding code by describing what it does, not by guessing what it is called.

---

## What it is

`devctx search` answers a question in prose — *"where do we decide a token is
expired"* — with the chunks of code most likely to contain the answer. Three
retrieval strategies are available, and one of them is the default:

```bash
devctx search "expired token handling"          # vector (default)
devctx search "expired token" --keyword         # BM25
devctx search "expired token" --hybrid          # both, fused
```

Agents reach the same thing through the `search` MCP tool.

## Why it exists

Grep matches the string you typed. That works when you already know the
vocabulary of the codebase and fails exactly when you don't — a new repository, a
subsystem you've never opened, a concept that three teams spelled three
different ways (`isExpired`, `checkTTL`, `validateWindow`).

Vector search matches *meaning*, so it finds the third spelling from the first
one. Keyword search matches the literal token, which is what you want for an
error string or an identifier you copied out of a stack trace. Neither is
strictly better, which is why both are here and why hybrid exists.

## How it works

### The pipeline

Indexing turns files into embeddable chunks:

```
git diff → parse (tree-sitter) → chunk → embed → store (DuckDB)
```

Each stage lives in its own crate: `devctx-parse`, `devctx-chunk`,
`devctx-embed`, `devctx-store`. It all runs in-process — there is no sidecar and
no network hop unless you configure an API embedding provider.

### Chunk levels

The chunker never splits a symbol in half. It emits chunks at five levels, and
which ones a file produces depends on what is in it:

| Level | What it holds |
|---|---|
| `file` | A summary chunk: the path plus the symbols declared in it |
| `class` | A container — `class`, `struct`, `enum`, `trait`, `interface` — with its signature and members |
| `doc` | A documented symbol's prose, when the doc comment says something the name doesn't |
| `function` | One callable, whole |
| `block` | A slice of a function too large to embed as one chunk |

Two behaviours are worth knowing because they change what you get back:

- **Small symbols are grouped.** Anything under `min_chunk_tokens` (64) is
  merged with its neighbours into one chunk rather than embedded alone — a file
  of one-line getters produces a handful of chunks, not two hundred.
- **A doc comment that only restates the name gets no chunk.** `/// The name.`
  above `fn name()` carries no information the function chunk lacks.

Defaults, from `ChunkConfig`:

| Setting | Default | Meaning |
|---|---|---|
| `max_chunk_tokens` | 512 | Upper bound before a function is split into blocks |
| `min_chunk_tokens` | 64 | Below this, symbols are grouped |
| `large_function_threshold` | 1024 | Above this, a function is split into block chunks |

Token counts are estimated at ~4 characters per token. It is a heuristic, not a
tokenizer.

### Context headers

A `function` or `block` chunk is embedded with a breadcrumb line prepended:

```
# auth/middleware.rs > AuthMiddleware > authenticate
```

Without it, a method body reads as anonymous code. With it, the embedding
carries where the code lives, so a query naming the module or the type can find
a body that never mentions either.

### Content hashing

Every chunk carries `content_hash` — sha256 of its text, truncated to 16 hex
characters. This is what makes re-indexing cheap: a chunk whose hash is unchanged
is not re-embedded. It is also what makes multi-branch indexing cheap, since
branches share the overwhelming majority of their file contents.

## The three modes

### Vector — the default

The query is embedded with the same model that embedded the code, and the store
returns nearest neighbours by cosine distance (HNSW when the index is built,
otherwise a scan).

### Keyword — BM25

Full-text search over chunk text, served by DuckDB's FTS extension. Exact,
fast, and the right choice for error strings and identifiers.

### Hybrid — reciprocal rank fusion

Both retrievers run, and their ranked lists are fused:

```
score(item) = Σ  1 / (k + rank)     k = 60, rank is 1-based
```

An item ranked well by either retriever surfaces; an item ranked well by both
surfaces higher. RRF fuses *ranks*, not scores, so it needs no calibration
between two systems whose numbers mean different things.

If the FTS index has not been built, hybrid degrades silently to vector-only
rather than failing.

## Reranking

A cross-encoder can reorder the candidate pool before it is truncated to
`--limit`. **It is off by default, and that default was set by measurement, not
taste.** On this repository:

| Configuration | Latency | Resident memory |
|---|---|---|
| No reranking | 30 ms | 406 MB |
| Cheapest cross-encoder | 8.6 s | 2.4 GB |
| `bge-reranker-base` | 30 s | 3.4 GB |

And the one model measured across the whole bench made results *worse* — it
demoted a correct answer from first place to twenty-first.

Two things to understand before turning it on:

- **The pool is the ceiling.** A reranker reorders what it is handed and nothing
  else. An answer ranked below `reranking.pool` is invisible to it, however good
  the model is.
- **The pool is also the whole cost.** The cross-encoder is the slowest stage by
  two orders of magnitude, and pool size multiplies it. Deep pool with a small
  fast model, or shallow pool with a large one. Deep *and* large is unusable.

The default `reranking.pool` is 100. When no reranker runs, the retriever
fetches a shallow pool of 20 instead — a deeper fetch would be thrown away
without reordering. `--no-rerank` disables reranking for one search regardless
of config.

Built-in rerankers: `bge-base` (default), `bge-v2-m3` (multilingual),
`jina-turbo` (fastest). Set `reranking.model_dir` to load your own ONNX
cross-encoder — worth doing, since fastembed ships no lightweight one and the
built-ins are all over a gigabyte.

## Embedding models

`devctx models` lists what is available. The shipped default is `minilm-l6`
(384 dimensions, English). For non-English code, `ml-granite` (384,
multilingual) measured best on CPU.

The asterisk in `devctx models` marks what *this machine* is configured to give
new projects, which you can change in the central config — not what ships.

**Choose before your first index.** Changing the model afterwards means
re-indexing every file and re-embedding every memory, because vectors from two
models do not live in the same space.

If your code or comments are not in English, pick a multilingual model. The
English models will embed Spanish perfectly happily — just badly.

## Branch awareness

Chunks are stored per `(repo, branch)`. A search returns results for the branch
you are on, so a symbol deleted on your branch does not surface from `main`.

Branches you want indexed are declared in config under `indexing.branches`, and
`devctx index --branch <name>` indexes a named one. Because the copy is driven by
`content_hash`, indexing a second branch copies rather than re-embeds anything
the two branches share — measured at 95–96% of files on three real
repositories.

Indexing is worktree-independent: run it from any worktree and it updates the
one index.

## Filters

`--language <lang>` restricts to one language. `--limit` caps results (default
10). `--format json` emits a JSON array instead of the table.

## Worked example

```bash
$ devctx search "how do we decide a token is expired" --limit 3
```

1. The query is embedded (384-dim vector, `ml-granite`).
2. The store returns the 20 nearest chunks for this repo and branch.
3. With reranking off, the top 3 are returned in retriever order.

The top hit is typically a `function` chunk whose context header names the type
and module — which is how a query that says "token" finds a method called
`still_valid`.

## Mental model

Grep is an index of **strings**. This is an index of **meaning**, with a string
index next to it and a way to fuse the two.

Use vector search when you know what the code *does*. Use keyword when you know
what it is *called*. Use hybrid when you are not sure — which, in an unfamiliar
codebase, is most of the time.
