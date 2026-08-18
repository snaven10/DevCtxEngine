# Models and Tuning

> 🇪🇸 [Leer en español](es/09-modelos-embeddings-y-tuning.md)

Choosing an embedding model, and the handful of knobs that change behaviour
rather than decoration.

For the full config schema and every environment variable, see
[Configuration](11-configuration.md). This page is about *which* values to pick
and why.

---

## The one decision that is hard to undo

**Choose the embedding model before your first index.**

Vectors from two models do not live in the same space, so changing the model
means re-indexing every file *and* re-embedding every memory. Everything else on
this page can be changed later.

```bash
devctx models
```

| Model | Dims | Languages | Notes |
|---|---|---|---|
| `minilm-l6` | 384 | English | Smallest and fastest; the built-in fallback |
| `minilm-l12` | 384 | English | Slightly better than L6, still light |
| `bge-small` | 384 | English | Better English retrieval than MiniLM |
| `bge-base` | 768 | English | Best English precision; 768-wide, so a larger store |
| `ml-minilm` | 384 | 50+ | Fast multilingual, no files to fetch |
| `ml-mpnet` | 768 | 50+ | 768-wide; **128-token input cap** |
| `ml-granite` | 384 | multilingual | **Recommended for non-English code.** Best multilingual on CPU: top recall, fastest indexing |
| `ml-granite-lg` | 768 | multilingual | 768-wide sibling; `ml-granite` matches it on CPU |

### How to choose

**Is your code or are your comments not in English?** Pick a multilingual model.
The English models will embed Spanish perfectly happily — just badly. This is the
most common wrong choice, and it fails quietly: search still returns results,
they are just worse than they should be, and nothing tells you.

**Otherwise**, 384 dimensions is the right default. It indexes faster and stores
smaller, and `ml-granite` measured at least as good as its 768-wide sibling on
CPU. Reach for 768 when you have measured that you need it, not before.

Watch `ml-mpnet`'s **128-token input cap** — chunks longer than that are
truncated before embedding, which silently discards the tail of every function
over about 500 characters.

### Getting the files

The `FILES` column in `devctx models` says what each needs:

- `automatic` — downloads itself on first use.
- `download` — needs `devctx models --download <model>` once.
- `ready` — already on this machine.

Downloads land in a shared cache, so a second project using the same model
fetches nothing.

## Providers

```yaml
embeddings:
  provider: local        # local (default) | openai | voyage | custom
  model: ml-granite        # registry key; ships as minilm-l6
  model_dir: ""          # a directory holding your own ONNX model
  offline: auto          # auto (default) | true | false
```

**`local`** runs fastembed in-process. No network after the model files are
present, and no data leaves the machine.

**`openai` / `voyage`** call an API. Your code goes to a third party — a real
decision, not a performance one.

**`custom`** loads an ONNX model you supply. Point `model_dir` at a directory
holding the ONNX file plus `tokenizer.json` and `config.json`. Accepted
filenames, in order: `onnx/model_quint8_avx2.onnx`, `onnx/model.onnx`,
`model_quint8_avx2.onnx`, `model.onnx`.

A custom provider has no registry entry, so its width must be declared via
`DEVCTX_EMBED_DIMENSION` (default 384 if unset — and a wrong value here corrupts
the store, so set it).

`model_dir` in the config wins over the `DEVCTX_MODEL_DIR` environment variable.
Setting it in config means no shell export is needed.

## Memory and batching

Two environment variables, both mattering only when the machine is tight:

| Variable | Default | Effect |
|---|---|---|
| `DEVCTX_EMBED_MAX_CHARS` | 4096 | Characters per text fed to the encoder. `0` disables the cap. |
| `DEVCTX_EMBED_BATCH_SIZE` | 32 | Texts per encoder batch. |

