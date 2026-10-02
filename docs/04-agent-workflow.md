# Agent Workflow

> Back to [README](../README.md)
> 🇪🇸 [Leer en español](es/04-flujo-de-trabajo-del-agente.md)

How an agent should actually use these tools during a task — which one answers
which question, and in what order.

For memory specifically — when to save, what to put in it — see
[MEMORY-PROTOCOL.md](../MEMORY-PROTOCOL.md). For first-time setup, see
[AGENTS.md](../AGENTS.md).

---

## The problem this solves

An agent dropped into an unfamiliar repository has a context window and no idea
what is in it. The naive approach — read files until something looks relevant —
spends the window on retrieval and still misses the reasoning, because reasoning
is not in the code.

What the code cannot tell you: that the obvious approach was tried and abandoned,
that this function is load-bearing for a caller three modules away, that the
weird branch exists because of a production incident.

## Choosing a tool

| The question you have | The tool |
|---|---|
| Where is the code about X? | `search` |
| I know the name — show me the thing | `read_symbol` |
| What calls this? | `get_references` |
| What breaks if I change it? | `impact_analysis` |
| Why is it written this way? | `memories_by_symbol` / `memories_by_file` |
| What do we know about X? | `recall` |
| What should I read before answering? | `build_context` |
| Which HTTP route serves this? | `search_routes` / `routes_for_handler` |
| The answer is in another repository | `search_project` |
| I just lost my context | `memory_context` |
| What was I doing on this plan? What's next? | `plan_status` |

The two rows people skip are the expensive ones to skip: `impact_analysis`
before changing anything public, and `memories_by_symbol` before assuming code
is wrong.

## The loop

### Start with `build_context`

If you are about to do real work in an area you do not know, one call replaces
the first three or four:

```
build_context("how do we decide a token is expired", max_tokens=4096)
```

It returns, in one budgeted brief: what was already decided about this area, the
code that ranks highest, and the memories recorded against exactly those files.
That last part is the one manual retrieval never reaches — a memory whose words
do not match your question but whose *files* do.

It tells you what did not fit, so you know whether to raise the budget or narrow
the question.

### Then narrow

`build_context` orients. It does not replace reading. Once you know which
symbols matter:

```
read_symbol("verify_token")      → the definition
get_references("verify_token")   → every call site
impact_analysis("verify_token")  → the blast radius, transitively
```

**The graph is keyed by qualified name.** A bare name expands to every
`Class.method` carrying it, and the report says which declarations it merged —
in one Quarkus repository `actualizar` is twenty-one distinct methods. Ask with
the qualified name when you mean exactly one.

**Empty is still not proof.** A clean report means "nothing found", never
"nothing there": dynamic dispatch leaves no edge, only 7 languages produce edges
at all, and a stale index looks identical to absent code. Cross-check an empty
one with `search --keyword` before you touch anything.

### Before you decide the code is wrong

Run `memories_by_symbol` on it. The most expensive mistake an agent makes is
"fixing" a deliberate decision, and the call graph cannot warn you — only the
memory can.

Read `link_sources` on the result. `files-field` and `content-mention` mean
someone connected that memory to that code deliberately. `inference` means only
that the words matched, and deserves less weight.

### After you finish

Record what the next session will need. The bar is: **would someone re-derive
this, at cost, if it were not written down?**

Bug fixes with a root cause, decisions with reasoning, gotchas, conventions.
Not: what the diff already says.

Always pass `files`. A memory without it is findable only by text, which
requires already knowing what to ask. With it, the memory reaches anyone who
lands on that code. Details in
[MEMORY-PROTOCOL.md](../MEMORY-PROTOCOL.md).

## Working across repositories

`list_projects` shows what this machine tracks. `search_project` searches
another one by name without leaving your session — the backend question you hit
while editing the frontend.

If a lesson turns out to apply beyond one repository, `memory_move` promotes it
to `group` or `global` rather than making you rewrite it.

## When nothing is bound

A globally-registered MCP server starts in whatever directory the client was
launched from, which is frequently no repository at all. Tools then report that
no project is bound.

```
list_projects        → what exists
use_project <name>   → bind this session
```

This is a normal state, not a broken install.

## Plans, and the markdown format `plan_status` expects

A repository using `plans/PLAN-NNN[-slug]/` for its work plans gets
`plan_status` for free: no argument lists every plan and names the active one
(the one with pending work whose `.md` changed most recently; ties go to the
higher plan number); `plan: "PLAN-005"` (or `5`, or the directory name) gives
that plan's ready/in-progress/blocked tasks. Call it right after a compaction —
it is what the recovered context is missing.

`plan_status` reads `plans/PLAN-NNN[-slug]/tasks/TASK-NNN-*.md` files. The
header block (between the H1 and the first `## ` section) needs at minimum:

```markdown
# TASK-003 — a short title

- **Plan:** PLAN-005 — plan title
- **Depende de:** TASK-001, TASK-002
- **Estado:** `pending`
```

`Estado` takes one of five values, written the way you like; the parser
also accepts these synonyms (any case, accents ignored, English or Spanish):

