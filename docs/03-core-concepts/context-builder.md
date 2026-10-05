# Context Builder

> 🇪🇸 [Leer en español](../es/03-conceptos-fundamentales/constructor-de-contexto.md)

One budgeted brief for one question: what is already known, the code that ranks
highest, and the knowledge recorded against exactly that code.

---

## What it is

```bash
devctx context "how do we decide a token is expired" --max-tokens 4096
```

Agents call the `build_context` MCP tool. The output is **prose, not JSON** — it
is meant to be read straight into a model's context, and a JSON envelope around
code and prose spends budget on punctuation.

## Why it exists

An agent facing an unfamiliar area does three searches, reads four files, and
runs a recall — spending a large slice of its window on retrieval before it has
started thinking. Worse, it gets the *code* and misses the reasoning, because it
never thought to ask what had already been decided.

`build_context` does that assembly once, under a stated ceiling, and returns a
single artifact.

## The three passes

The order is the design. Each pass narrows what the next needs to say.

### 1. What is already known

A `recall` against the question, across all scopes, limit 5.

**First, because it is the part no amount of reading the code recovers**, and
because it is small. Code tells you what happens; it does not tell you that the
obvious alternative was tried and abandoned.

Each memory is shown as `[memory] <id> — <title>` plus its first lines (6 lines
or 500 characters), with no per-item truncation stamp, and the memories as a
whole may use at most **35%** of the budget so a long recall cannot starve the
code. A memory is not repeated as code when the brief quotes it literally.

### 2. Code

A hybrid search for the question (vector + keyword, no reranker), fetching 30
hits, with everything [`search`](search.md) does: tests, docs and config rank
below code, duplicates are collapsed, and an identifier in the question puts its
definition first. If the BM25 index has not been built, it is not built for the
brief — hybrid degrades to vector plus anchoring. The `kind` and `include_tests`
filters apply here too.

The fetch is deliberately deeper than what will fit: *the budget decides where
to stop, not the limit*. A hit whose text a memory above already quotes is
skipped — not worth paying for twice.

### 3. Recorded against this code

For the first 5 files that passes 1 and 2 selected, the memories linked to those
files via the memory↔graph junction.

This is the pass that justifies the whole design. These are memories attached to
*exactly this code* — knowledge a semantic recall on the question's wording
would never have surfaced, because the memory and the question use different
words. It is what the junction exists for.

Each is labelled with its link provenance:

```
[memory · files-field · about crates/devctx-search/src/lib.rs] Rerank default
```

`files-field` and `content-mention` mean something connected the memory to this
code at write time. `inference` means only that the words match. The label is
there so the reader can weigh it.

Duplicates across files are dropped by memory id. Only memories that made it
into the brief contribute their `files`.

## Which repository

`build_context` takes an optional **`project`** — a registered name or any path
inside one — that builds the brief from that repository for this call only. In a
group session it must be a *member of the group*; anything else is rejected.

In a **group session without `project`**, the old behaviour (silently use the
group's default member) is gone. Instead the brief names where it came from:

```
[devctx] context from acme-api (best match 0.71; 3 of 5 members scored; not scored: acme-web (no running server), acme-docs (different model))
[devctx] ambiguous: also acme-worker (0.69) — within 0.03 of acme-api (0.71); pass `project` to choose
```

How a member is chosen:

1. Only members whose server is **already running** are scored: it reads each
   member's `serve.json`, checks `/health` (0.8 s) and runs a vector search
   (10 hits, no reranker) over HTTP. **It never starts a process.** A member
   with no server is `not scored (no running server)`; one on a different
   embedding model (even of the same width) or whose checkout is gone is
   `not scored` too, because cosines from two models are not comparable.
2. The score is the mean of the top 3 raw retriever scores of the member's
   *code* hits (all hits if the question matches no code, or `kind` is explicit).
   A member that *defines* an identifier named in the question gets a +0.03 tiebreak,
   never enough to overturn a clear lead.
3. The whole selection shares one **2.5 s deadline**; a member that does not
   answer in time is `busy`, not an error.
4. A lead under 0.03 still answers from the best member, plus the `ambiguous`
   line naming the others. If some comparable members could not be scored, the
   answer says `compared only k of N comparable members` and offers the ones the
   question names. If nobody could be scored and nothing failed (all cold), it
   answers from the group's default member and says it was *not chosen by
   relevance*. A member that answers with an error is an error, listed.
5. A choice that compared **every** comparable member with a clear lead is cached
   for **10 minutes**, per normalised question and `kind` / `include_tests`; its
   header says `cached choice`. Partial and ambiguous choices are never cached,
   so a member that has warmed up since gets its turn.

Without a group, a bound project answers as before; the `context from` line
appears only in a group session or when `project` is passed. `branch_fallback`
(see [Search](search.md#branch-awareness)) is reported as
`[devctx] branch_fallback: …`.

## The budget

`--max-tokens` (default 4096) is a **hard stop**, not a target. Tokens are
converted to a character budget with a fixed ratio, and every item is checked
against the remaining space before it is appended.

Two behaviours are worth knowing:

**Nothing is silently dropped.** Whatever did not fit is counted and named at
the end, once:

```
[devctx] omitted: 7 item(s), reason: budget (4096 tokens). Raise max_tokens, or narrow the query.
```

**A chunk that does not fit no longer ends the brief.** An item too big for the
space left is skipped and the next one is tried, so one large function does not
hide the smaller relevant ones behind it. Each code chunk is capped at the larger
of a third of the budget and 600 characters; a leading doc comment of more than 6
lines is cut to 3 plus `N doc lines trimmed`, kept well-formed in its language
(the `*/` stays); and a single enormous line is cut by characters with a marker
saying how much was left.

A brief that quietly truncated would read as "this is everything there is",
which is the one thing it must never mean.

**Headings ride with their first item.** A section header is emitted attached to
the first entry that fits, never on its own. A heading emitted separately can
survive a budget that its items did not, leaving an empty section — and an empty
section reads as "nothing here", which is exactly the wrong message when the
truth is "it didn't fit".

## Output shape

```
## What is already known

[memory] Reranking stays off by default
Measured 30 ms → 8.6 s and 406 MB → 2.4 GB...

## Code

// crates/devctx-search/src/lib.rs:55
pub fn search(...)

## Recorded against this code

[memory · files-field · about crates/devctx-search/src/lib.rs] Pool is the ceiling
A reranker reorders what it is handed and nothing else...
```

Sections with no content do not appear at all.

## Options

| Flag | Default | Effect |
|---|---|---|
| `--max-tokens` | 4096 | Hard ceiling for the whole brief |
| `--no-memories` | off | Code only — skips passes 1 and 3 |

The MCP tool also takes `project`, `kind` and `include_tests` (and
`include_memories` for `--no-memories`); the CLI does not.

`--no-memories` is for when you want raw retrieval without the opinion layer.

## Mental model

`search` answers *"where is the code?"*. `recall` answers *"what do we know?"*.
`build_context` answers the question an agent actually has, which is **"what
should I have read before answering this?"** — and answers it within a ceiling
you set, telling you honestly what it left out.
