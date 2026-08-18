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

The files these memories name are recorded, and pass 2 uses them.

### 2. Code

A vector search for the question, fetching 30 hits.

**Files a memory already pulled in are skipped** — not worth paying for twice.
The fetch is deliberately deeper than what will fit: *the budget decides where
to stop, not the limit*.

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

Duplicates across files are dropped by memory id.

## The budget

`--max-tokens` (default 4096) is a **hard stop**, not a target. Tokens are
converted to a character budget with a fixed ratio, and every item is checked
against the remaining space before it is appended.

Two behaviours are worth knowing:

**Nothing is silently dropped.** Whatever did not fit is counted and named at
the end:

```
[devctx] 7 further item(s) did not fit in 4096 tokens.
Raise max_tokens, or narrow the query.
```

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

`--no-memories` is for when you want raw retrieval without the opinion layer.

## Mental model

`search` answers *"where is the code?"*. `recall` answers *"what do we know?"*.
`build_context` answers the question an agent actually has, which is **"what
should I have read before answering this?"** — and answers it within a ceiling
you set, telling you honestly what it left out.