| Status | Recognized words |
|---|---|
| `pending` | `pending`, `pendiente`, `todo` |
| `in_progress` | `in_progress`, `in-progress`, `en progreso`, `en curso`, `in progress`, `wip` |
| `done` | `done`, `completed`, `hecho`/`hecha`, `terminada`, `cerrada`, `implemented`, `ejecutada`, `merged`, or a bare `✅` |
| `blocked` | `blocked`, `bloqueada`/`bloqueado` |
| `skipped` | `skipped`, `omitida`, `descartada`, `cancelled`/`cancelada`, `superseded`, `postponed`/`pospuesta` |

`skipped` is for a task dropped on purpose: it resolves the tasks that depend
on it (like `done`) but is counted apart — the CLI shows `done/total (+N
skipped)` — because it was not done. The value may carry emoji, bold and
trailing prose (`✅ **`done`** — ran on the branch`): a backtick span that is
exactly a keyword wins, otherwise the first keyword in the text. A value with
no keyword stays unknown and produces a warning; the parser never guesses.
`` `pending` `` in backticks stays the recommended form.

Where the status is looked for, in order:

1. a header field — `Estado`, `Status` or `State`, in any of `- **Estado:** x`,
   `**Estado**: x`, `**Estado: x**` or plain `Estado: x`; several fields may
   share a line separated by ` · ` (`**Plan:** PLAN-005 · **Estado:** pending
   · **Depende de:** —`);
2. YAML front matter (`---` … `---` at the top of the file) with `estado:`,
   `status:` or `state:`;
3. a `## Estado` (or `## Status`) section: its first non-empty line.

The name must match exactly, so `Estado final` or `Estado objetivo` are not
the status.

`Depende de` (also `Depends on`, `Dependencias`, `Dependencies`, `Bloqueada
por`, `Blocked by`; header or front matter) only counts `TASK-<id>` tokens:
the prose around them is ignored, and ranges (`TASK-001..004`) and lists
(`TASK-033/034`) are expanded. A leading `—`, `Ninguna`, `None`, `N/A` or `[]`
means none, even if the text after it mentions a task (`— (parallel to
TASK-001)`). A reference to another plan's task (`PLAN-120/TASK-031`) is kept
as an external dependency, reported but never blocking. Because ids are
picked out of the text, a task id mentioned in a sentence in this field does
become an edge — keep the field to the dependencies themselves.

Ids keep a letter suffix or prefix as written: `TASK-001b` is not `TASK-001`,
and `TASK-R01` is its own task. Two files with the same id (a companion
`TASK-001-DESIGN.md` next to `TASK-001-verify.md`) produce a warning and only
the first, in sorted order, is the task. Anything else the parser cannot place
becomes a warning, never a guess.

A plan's own progress table (in the plan's `.md`, not the task file) is read
only to check it agrees with the task files, and only a table with an
`Estado`/`Status` column counts (a rollback table with ids in the first column
is ignored). The plan doc itself is the `PLAN-`-prefixed file, preferring
`<dir-name>.md`. **The task file always wins** —
if a plan's table says `done` and the task file says `pending`, the task is
reported `pending`, plus a warning naming the disagreement. Never edit a task
file to make a warning go away without checking which one is actually true.

### Where `plans/` is read from

The output of `plan_status` carries `plans_root: {path, source}`. The root is
resolved in this order:

1. **workspace** — the workspace root (where an MCP group descent started, or
   the directory you launched from when it is no project), if it has `plans/`;
2. **project** — the project's own root, if it has `plans/`;
3. **ancestor** — only when the project declares `group:`: the nearest
   ancestor directory with `plans/PLAN-*`, stopping below `$HOME`;
4. otherwise `source` is `none` and the plan list is empty.

The use case: a product whose repositories are separate devctx projects under
a directory that is not itself a project, with the plans kept in
`<workspace>/plans/` — they belong to the product, not to any one repo. In
that layout the MCP in group mode (or unbound) reads `<workspace>/plans`;
`devctx web` run from the workspace root works without a `.devctx/` there
(same group descent as `devctx mcp`); and the CLI from inside a member repo
that declares `group:` finds the workspace `plans/` as the ancestor. A running
daemon keeps serving the root it resolved at start: after changing this, run
`devctx serve --stop`.

## When the index is stale

`index_status` reports the last-indexed commit and whether the index is current.
If it is behind, `index_repo` catches it up incrementally — only what changed.

The index mirrors the **work tree**, not the last commit, so uncommitted code is
searchable. Anything git ignores is not, which is the usual reason a file you
can see is not findable.

## Anti-patterns

**Reading files to find things.** That is what `search` is for. Reading is for
after you know which file.

**Skipping `impact_analysis` because the change looks small.** Size of diff has
no relationship to size of blast radius.

**Trusting a search that returned nothing.** Check `index_status` first — an
empty result from a stale or unbuilt index looks identical to an empty result
from code that does not exist.

**Saving a memory without `files`.** It costs one field and determines whether
the memory is ever found again from the code.

**Assuming reranking would help.** It is off by default because it was measured:
two orders of magnitude slower, and the one model benchmarked across the suite
made results worse. See [ADR-15](08-design-decisions.md).