These interact. A single very long chunk pads the entire batch to its length, so
a large batch *and* a high character cap is what produces the memory spike — not
either alone. On a constrained machine, lowering `DEVCTX_EMBED_MAX_CHARS` to
2048 is usually the more effective of the two, because it attacks the padding
rather than the count.

## Storage tuning

```yaml
storage:
  hnsw: true            # approximate nearest-neighbour index
  metric: cosine        # cosine (default) | ip
  fts: false            # BM25 index, enables `search --keyword`
```

**`hnsw`** is on by default, on measurement: 84 ms → 49 ms on a 17k-vector store
with recall@10 unchanged. Turning it off buys a slower search and nothing else.

**`metric: ip`** (inner product) skips the norm computation cosine pays on every
comparison, so it is measurably cheaper. But **the two only rank identically
when embeddings are unit-normalized.** The local providers normalize; an API or
custom provider that does not would silently rank by magnitude instead of
direction. That is why `ip` is opt-in — the failure mode is wrong results, not
an error.

**`fts`** builds the BM25 index that `search --keyword` and hybrid search need.
Without it, hybrid degrades to vector-only silently.

Both indexes are dropped during a bulk index and rebuilt after — see
[Performance](07-performance.md).

## Reranking

```yaml
reranking:
  enabled: false        # off by default
  model: bge-base       # bge-base | bge-v2-m3 | jina-turbo | custom
  model_dir: ""
  pool: 100
```

Off by default because it was measured: 30 ms → 8.6 s at best, 30 s with
`bge-reranker-base`, and the one model measured across the whole bench made
results worse. Full figures in [Design Decisions ADR-15](08-design-decisions.md).

If you turn it on, the two things that matter:

- **`pool` is the ceiling.** A reranker reorders what it is handed. An answer
  ranked below the pool is invisible to it, however good the model.
- **`pool` is also the whole cost.** It multiplies the slowest stage. Deep pool
  with a small model, or shallow pool with a large one — not both.

The built-in cross-encoders are all over a gigabyte, because fastembed ships no
lightweight one. `model_dir` is the way around that: point it at an ONNX export
of something like `ms-marco-MiniLM-L-12-v2`, an order of magnitude smaller.

## Summarization

```yaml
summarization:
  provider: extractive   # extractive (default) | openai | noop
  require_local: true    # privacy guard: blocks non-local providers
  target_tokens: 200
  model: gpt-4o-mini     # only for API providers
```

**`extractive`** selects sentences from the source rather than generating text.
It runs locally, costs nothing, and — the reason it is the default for code —
**preserves identifiers verbatim.** A generated summary paraphrases
`AuthMiddleware::authenticate` into "the authentication method", which is exactly
the token you would have searched for.

`require_local: true` is a guard, not a preference: it blocks the API providers
outright, so a config change cannot quietly start sending code to a third party.

## Indexing scope

```yaml
indexing:
  exclude:
    - "**/node_modules/**"
    - "**/*.generated.ts"
  branches:
    - main
    - develop
```

**`exclude`** uses `.gitignore` syntax and the same matcher, so a pattern
behaves identically here and there — and identically whether a file arrives via
`index`, the post-commit hook, or `watch`. It is for code git *does* track but
that is not worth searching. Anything already git-ignored needs no rule.

**`branches`** is declared, not inferred, and that is the whole point:

- A repository with worktrees has several branches live at once, and nothing
  about the checked-out one says which of the others matter.
- Guessing a base from the git graph gets it wrong in the ordinary case — two
  feature branches off the same parent — and gets it wrong *silently*, answering
  searches with another branch's code.
- It is what makes pruning safe. This list defines what belongs in the index, so
  anything else can be dropped. Without it, there is no way to tell a live branch
  from one merged and deleted six weeks ago, and the index only ever grows.

The first entry is the default: what `devctx index` targets with no `--branch`,
and what search falls back to when the checked-out branch is not indexed.

An empty list means "whatever is checked out", which is correct for a repository
with one branch and no worktrees.
