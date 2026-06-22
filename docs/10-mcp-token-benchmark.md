> 🇪🇸 [Leer en español](es/10-benchmark-tokens-mcp.md)

# Token & Cost Benchmark: MCP (Filtered Retrieval) vs. Direct Mode (Brute-Force Dump)

A controlled A/B measuring how DevAI's MCP affects token consumption and cost on a real task. The **same
diagnostic task** was solved twice, varying only whether the agent had access to DevAI's tools (vector
retrieval + memory) or was restricted to `grep`/`read`.

> **Methodology**: both sessions started from a clean context, used the same model, and ran the same
> analysis/comprehension task (**0 lines of code changed** in both → no implementation variance). Figures
> taken from Claude Code's `/cost` at the end of each session.

---

## 1. Executive Summary

| Metric | With DevAI MCP (filtered) | Without MCP (direct dump) | Impact |
|---|---|---|---|
| **Total cost** | **$1.19** | **$4.14** | **+247.9%** without MCP · **71.3% savings** with MCP |
| Total token volume | ~0.67 M | ~8.27 M | **~12× more** without MCP |
| Total cache read | 543.5 k | 7.56 M | ~14× more without MCP |
| Output tokens | ~6.0 k | ~53.2 k | ~8.9× more without MCP |
| API duration | 11 min 36 s | 11 min 28 s | Practically identical |
| Wall time | 20 min 48 s | 12 min 56 s | +7 min 52 s with MCP *(local latency, see §4)* |
| Lines of code changed | 0 | 0 | Analysis task in both runs |

**Headline:** on a diagnostic task, DevAI MCP cut cost by **71.3%** and moved **~12× less token volume**, in
exchange for higher *wall time* that is attributable to local indexing latency — not to the API.

---

## 2. Per-Model Breakdown

| Configuration | Input | Output | Cache Read | Cache Write | Partial Cost |
|---|---|---|---|---|---|
| **With MCP** · claude-haiku-4-5 | 594 | 19 | 0 | 0 | $0.0007 |
| **With MCP** · claude-opus-4-8 | 14.0 k | 6.0 k | 543.5 k | 112.2 k | $1.1900 |
| **Without MCP** · claude-haiku-4-5 | 6.1 k | 25.5 k | **7.5 M** | 688.5 k | $1.7400 |
| **Without MCP** · claude-opus-4-8 | 14.1 k | **27.7 k** | 61.9 k | 256.1 k | $2.4000 |

---

## 3. The Two Drivers of the Savings

The overhead of direct mode does **not** come from a single factor. There are two, worth documenting
separately:

### 3.1 Driver A — Cache read: re-injecting whole repositories
Without an intermediate filter, the agent dumps and re-reads large slices of the repository each turn.
Prompt cache reads them again and again: **7.5 M cache-read tokens on Haiku alone**, vs **543.5 k with MCP**
(~14× less). MCP acts as a smart indexer: it pre-filters with vector search + memory and hands the main
model a **clean, bounded** context.

### 3.2 Driver B — Output blow-up: longer, more redundant answers
Less obvious but just as relevant: **without MCP, Opus generated 27.7 k output tokens vs 6.0 k with MCP
(4.6×)**. Synthesizing from dumped raw code makes the model ramble and repeat. And **Opus output is the most
expensive, non-cacheable component** — which is why the no-MCP Opus cost $2.40, driven by its output, not
its cache. **Clean context → shorter, sharper answers → less expensive output.**

> Net: direct mode overpays on **two fronts** — massive cache reads *and* inflated output.

---

## 4. The Wall-Time Trade-off (it's local, not the API)

MCP mode was ~8 min slower in *wall time* (20:48 vs 12:56), but **API time was practically identical**
(11:36 vs 11:28). The model did **not "think more"**. The extra minutes are **DevAI local latency**: the ML
service computing embeddings on CPU (hardware without a dedicated GPU) plus protocol round-trips.

**Practical implication:** that time overhead is *CPU-bound and tunable* — with a dedicated GPU, or with a
lighter embedding model. It is not inherent to the MCP architecture, but to the execution environment.

---

## 5. Influence of the Embedding Model (this benchmark used the heaviest)

This A/B ran with **`ml-mpnet` (paraphrase-multilingual-mpnet-base-v2, 768 dim)** — the **heaviest,
highest-quality** embedding model in the installation. That matters when reading the results, because each
metric reacts differently to model weight:

- **The cost/token savings (~71%) are essentially model-independent.** They come from *filtering* (retrieval
  returns bounded fragments instead of dumping repos), not from model weight. A lighter model filters too →
  the savings stay in the same order of magnitude.
- **Wall time WOULD change — for the better.** The time penalty (§4) comes from embedding compute on CPU. A
  lighter model is much faster:
  - `ml-minilm` (384 dim, multilingual): ~5× faster than `ml-mpnet` on CPU.
  - `minilm-l6` (384 dim, English): faster still (22 MB vs 1.1 GB).
  → The ~8 min wall-time gap **would shrink substantially** with either.
- **The price: retrieval precision.** `ml-mpnet` gives the best ranking, especially for non-English content.
  A lighter model may surface slightly less relevant results → occasionally the agent runs an extra search
  or reads a bit more, **marginally eroding** the token savings without changing the order of magnitude.

| Model | Dim | Speed (CPU) | Retrieval quality | When to use |
|---|---|---|---|---|
| `ml-mpnet` *(used here)* | 768 | slow (~225 ms/embed) | **best** (multilingual) | Max precision; CPU-capable box or GPU |
| `ml-minilm` | 384 | ~5× faster | good (multilingual) | **Balance** of speed/quality on modest machines |
| `minilm-l6` | 384 | fastest | lower (English) | Speed priority / English content |

> **Honest reading:** these figures are the **highest-quality, highest-wall-time** scenario. With a lighter
> model you would get **the same cost savings (~71%) with noticeably less time penalty**, trading some
> retrieval precision. The financial savings are robust; the time cost is tunable per model.

---

## 6. Scope & Domain Caveat

The 71% savings correspond to a **diagnostic / comprehension** task — exactly where a brute-force dump is
mostly redundancy. The gap **narrows** on tasks that genuinely require touching every file (e.g. a large
refactor), because there the content is read either way.

> Operating rule: **MCP pays off more the more the task is "find the needle in the haystack,"** and less
> when the task is "touch the whole haystack."

---

## 7. Conclusions & Recommendation

**Key findings:**
- **Cost:** **71.3% savings** ($1.19 vs $4.14) on the measured diagnostic task.
- **Volume:** **~12× fewer** total tokens moved; **~14×** less cache read; **~8.9×** less output.
- **Double savings:** MCP trims both *context re-reads* (cache) and *answer verbosity* (expensive Opus
  output).
- **Time cost:** higher *wall time*, but from **local (CPU) indexing latency**, not the API — tunable with a
  GPU or a lighter embedding model (§5).

**Recommendation:** for diagnostic and code-comprehension tasks, DevAI MCP is **strongly recommended**: the
financial savings (~71%) and token-volume reduction (~12×) outweigh the wait-time cost, which is itself
tunable at the hardware/model level. For refactors that require reading the whole codebase, evaluate case by
case.

---

*A/B run on 2026-05-29 · diagnostic task over a real multi-repo workspace · embedding model `ml-mpnet`
(768 dim, the heaviest available) · figures from Claude Code's `/cost`.*
