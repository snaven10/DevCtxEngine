# Embedding models, summarizer & tuning

A practical guide to the available embedding models, the token-budget strategies,
which configuration fits which hardware, and the behaviors verified empirically.

> **Why this matters**: the embedding model determines retrieval quality and the
> dimension of the vector store; the summarizer + token budget determine how much
> of each result reaches the LLM. Picking the wrong combination either loses
> relevant memories, corrupts identifiers, or burns CPU you don't have.

---

## 1. Available embedding models

The registry lives in `ml/devai_ml/embeddings/local.py` (`MODEL_REGISTRY`). All
run locally via `sentence-transformers`. Select with `embeddings.model` in
`config.yaml`, or with `devai model use <key>`.

| Key | Model | Dims | Size | Speed | Language | Best for |
|-----|-------|------|------|-------|----------|----------|
| `minilm-l6` | all-MiniLM-L6-v2 | 384 | 22 MB | very fast | 🇬🇧 English | resource-constrained machines, English code/text |
| `minilm-l12` | all-MiniLM-L12-v2 | 384 | 33 MB | fast | 🇬🇧 English | slightly better than L6, still lightweight |
| `bge-small` | BAAI/bge-small-en-v1.5 | 384 | 33 MB | fast | 🇬🇧 English | better English retrieval than MiniLM |
| `bge-base` | BAAI/bge-base-en-v1.5 | 768 | 110 MB | medium | 🇬🇧 English | top English precision, large repos |
| `ml-minilm` | paraphrase-multilingual-MiniLM-L12-v2 | 384 | 470 MB | fast | 🌍 50+ langs | **fast multilingual**, small machines with non-English content |
| `ml-mpnet` | paraphrase-multilingual-mpnet-base-v2 | 768 | 1.1 GB | medium | 🌍 50+ langs | **best multilingual quality**, machines with a decent CPU or a GPU |

### Which one to pick

- **Non-English / mixed content** → `ml-minilm` (fast) or `ml-mpnet` (best quality).
  Neither needs `query:`/`passage:` prefixes — both are drop-in with the current
  `encode()` call.
- **English only** → `bge-base` (best) or `minilm-l6` (lightest).
- **Avoid the `e5` family**: they underperform here because the local provider
  doesn't add the `query:`/`passage:` prefixes those models require.

> ⚠️ **Changing the model changes the vector dimension** (384 ↔ 768). The vector
> store is incompatible across dimensions → it **forces a full re-index**. See §6.

---

## 2. The response pipeline: rerank → token budget

When you call `recall` or `search`, the flow is:

```
vector search (top_k_fetch)  →  reranker  →  token budget (fit)  →  response
```

1. **Reranker** (`DEVAI_RERANK_*`): defaults to `flashrank`
   (ms-marco-MiniLM-L-12-v2). Reorders by relevance and trims to `limit`.
   The default model is **English** — it reorders well but yields lower scores on
   cross-lingual queries (an English query against a non-English memory ranks #1
   correctly but with a score near ~0.37). For non-English content, set
   **`DEVAI_RERANK_MODEL=ms-marco-MultiBERT-L-12`** — a multilingual flashrank
   model (same ONNX/CPU speed, ~150 ms for 15 candidates). Measured: the same
   cross-lingual query jumps from **~0.37 → ~0.99**. No re-index needed — the
   reranker runs at query time only. Other flashrank options:
   `ms-marco-TinyBERT-L-2-v2` (fastest), `ms-marco-MiniLM-L-12-v2` (default,
   English), `ms-marco-MultiBERT-L-12` (multilingual).

2. **Token budget** (`DEVAI_TOKEN_*` + `DEVAI_SUMMARIZER_*`): fits the content
   under `DEVAI_MAX_OUTPUT_TOKENS`. This is where drop/summarize/truncate happens.

### The per-item budget formula

```
per_item_budget = max(DEVAI_MAX_OUTPUT_TOKENS / limit, 128)
```

Each memory that **fits** its slice is returned **verbatim**; one that exceeds it
is processed by the strategy. With `MAX_OUTPUT_TOKENS=8000`:

| `limit` | slice/item | effect |
|---------|------------|--------|
| 4 | 2000 tok | almost everything verbatim |
| 8 | 1000 tok | medium verbatim, large summarized |
| 12 | 666 tok | many summarized |
| 18 | 444 tok | almost all summarized |

**Rule of thumb**: a memory stays verbatim ⟺ `memory_size ≤ 8000 / limit`. A
600-token memory is verbatim up to `limit ≤ 13`; a 2000-token one, up to `limit ≤ 4`.

