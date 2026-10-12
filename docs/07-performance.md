# Performance

> 🇪🇸 [Leer en español](es/07-rendimiento.md)

What costs what, which figures were actually measured, and how to measure your
own.

---

## Read this first

Every number on this page was measured on one developer machine — a CPU-only
WSL2 box — against real repositories. **They are orders of magnitude, not
guarantees.** Where a figure was not measured, this page says so rather than
estimating.

The one thing that generalises: **embedding dominates indexing, and everything
else is noise next to it.** Optimise the number of chunks you embed and you have
optimised indexing.

## Indexing

### What drives the cost

In descending order of impact:

1. **Chunks embedded.** The whole game.
2. **Whether the HNSW index is present during the load.** See below — this is a
   larger factor than most people expect.
3. **Model width.** A 768-dimension model is roughly twice the vector work and
   twice the storage of a 384-dimension one.
4. Parsing and chunking. Real, but small.

### The HNSW effect

DuckDB maintains an HNSW index on every insert. Measured on a ~1,300-file Java
backend, same machine, same run shape:

| Index during load | Throughput |
|---|---|
| HNSW present | ~7 files/min |
| HNSW dropped, rebuilt after | ~58 files/min |

An 8× difference. This is why indexing drops the derived indexes and rebuilds
them at the end, and why you should not build an HNSW index and then bulk-load
into it.

### Incremental vs full

Incremental indexing is the single most important performance feature, because
the common case is a handful of changed files rather than the repository.

```bash
devctx index              # incremental: only what git says changed
devctx index --full       # everything
devctx index --branch x   # a named branch
```

A chunk whose `content_hash` is unchanged is not re-embedded, so an incremental
run over a commit touching three files costs three files' worth of embedding,
not the repository's.

**Indexing reads the work tree, not the last commit.** `--full` does not discard
uncommitted work.

### Multi-branch

Indexing a second branch is far cheaper than indexing a repository, because the
same content hashing that makes incremental runs cheap also means branches share
rows. Measured on three repositories:

| Repository size | Files | Copied instead of embedded |
|---|---|---|
| ~1,400 files (TypeScript) | 1,406 | 1,343 (96%) |
| ~1,300 files (Java) | 1,297 | 1,251 (96%) |
| ~150 files (Java) | 153 | 146 (95%) |

So the marginal cost of a second declared branch is roughly 4–5% of a full
index, not 100%.

## Search

Measured on this repository (128 files, 2,333 chunks, 384-dimension model):

| Configuration | Latency | Resident memory |
|---|---|---|
| Vector search, no reranking | ~30 ms | ~406 MB |
| With the cheapest cross-encoder | ~8.6 s | ~2.4 GB |
| With `bge-reranker-base` | ~30 s | ~3.4 GB |

Reranking is off by default because of this table, and because the one model
measured across the whole bench made results *worse* — it demoted a correct
answer from first place to twenty-first. See
[Design Decisions ADR-15](08-design-decisions.md).

Keyword (BM25) and hybrid search were not separately benchmarked. Hybrid runs
both retrievers, so treat it as at least the cost of the vector path.

## The symbol graph (0.10)

Measured while 0.10 was built, October 2026, on the same CPU-only machine —
often loaded by other work, so read the figures as orders of magnitude. The
repositories: this one (≈440 files at the time), two private Java repositories
(1 250 and 216 files), a subset of a private Angular workspace and, for
`build_context`, that workspace whole. Graph tools were measured warm, each call
opening the store as the server does, 40 calls per case.

**`impact_analysis`** (2026-10-08 to 2026-10-10), p95 with the defaults
(high/medium calls, no tests or library callees, 200 per direction, dispatch on),
against the 0.9 walk on the same index:

| Case | 0.10 | 0.9 |
|---|---|---|
| `get`, 1 250-file Java repository | 40.7 ms | 4 165 ms (1 855 names walked) |
| `map`, same repository | 85.9 ms | 5 724 ms (2 571 names) |
| worst of 8 cases there, dispatch on, quiet machine | 117.9 ms | — |
| worst of every case, under load | 242 ms | — |
| `get` / `new`, this repository, before dispatch existed | 43.5 / 61.1 ms | 805 / 1 550 ms |

Two causes, measured apart: one query per level instead of one per name, and a
bare name meaning the repository's definitions instead of every call written
with it. Following dispatch costs 2–15 ms, and up to 70 ms where an interface
has several implementations.

**`traverse`** (2026-10-09): p95 at depth 2 between 16.8 and 47.5 ms over 16
cases on the 1 250-file Java repository (calls, references, imports, contains,
implements/inherits, instantiates, all relations); a synthetic worst case
(3 000 definitions of one name, 1 000 implementing an interface) 93–126 ms after
a fix that brought it down from 2.2–3.5 s (2026-10-10).

**`repo_map`** (2026-10-09/10), 1 024-token budget: warm p50 / p95 20.9 /
34.3 ms on the 1 250-file Java repository (22 801 symbols; first load 104 ms to
read and build the cached adjacency, ≈7.4 MB), 19.2 / 26.7 ms on the Angular
subset, 12.0 / 16.5 ms on this repository. Never over the budget (largest map
996 tokens); with a neighbouring file as the focus, the expected file was in
the map in 16 of 17 cases.

