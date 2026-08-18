# Memory

> 🇪🇸 [Leer en español](../es/03-conceptos-fundamentales/memoria.md)

Knowledge that outlives the session that produced it — and that can be found
again from the code it is about.

---

## What it is

A memory is a short, typed, scoped note: a decision and its reasoning, a bug's
root cause, a gotcha that cost someone an afternoon.

```bash
devctx remember "Reranking stays off by default: measured 30ms → 8.6s" \
  --type decision \
  --topic search-rerank-default \
  --files crates/devctx-core/src/config.rs

devctx recall "why is reranking off"
```

Agents use the `remember` and `recall` MCP tools, plus `memories_by_symbol`,
`memories_by_file`, `memory_refs`, `memory_context`, `memory_stats`,
`memory_forget` and `memory_move`.

## Why it exists

Chat history is a whiteboard: useful during the meeting, erased afterwards. Code
comments are sticky notes — they annotate one spot but cannot hold a decision
that spans a system. Neither survives into the next session, so the same
question gets re-answered, and sometimes re-answered *differently*.

Memory is the notebook: searchable by meaning, deduplicated, and — the part that
matters most — reachable from the code.

## The three tiers

| Scope | Stored under | Visible from |
|---|---|---|
| `local` | The project's own store | This repository only |
| `group` | Central store, `@group:<name>` | Every repository sharing `project.group` |
| `global` | Central store, `@global` | Every project on the machine |

`--scope all` (the default for `recall`) searches every tier that applies and
fuses the results by rank.

### Why group and global rows are re-keyed

A memory's identity is derived from its `project` plus its content hash. If a
global row kept the project that contributed it, the *same* lesson learned in
two repositories would land as two rows — deduplication failing exactly where it
matters most.

So global rows all carry the reserved project `@global`, and group rows carry
`@group:<name>`. The contributing repository stays in the `repo` field as
provenance. Group keying keeps each product's shared knowledge in its own space:
dedup still collapses the same lesson from two sibling repositories, without
leaking it to unrelated projects the way `@global` would.

## Deduplication

Writing happens through one path, and it either inserts or revises:

- **With `--topic`** — upsert by topic key. Saving again under
  `search-rerank-default` revises that memory instead of adding a second one.
  This is how a memory stays current rather than accumulating contradictory
  versions.
- **Without `--topic`** — identity falls back to a content hash over normalized
  text (lowercased, whitespace collapsed). Saving the same thing twice is a
  no-op.

Use a topic key for anything you expect to revise. Use bare content for
one-off observations.

## The memory↔graph junction

This is the part that distinguishes memory here from a searchable notes file.

**Pass `--files`.** It is the single highest-leverage field:

```bash
devctx remember "..." --files crates/devctx-search/src/lib.rs
```

With it, the memory becomes findable from every symbol in those files —
`memories_by_symbol` answers *"what was decided about `search()`?"* before you
have the words to phrase a `recall`. Without it, the memory is findable only by
text, which requires already knowing what to ask.

### Where the junction row lives

The call graph is per-repository and lives in the project store. A global or
group memory lives in the central one. A memory about this repository's
`charge()` must be findable from `charge()` regardless of which store holds its
text.

So the junction row always goes in the **project** store — next to the graph it
points into — carrying only the memory's id. Resolving that id looks locally
first and falls back to the central store. Copying memory text into every
project that mentions it would mean an edit in one place leaves stale copies
everywhere else.

### Link provenance

Every result carries `link_sources`, and the distinction is load-bearing:

| Value | Meaning |
|---|---|
| `files-field` | The memory's `files` named this file. Structural. |
| `content-mention` | The memory's prose named this file, and the file is indexed. Structural. |
| `inference` | Only the words match. Weaker. |

The first two mean something connected this memory to this code at write time.
`inference` means the text happens to line up. A caller weighing whether to
trust a link should read this field.

### Backfilling old memories

Memories written before the junction existed — migrated, imported, or saved by
an older build — carry no links:

```bash
devctx memories backfill-links --dry-run
devctx memories backfill-links
```

There is a text-derived pass for memories with no `files` at all, which is
roughly half of a real corpus. It builds a *candidate* list from file paths
named in the prose and over-matches by design: the same pattern that finds
`apps/registry/src/app/components/firmar-registro.ts` also finds `Shepherd.js`,
which is a library nobody indexed, and `CLAUDE.md`, which is not code. Every
candidate is checked against the index before a link is written. **The index is
what tells them apart, never the pattern.**

## Recall

```bash
devctx recall "why is reranking off" --limit 5 --scope all
```

Retrieval fetches a deeper pool than the limit (`limit × 8`, minimum 40) from
each applicable tier, then fuses the ranked lists by rank and deduplicates by
memory id. `--repo <name>` narrows global results to one contributing
repository.

## Managing memories

```bash
devctx memory-stats                       # counts for this project
devctx memory-forget <id>                 # delete one, wherever it lives
devctx memories export > memories.jsonl   # one JSON object per line
devctx memories import memories.jsonl     # only ever adds, never overwrites
devctx memory-purge <project-key>         # delete every memory under one key
```

`memory_move` (MCP) promotes a memory between tiers — a lesson that turns out to
apply beyond one repository moves to `group` or `global` without being rewritten.

Deleting matters as much as writing. A memory recording a root cause that turned
out to be wrong is worse than no memory, because it will be recalled with
confidence.

## Mental model

Three questions, three tools:

- *"What do we know about X?"* → `recall`. Needs you to have the words.
- *"What was decided about **this** function?"* → `memories_by_symbol`. Works
  when you are standing on the code and don't have the words yet.
- *"What should I know before answering this question?"* → `build_context`,
  which assembles code and memories into one budgeted brief.

The second one is why `--files` is not optional in practice.