---

## 3. Token-budget strategies (`DEVAI_TOKEN_STRATEGY`)

| Strategy | What it does | CPU cost | Drops items | Recommendation |
|----------|--------------|----------|-------------|----------------|
| `drop` | discards whole items from worst-ranked down until it fits | **zero** | **YES** ❌ | avoid for memories — hides relevant results |
| `soft_truncate` | cuts each large item at a sentence boundary (keeps the head) | **zero** | no | good for small machines / browsing |
| `hard_truncate` | cuts at an exact char count | zero | no | rarely |
| `summarize` | summarizes each large item with the summarizer | depends on summarizer | no | **recommended** with `extractive` |

> **The original bug**: with `drop` + `MAX_OUTPUT_TOKENS=4000`, one or two large
> memories filled the budget and the rest were **silently dropped**
> (`items_dropped: 9`) → you'd conclude "that memory doesn't exist" when it did.
> Any strategy other than `drop` keeps `output_count == input_count`.

---

## 4. Summarizers (`DEVAI_SUMMARIZER_PROVIDER`)

| Provider | Type | Local | Verdict |
|----------|------|-------|---------|
| `noop` | none | ✅ | with `strategy=summarize` it falls back to truncation — useless |
| **`extractive`** | extractive (picks sentences by similarity to the query) | ✅ | **recommended**: reuses the embedding model, never corrupts identifiers, finds buried content |
| `flan-t5` | abstractive (generates text) | ✅ | **do NOT use for code/non-English**: corrupts identifiers (e.g. a `getStatusById` symbol comes out `getStatuById`) and words, 512-token input limit, slow. Patched for transformers 5.x but still not recommended |
| `openai` | abstractive cloud | ❌ | blocked by `require_local=true` (data exfiltration guard) |

**`extractive` is the right choice** for a code-memory tool:
- Preserves identifiers **verbatim** (it picks whole sentences, never splits words).
- It is **query-focused**: it surfaces the sentences relevant to your query, even
  when they sit at the end of a long memory.
- It reuses the already-loaded embedding model → no extra download.

---

## 5. Recommended configuration by hardware

The heaviest CPU factor is the **embedding model** (ml-mpnet 768d is ~5x slower
than minilm-l6 on CPU). The summarize strategy is secondary (`extractive` adds
~0.5–1 s per recall to embed sentences; `soft_truncate` is free).

### 🖥️ Small / no-GPU (or weak GPU) machine, non-English content
```jsonc
DEVAI_EMBEDDING_MODEL    = "ml-minilm"        // 384d multilingual, fast
DEVAI_EMBEDDING_DEVICE   = "cpu"
DEVAI_TOKEN_STRATEGY     = "soft_truncate"    // zero extra CPU, drops nothing
DEVAI_MAX_OUTPUT_TOKENS  = "6000"
DEVAI_RERANK_PROVIDER    = "flashrank"
```
> **Should `drop` and `summarize` be off on a small PC?** `drop`: yes, always off
> (it loses memories — never worth it). As for `summarize`: on a small machine
> prefer `soft_truncate` instead — it keeps **all** memories and spends **no**
> extra CPU (no sentence embedding). Use `summarize`+`extractive` only if you can
> afford ~1 s more per recall in exchange for query-focused summaries.

### 🖥️ Powerful / GPU machine, non-English content
```jsonc
DEVAI_EMBEDDING_MODEL    = "ml-mpnet"         // 768d multilingual, best quality
DEVAI_EMBEDDING_DEVICE   = "cpu"              // or "cuda" with a good GPU
DEVAI_TOKEN_STRATEGY     = "summarize"
DEVAI_SUMMARIZER_PROVIDER= "extractive"
DEVAI_MAX_OUTPUT_TOKENS  = "8000"
```

### 🖥️ English-only content
```jsonc
DEVAI_EMBEDDING_MODEL    = "bge-base"   // or "minilm-l6" on a small machine
DEVAI_TOKEN_STRATEGY     = "summarize"
DEVAI_SUMMARIZER_PROVIDER= "extractive"
```

### Measured cost (CPU only, no GPU — old Maxwell laptop GPU, CPU fallback)
- `ml-mpnet`: ~225 ms per memory embed; ~27 chunks/sec in batch.
- Full re-index of a large repo (~1500 files, ~7000 chunks, 58k edges): ~2 h.
- Typical recall: ~1–2 s. (`minilm-l6` was ~5x faster.)

---

