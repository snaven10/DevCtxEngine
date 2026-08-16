# Proposal: make `devctx init` a conversation

**Status:** draft, for review
**Date:** 2026-08-16

## The problem

`devctx init` writes a config and prints two lines. Every decision in that file
is made for the user, silently, and two of them are expensive to undo:

- **The embedding model** fixes the width of every vector. Changing it later
  means re-indexing every repository and re-embedding every memory. Picking an
  English model for a Spanish codebase produces no error — just poor results,
  months later.
- **Where the memories go** — this repository, the product's group, or the
  global space — decides who can recall them. Nothing asks, so everything
  lands wherever the default points.

The current release added a model prompt, but it is one question with no
context: it does not say what the other repositories on this machine already
use, and it asks nothing about storage or groups.

## What exists today (verified, not assumed)

| | today |
|---|---|
| Central registry | caches each repo's `name, path, config_path, db_path, embed_provider, embed_model, embed_dim` — so the wizard can report other repos without opening them |
| Group memories | a reserved key `@group:<name>` **inside** `central.duckdb`. There is no separate group database |
| Where the index lives | `state_dir` / `storage.db_path` in the project config; empty means `<repo>/.devctx/state/` |
| `init` defaults | inherits `embeddings` and `reranking` from the central config; everything else built-in |

Two of those built-in defaults are wrong for anyone who has measured them, and
this proposal fixes them regardless of the wizard:

- `storage.hnsw: false` — measured 84 ms → 49 ms with it on, recall@10 100%.
  A new repository should not start with the slow path.
- `storage.metric: ''` — should read `cosine`. It behaves correctly (unknown
  values normalize to cosine) but teaches the reader nothing.

## Proposed behaviour

`devctx init` on a terminal walks through four questions. Each shows what
already exists on the machine, because the right answer is usually "the same as
the others".

### 1. Embedding model

```
This machine already uses:
  ml-granite  (384d, multilingual)   REVFA_BackEnd, REVFA_FrontEnd, +2 others
                                     and the shared memory space

MODEL            DIMS  LANGUAGES      FILES      NOTES
*ml-granite       384  multilingual   ready      best multilingual on CPU
 minilm-l6        384  English        automatic  smallest and fastest
 ...

Model [ml-granite]:
```

The existing usage comes from the registry, counted by model. Choosing a model
that needs files downloads them right there. Choosing one that differs from the
shared memory space prints why that is allowed but rarely wanted: the code index
and the memories then live in different spaces, which is fine — they are
searched separately — but a second model costs a second few hundred megabytes
of RAM in any process that touches both.

### 2. Where this repository's index lives

```
Where should this project's index be stored?
  1) inside the repository        <repo>/.devctx/state/     (default)
  2) a directory you name         e.g. an external disk

Index location [1]:
```

The index is a build artefact — large, binary, rebuilt from the repository —
so inside the repo is right by default, and `.devctx/.gitignore` already
excludes it. Naming a directory exists for the case that motivates it: a
repository on a small disk, or several worktrees of one repo that should not
each carry a copy.

**Not offered: putting a project's code index in the central store.** One
DuckDB file allows a single writing process; funnelling every repository's
indexing through it would serialize them and make one re-index block the rest.
Per-repository indexes are the reason a re-index never blocks another project.

### 3. Group

```
Memories can be shared between the repositories of one product.
Groups on this machine: REVFA (4 repositories)

Group for this repository [none]:
```

Answering `REVFA` puts it in the existing group. Answering a new name creates
one — which costs nothing, because a group is a key in the central store, not a
database.

**This is where the proposal disagrees with the request.** A separate database
per group was asked for; it should not be built:

- The central store is already the single writer for shared memory. A second
  file per group multiplies the daemons, the locks and the failure modes, for a
  table that holds thousands of rows, not millions.
- Recall fuses local + group + global in one call. Splitting the group into its
  own file means opening two databases per recall instead of one.
- Isolation is what a group is *for*, and the key already gives it: a
  `@group:REVFA` memory is invisible to any repository outside the group. A
  separate file adds no isolation that the key does not already provide.

What a separate file would genuinely buy is *portability* — handing one group's
memories to a colleague without the rest. That is an export, not a storage
layout, and it is specified in §5.

### 4. Confirmation

```
  project     demo
  group       REVFA          → memories shared with 4 repositories
  model       ml-granite     → 384d, same space as the shared memories
  index       ./.devctx/state/index.duckdb   (HNSW on)
  memories    local → here · group → central store · global → central store

Write this? [Y/n]
```

The line that matters is the last one: it is the only place the three tiers are
explained at the moment someone is deciding between them.

## 5. Export and import memories

Portability is the real want behind "a database per group": handing someone one
product's memories without the rest, moving to another machine, keeping a
backup that is not a binary blob. It is answered by moving memories, not by
splitting where they live.

