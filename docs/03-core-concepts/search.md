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

### Centrality (hybrid only, opt-in)

On an index made by 0.10 or later, every symbol has a global **rank**: a
PageRank over the symbol graph, computed at the end of each index run (calls
and instantiations count fully, inheritance a little less, type uses half,
imports less; a call from a test — a test file, or a Rust `#[cfg(test)]`
module, `mod tests` or `#[test]` function — or of low confidence passes on
less, and a helper called 300 times by one caller does not count 300 times).
Hybrid search can add it as a **third list of the fusion**: the candidates the
two retrievers already brought (after the hard filters), ordered by the rank of
the innermost symbol containing each chunk — read as its percentile within its
branch —, each adding `w / (k + position)` to its fused score.

- **Off by default in 0.10** (`search.centrality_weight: 0`). Measured on 12 of
  the 30 cases of the evaluation harness, turning it on was within the noise,
  and the one Spanish case that moved got worse; it is re-evaluated on all of
  them, by language, before it is turned on by default.
- `w` is `search.centrality_weight`, from `0` (off) to `1`; a value outside
  that range, or not a number, is bounded (`0` or `1`) and warned about.
- It only reorders: a chunk no retriever brought is never added. A chunk of no
  symbol (a whole-file summary, a doc, a config file, a memory) gets no bonus.
- The bonus is at most `w / 61`, but fused scores sit a few thousandths apart,
  so it moves hits several places, not just near-ties: on one measured case,
  with `w = 0.3`, the expected file went from 7th to 1st.
- Vector search never uses it; the kind penalty, the dedup and identifier
  anchoring come after it, so a definition the query names still comes first.