## 6. Verified behaviors

An empirical test battery over real memories with `ml-mpnet` + `extractive`:

| Test | What was measured | Result |
|------|-------------------|--------|
| Content at the END | query targeting the last paragraph | `summarize`/extractive **finds it** ✅; `soft_truncate` misses it ❌ |
| Verbatim threshold | budget sweep | verbatim if `budget ≥ memory size`; summarized below that |
| Minimum budget (60 tok) | extreme compression | coherent, **identifiers intact, zero corruption** |
| 3 strategies | drop/summarize/soft_truncate | drop = all-or-nothing; summarize = compresses the relevant part; soft = linear |
| Cross-lingual query | query in language A, memory in language B | correct #1 match — score ~0.37 with the English reranker, ~0.99 with `ms-marco-MultiBERT-L-12` |
| Code (`search`) | — | forces `drop` automatically — **code is never summarized** (avoids corrupting identifiers) |

**Conclusions**:
- `extractive` surfaces relevant content even when buried deep in a long memory
  → it is the correct strategy for targeted recall.
- Cross-lingual retrieval works thanks to the multilingual model.
- `summarize`/`soft_truncate` never lose memories (`output_count == input_count`).

### Usage cheat sheet

| You want… | Configure / use |
|-----------|-----------------|
| The exact detail of a specific thing | `limit 3-5` → full verbatim |
| To explore a broad topic | `limit 12-18` → many results to the point, none lost |
| To query in another language | nothing — `ml-mpnet`/`ml-minilm` bridge it |
| Always surface the relevant bit even if buried | `summarize` + `extractive` |

---

## 7. Gotchas when migrating models (learned in production)

1. **`config.yaml` overrides the env var.** The Go CLI (`devai index`) and the MCP
   read `embeddings.model` from `config.yaml` and pass it to Python, **overriding**
   `DEVAI_EMBEDDING_MODEL`. **Each repo has its own `.devai/config.yaml`**, plus
   one at the workspace root and one in `state/`. Changing only the env is not
   enough: run `devai model use <key>` in EACH repo, or edit every `config.yaml`.
   (The template default lives in `cmd/devai/cmd/init.go`.)

2. **Wiping `vectors/` is not enough — clear `file_state`.** The re-index checks a
   per-file hash in the `file_state` table (in `index.db`) and **skips** matches,
   even when the vectors no longer exist. `--incremental=false` does NOT bypass
   the hash check. You must `DELETE FROM file_state` (and `index_state`) to force
   a re-embed. **`index.db` holds the memories and the graph → do NOT delete it**,
   only those two tables. Memories are re-embedded with a standalone script that
   reads them from SQLite and re-embeds `f"{title} {content}"` (there is no native
   re-embed command).

3. **The idle watchdog (1800 s) kills a long re-index.** `index_repo` is a single
   long RPC call; the watchdog measures "idle" as time since the last *new*
   request, not CPU activity. A large repo with a heavy model takes > 30 min → the
   watchdog kills the ML service (`reading response: EOF`). For re-indexing set
   `DEVAI_ML_IDLE_TIMEOUT_SEC=0`.

### Full model-switch procedure
```bash
# 1. switch the model in EVERY config.yaml
for r in repoA repoB ...; do (cd "$r" && devai model use ml-mpnet); done
# 2. stop the MCP / ML service (release the LanceDB)
# 3. wipe the vector store (keeps index.db = memories + graph)
rm -rf "$DEVAI_STATE_DIR/vectors"
# 4. clear file_state + index_state in index.db (NOT memories)
#    sqlite3 index.db "DELETE FROM file_state; DELETE FROM index_state;"
# 5. re-index each repo with the watchdog disabled
for r in repoA repoB ...; do
  (cd "$r" && DEVAI_ML_IDLE_TIMEOUT_SEC=0 devai index --incremental=false)
done
# 6. re-embed memories with the new model (standalone script)
# 7. reconnect the MCP
```

---

## 8. Where each configuration lives

| File | Read by | Purpose |
|------|---------|---------|
| `<repo>/.devai/config.yaml` | CLI `devai index` (from that repo) | model + excludes when indexing that repo |
| `<workspace>/.devai/config.yaml` | MCP (cwd = root) | model for the MCP service |
| `<workspace>/.devai/state/config.yaml` | shared-state resolution | shared `state_dir` |
| `.mcp.json` (client env) | MCP at runtime | strategy, summarizer, max_tokens, rerank, idle timeout |

**They must all use the SAME model**, or gotcha #1 reappears.
