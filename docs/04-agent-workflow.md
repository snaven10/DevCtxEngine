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

**The graph is binary per symbol.** Measured on a Java/Quarkus repository,
`crearNotificacion` returned 8 edges for 8 call sites while `actualizar` and
`cambiarEstado` returned zero despite real callers — and nothing says in advance
which group your symbol is in.

So: **edges are reliable; empty is not.** A clean impact report means "nothing
found", never "nothing there". Cross-check an empty one with
`search --keyword` before you touch anything.

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