**`skeleton`** (2026-10-11): 4.9–10.2 % of the file's tokens on 12 files of
623–4 684 lines in four repositories; p95 ≤ 26 ms.

**`build_context` with `related: true`** (2026-10-12), 4 096-token brief, p50
without → with the section: this repository 92 → 116 ms, the 216-file Java one
78 → 87 ms, the 1 250-file one 142 → 179 ms, the Angular workspace 132 →
186 ms (machine load 14–16). Expected file in the brief: 21 → 22 of 26 cases.

**Ranks** (2026-10-09): the global PageRank adds 21–165 ms to a link pass
(3 000 to 22 876 symbols), and hybrid search with centrality on 0–6 ms of p50.

### Indexing cost of 0.10 against 0.9.0

Not measured yet on equal terms: indexing from scratch, database size and peak
memory of 0.10.0 against 0.9.0, the evaluation harness on all 30 cases (with the
centrality weights and by language) and the upgrade of real 0.9.0 indexes are
the field verification before the release (PLAN-009 TASK-016). *To be
completed in TASK-016.* The partial figures measured so far — vector reuse, the
link pass, database growth — are in
[Keeping the index fresh → What it costs](13-keeping-the-index-fresh.md#what-it-costs).

## Storage

Real figures from this repository's store:

| Quantity | Value |
|---|---|
| Files indexed | 128 |
| Chunks | 2,333 |
| Symbols | 1,599 |
| Store on disk | 17 MB |

Which works out to roughly **18 chunks per file** and **~7 KB per chunk** at 384
dimensions. A 384-dimension `f32` vector is 1.5 KB of that; the rest is chunk
text, graph rows and index structures.

Doubling the model width roughly doubles the vector portion. It does not double
the text.

### Where it lives

| Path | Holds |
|---|---|
| `.devctx/` in the repository | That project's index and config |
| `~/.local/share/devctx/` | Project registry, global and group memories, model files |

`.devctx/` should be gitignored. The central directory also holds downloaded
model files, which is usually most of its size — check before assuming your
memories are large.

## Tuning

### Exclude what you would never ask about

The highest-leverage setting, because it removes chunks rather than making them
cheaper.

```yaml
indexing:
  exclude:
    - "**/node_modules/**"
    - "**/target/**"
    - "**/dist/**"
    - "**/*.min.js"
    - "**/*.lock"
```

`.gitignore` is applied first and is the coarser tool; `exclude` is for things
git tracks but you never ask about — vendored code, generated clients, fixtures.

**Known caveat:** changing `exclude` between runs is not reflected in the
content hash, so branch-copy dedup can carry over rows the new exclusions would
have dropped. Run `devctx index --full` after changing it.

### Pick the model once

384-dimension models index faster and store smaller. The default for new
projects is `ml-granite` (384, multilingual), which on CPU measured best on both
recall and indexing speed of the multilingual options.

Changing the model after indexing means re-indexing every file *and* re-embedding
every memory, because vectors from two models are not comparable. Choose before
the first index.

### Keep the index fresh cheaply

```bash
devctx hooks install     # re-index on commit; costs nothing when idle
devctx watch             # re-index on save; a process, but immediate
```

The hook is the cheapest automation that works. See
[Keeping the Index Fresh](13-keeping-the-index-fresh.md) for all four options.

## Resource usage

**One process.** Parsing, chunking, embedding and reranking are in-process
Rust — there is no sidecar holding a second copy of anything, and no
serialization boundary between stages.

Embedding runs **one batch at a time**, 8 texts per batch by default
(`DEVCTX_EMBED_BATCH_SIZE`): peak memory scales with the batch, and the old
parallel split peaked at 6.5 GB at batch 8 and 18.2 GB at batch 32. A model that
nothing has used for 5 minutes is dropped (`DEVCTX_MODEL_IDLE_SECS`), in project
servers and in the central daemon alike.

Resident memory is dominated by the loaded model. The ~406 MB figure above is a
384-dimension embedding model plus the store; enabling a cross-encoder adds
gigabytes, which is the real reason it defaults off.

**CPU-only is the assumed case.** Nothing here requires a GPU, and every
figure on this page is CPU. A build with `--features gpu` can run the local
embedder and reranker on an NVIDIA GPU (`device: cuda`); it is opt-in, unmeasured
here, and described in [Models and Tuning](09-models-and-tuning.md#gpu-cuda).

**Network:** only model downloads on first use, and only for models whose files
are not already present. `devctx models` shows which of those apply. With a
local embedding provider, indexing and search make no network calls.

## Measuring your own

```bash
devctx status                  # files, chunks, symbols, model, freshness
devctx projects list           # every repository, size, and index age
time devctx index --full       # your indexing throughput
time devctx search "..."       # your search latency
```

`devctx status` emits JSON, so it is scriptable. If your figures differ wildly
from this page, the usual causes are model width, an HNSW index present during a
bulk load, or a repository full of vendored code that `exclude` should be
removing.