```bash
devctx memories export --scope local            > project.jsonl
devctx memories export --scope group            > revfa.jsonl
devctx memories export --scope global           > global.jsonl
devctx memories export --scope group --repo REVFA_BackEnd   # only what one repo contributed

devctx memories import revfa.jsonl              # into the scope each memory declares
devctx memories import revfa.jsonl --scope local   # override: land them all here
devctx memories import revfa.jsonl --dry-run
```

**Format: JSONL, one memory per line.** Not a DuckDB file, and the reason is
the whole point of exporting: a database file is only readable by the version
that wrote it, which makes it useless for the case where it matters — a
colleague on a different release. JSONL is greppable, diffable, streams without
loading the lot into memory, and can be fixed by hand when something is wrong
with one line.

Each line carries the memory's fields plus its embedding:

```json
{"id":"mem_…","title":"…","content":"…","type":"decision","scope":"group",
 "project":"@group:REVFA","tags":"…","repo":"REVFA_BackEnd","created_at":"…",
 "embedding":{"model":"ml-granite","dim":384,"vector":[…]}}
```

**Vectors travel, but are trusted only on an exact match.** Import reuses the
embedding when the destination's model *and* dimension are identical, and
re-embeds otherwise. Both halves of that were measured today rather than
assumed:

- Re-embedding 2090 memories took **46 minutes**. Discarding usable vectors
  would make every import pay that, including the common case of moving between
  two machines set up the same way.
- Vectors from a *different* implementation of the same nominal model scored
  **0.76–0.87 cosine against their own re-embedding**, where identical would be
  1.00. Carrying those across would rank everything wrongly, silently. Hence
  the match must be exact, and the model name alone is not enough — the import
  reports which path it took, per file:
  `imported 2090 memories (reused embeddings: ml-granite/384)` or
  `… re-embedded with minilm-l6/384 (exported as ml-granite/384)`.

### Import only ever adds

Import never overwrites and never deletes. It is the one operation whose input
comes from somewhere else, so the rule is that nothing already here can be lost
by running it — including running it with the wrong file.

That is **not** what `remember` does, and the difference matters. `remember`
with a topic key that already exists *revises* it: the content is replaced and
`revision_count` goes up. Correct when you are amending your own note; wrong
when the incoming text came from another machine, where it would silently
destroy a local memory that the sender never saw.

Three cases, decided by comparing before writing:

| the incoming memory | what happens |
|---|---|
| identical content to one already here (same normalized hash) | skipped — it is already there |
| not here at all | added |
| same topic key, **different** content | added as its own memory, with the topic key dropped, and reported |

The third row is the one that earns the rule. Dropping the topic key on the
incoming copy leaves the existing memory owning its topic, so nothing is
overwritten, and keeps the incoming text, so nothing is discarded. Both survive
and the summary names them, because a topic collision between two machines is
usually two people having learned different things about one subject — which is
worth reading, not resolving automatically.

```
imported 41 memories · 12 already present · 3 topic collisions kept separately:
    · "auth approach"        (topic: auth-approach)
    · "PDF template lookup"  (topic: pdf-templates)
    · "NUI format"           (topic: nui-format)
```

`--dry-run` prints exactly that table without writing, so the outcome is
readable before it happens.

**`--scope` on export selects, on import overrides.** Exporting a group and
importing it without a scope puts each memory back in the tier it declares;
with `--scope local` the whole file lands in the current project, which is what
someone wants when adopting another team's notes without publishing them to
their own group.

## What stays non-interactive

Every question is skipped, silently, when stdin is not a terminal — a script,
a CI job, an agent. Those callers get today's behaviour: central defaults, no
prompts, no blocking. Flags override individually (`--model`, `--group`,
`--state-dir`, `--yes`), so an agent can be explicit without a terminal.

This is not a nicety. An agent following `AGENTS.md` runs `devctx init` without
a TTY, and a prompt it cannot answer would hang the setup it was told to
perform.

## Out of scope

- Changing where memories are stored, beyond what the group key already does.
- A group-scoped database (see §3) — export answers the need behind it (§5).
- Exporting the *code* index. It is derived from the repository: whoever
  receives it has the repository, and `devctx index` rebuilds it correctly for
  whatever model they use. Memories are the only thing that cannot be
  regenerated.
- Reconfiguring an existing project. `init` refuses when a config exists, and
  that stays: editing `.devctx/config.yaml` is the way, and a wizard that
  rewrote it would have to reason about an index already built with the old
  model.

## Risks

- **A prompt in the wrong place hangs something.** Mitigated by the TTY check,
  but it has to be tested by actually running `init` with stdin closed, not by
  reasoning that the check is there.
- **The registry may be stale.** It caches each project's model; if someone
  edits a config by hand, the wizard reports what was registered, not what is
  on disk. `projects refresh` exists for that, and the summary should say the
  figure comes from the registry.
- **More questions is more to get wrong.** Every question defaults to what the
  machine already does, so pressing Enter four times reproduces the current
  behaviour exactly.