- **What changes when it is on:** the order of hybrid results and their
  `score` (each ranked hit's goes up by its bonus), and such a hit carries a
  `raw_score` — the vector + keyword fusion, without centrality, which is what
  a group search compares members on. With it off, nothing changes.
- It applies only where the branch has a current symbol graph — the same rule
  for the MCP tools, the CLI without a server and the TUI: on an index from an
  older extractor, or one whose ranks were never computed, hybrid fuses vector
  and keyword alone. The first `devctx index` of the new version computes the
  ranks without re-embedding.

`build_context` searches in hybrid mode, so the brief uses it too when it is on.

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

## Ranking: what rises and what sinks

Retrieval finds candidates; four steps decide what the agent sees first.

### File kind and the penalty

Every hit is classified from its path (and language) as one of four **kinds**,
checked in this order, first match wins:

| Kind | What matches |
|---|---|
| `test` | `test/`, `tests/`, `__tests__`, `__mocks__`, `spec/`, `e2e/`, `testdata`, `fixtures`; `*_test.*`, `test_*`, `*.test.*`, `*.spec.*`, `*.cy.*`, `conftest.py`, and `*Test` / `*Tests` / `*IT` in Java/Kotlin/Scala/Groovy/C#/PHP/Swift (`Contest.java` is code) |
| `doc` | `*.md`, `*.mdx`, `*.rst`, `*.adoc`, `*.txt`, `docs/`, `README*`, `CHANGELOG*`, `LICENSE*`, `CONTRIBUTING*` |
| `config` | `*.sql`, `*.yaml`, `*.yml`, `*.json`, `*.toml`, `*.xml`, `*.properties`, `*.ini`, `*.cfg`, `*.conf`, `requirements*.txt` |
| `code` | everything else; memories have no file and count as code |

Tests, docs and config are **demoted, not excluded**. The factor in config
(`search.penalty.test|doc|config`, default `0.6` each) is turned into a number
of positions — `round((1 − factor) × 10)`, so `0.6` ranks a hit as if it were
four places lower — because scores have no common scale across modes (cosines,
RRF fractions, cross-encoder logits). Ties go to the unpenalised hit, scores
stay monotone (a demoted hit never shows a higher score than the one above it;
`raw_score` keeps the retriever's own number), and the demotion is capped at
`(limit − 1) / 2` positions so a small `limit` cannot push the best hit out of
the answer. `1.0` turns a kind's penalty off; `0.0` sends it to the end.

```yaml
# .devctx/config.yaml
search:
  penalty:
    test: 0.6
    doc: 0.6
    config: 0.6
```

### Hard filters: `kind` and `include_tests`

When a demotion is not enough, filter:

```bash
devctx search "token refresh" --kind code        # only code
devctx search "token refresh" --kind test        # only tests
devctx search "token refresh" --no-tests         # everything but tests
```

On the MCP tool: `kind: "code" | "test" | "doc" | "config"` and
`include_tests: false`. An explicit `kind` wins over `include_tests`; an invalid
`kind` fails with ``` `kind` must be one of code, test, doc, config ```. Both
also apply to `search_project`, to group-wide search and to `build_context`.

### Dedup

The same chunk is returned once. A hit is dropped when another, better-ranked,
has the same id, the same `(file, start, end)` — the same range indexed on two
branches — or a range *contained in* a content chunk of the same file. Summary
chunks (`file`, `class`, `doc`) never swallow the code inside them: a function
behind its file's summary chunk is still a result. Dedup runs before the cut to
`limit`, so what it drops is not reported as omitted.

### Identifier anchoring

In `keyword` and `hybrid` mode BM25 rewards whoever *mentions* an identifier
most, not whoever *defines* it. So a query containing identifier-shaped tokens —
`snake_case`, `camelCase` / `PascalCase`, `Foo.bar`, `foo::bar` — looks up
their definitions in the symbol table and puts them first, marked
`anchored: true`. Bounded on purpose: at most 3 identifiers per query, at most
half the answer (never fewer than one), and a matching test copy or mock never
outranks the production definition. File names (`state.rs`, `README.md`),
versions (`v0.8.4`) and abbreviations (`e.g`) are not identifiers; neither is a
plain word. Anchored definitions are not demoted, but a hard `kind` filter still
removes them. On a 0.10 index the definitions come from the symbol table, one
per definition (a large function split in blocks pins its first block), and an
anchored hit also carries `sym`, the id of the definition it is.

## What the index leaves out

Search can only rank what was indexed. Since 0.9, vendored and generated paths
are excluded **by default** even when git tracks them — `node_modules/`,
`dist/`, `vendor/`, `third_party/`, `bower_components/`, `*.min.js`,
`*.generated.*`, and `/build/` and `/target/` at the repository root. The list,
the opt-outs and how a change reconciles are in
[Configuration](../11-configuration.md#default-excludes) and
[Keeping the index fresh](../13-keeping-the-index-fresh.md#controlling-what-gets-indexed).

## Branch awareness

Chunks are stored per `(repo, branch)`. A search returns results for the branch
you are on, so a symbol deleted on your branch does not surface from `main`.

**When your branch has no rows**, the answer does not go quiet. `search` — and
the graph tools `read_symbol`, `get_references`, `impact_analysis`,
`search_routes`, `routes_for_handler` — fall back, in this order, to the
configured default branch, then the most recently indexed branch that has rows,
and say so:

```json
"branch_fallback": {
  "current": "feat/plan-124",
  "used": "main",
  "why": "current branch feat/plan-124 is not indexed; answering from branch main"
}
```

(`is indexed but empty` when the branch has a record but no rows.) With no
indexed branch at all the call fails with `<repo> has no index for any branch;
run devctx index`. `index_status` reports `indexed: false` with
`indexed_branches` (most recent first) and a hint naming the branch that would be
used. The fallback is a lie of omission if you ignore it: the symbol you are
about to edit on your branch may differ from the one the answer describes.

### A stale extractor

The index records which extractor produced it (`index_meta`: a version plus a
hash of the language definitions). When that differs from the running binary —
or there is no record, as in any index made by 0.8.2 or earlier —
`index_status` says `extractor_stale: true` with a hint, and every graph tool
adds `warning: "index generated by an older extractor …; reindex (devctx index
--full)"`. Incremental runs never clear it: only `devctx index --full` does, and
a full run no longer copies rows from a branch that is itself stale. Until then
a **new** branch re-embeds every file instead of copying from an indexed one.
`search` itself carries no such warning; it is a retrieval, not an extraction.
If you change the grammars or `routes.rs`, bump `EXTRACTOR_VERSION` by hand: the
hash covers only the language JSON files.

Branches you want indexed are declared in config under `indexing.branches`, and
`devctx index --branch <name>` indexes a named one. Because the copy is driven by
`content_hash`, indexing a second branch copies rather than re-embeds anything
the two branches share — measured at 95–96% of files on three real
repositories.

Indexing is worktree-independent: run it from any worktree and it updates the
one index. Which `HEAD` it reads from a linked worktree is covered in
[Keeping the index fresh](../13-keeping-the-index-fresh.md#from-a-linked-worktree).

## Filters

`--language <lang>` restricts to one language. `--limit` caps results (default
10). `--kind <code|test|doc|config>` and `--no-tests` filter by file kind (see
above). `--format json` emits an object, `{"results": [...]}`, instead of the
table; each hit has `score`, `file`, `start_line`, `end_line`, `symbol`,
`symbol_type`, `level`, `language`, `kind`, `text` and, when relevant,
`raw_score` and `anchored`. The MCP tool answers the same object, plus
`branch_fallback`, `omitted` and `warning` when they apply — see
[MCP integration](mcp-integration.md#return-shapes).

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
