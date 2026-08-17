# Memory protocol

How a coding agent should use DevCtxEngine's memory. Written to be handed to an
agent verbatim — point yours at this file and it has the whole contract.

The tools are described by their own schemas; what a schema cannot say is *when*
to reach for them, and that is what goes wrong. An agent with `remember`
available and no policy either never writes anything, or writes a diary nobody
can search. Both end with the next session re-deriving what the last one already
knew.

---

## The one rule

**Save what the code cannot tell you. Search before you re-derive.**

Everything below is that rule made specific.

---

## When to search

Call `recall` **before starting work**, not after getting stuck. The cost of a
recall that finds nothing is one tool call; the cost of skipping one that would
have found something is the whole investigation, repeated.

Reach for it when:

- The task names a feature, bug, module or system by name.
- You are about to investigate *why* something behaves the way it does.
- Something looks wrong, arbitrary, or like a bug — it may be a decision.
- You are resuming work, or the conversation has been compacted.

There are four ways in, and they answer different questions:

| Tool | The question it answers |
|---|---|
| `recall` | "What do we know about this topic?" — semantic, by wording |
| `memories_by_symbol` | "What was decided about *this function*?" |
| `memories_by_file` | "What was decided about *this file*?" |
| `memory_context` | "What has been going on here?" — recent, no query |

`recall` is the general one. The other three matter because **a memory you
cannot name is a memory you cannot recall.** After a context reset you do not
yet have the words; you have a file you are editing and a function that looks
strange. `memories_by_symbol` turns the code itself into the query.

Use `memory_context` when you have no query at all — after a reset, or when
picking up someone else's work.

### Reading the result

Every memory comes back with **`link_sources`**, and it is not decoration:

- `files-field`, `content-mention` — a structural link, recorded when the memory
  was written. Something connected this memory to this code deliberately.
- `inference` — the junction had nothing and the text merely mentions the name.
  Weaker. Read it, but confirm before acting on it.

`matched_by: junction` means the whole answer is structural.
`matched_by: text-inference` means none of it is.

---

## When to save

Save **after** the work, when you know what was true — not while guessing.

Save when any of these just happened:

- **A decision.** An approach chosen over an alternative, and why the other lost.
- **A bug fixed**, with the root cause. Not "fixed the 500" — *what* was broken.
- **A discovery.** Something the code does that reading it would not reveal:
  a gotcha, an ordering constraint, a service that lies about its own status.
- **A convention** established, or a config that has to be a specific value.
- **A dead end.** What you tried that did not work, so the next session does not
  spend the same hour on it. This is the one most often skipped and most worth
  keeping.

Do **not** save:

- What the code already says. A summary of a function is not a memory.
- What git already says. "Renamed X to Y" is a commit message.
- Anything that matters only inside this conversation.
- Secrets, tokens, credentials, personal data.

If you cannot say what *breaks* for the next reader who does not know it, it is
not worth saving.

---

## How to save

### Always fill in `files`

`remember` takes a comma-separated `files`. **This is the field that makes the
memory findable from the code**, so filling it in is not optional politeness —
it is the difference between a memory that surfaces when someone lands on the
function and one that only surfaces if they already knew to ask.

DevCtxEngine reads those files, finds the symbols in them, and links the memory
to every symbol your text actually names. A memory saved without `files` has no
structural links at all and can only ever be reached by text.

So: **name the files, and name the symbols in the prose.** Write
`separarApellidos` rather than "the splitting method". The prose is what the
linker matches against.

### Use a `topic` for anything that will change

`topic` is an upsert key. Saving again with the same topic **revises** the
memory instead of adding a near-duplicate beside it. Use one for anything with a
lifetime: a feature's state, a system's known behaviour, an ongoing
investigation.

Omit it for one-off observations that will never be updated.

### Pick the right `scope`

| Scope | Where it goes | Use for |
|---|---|---|
| `local` | this repository | anything specific to this codebase |
| `group` | every repository of this product | contracts, shared conventions, cross-repo behaviour |
| `global` | every project on this machine | tooling, environment, how *you* work |

The default is `local`, and it is usually right. Reach for `group` when a second
repository in the same product would need to know. Reach for `global` only when
the fact has nothing to do with any particular product.

Getting this wrong is recoverable but annoying: a memory in the wrong tier is
invisible where it is needed, or noise where it is not.

### Write it so it survives

A memory is read months later by someone — possibly you — with no memory of the
conversation that produced it. So:

- **Convert relative dates.** "Last week" is meaningless later; write the date.
- **Name things fully.** Not "the endpoint" — the path and the method.
- **State the why, not just the what.** The what is recoverable from the code.
- **Include the failure mode.** What went wrong, what it looked like, and what
  made it obvious.

A useful shape, though the tool imposes none:

```
What:  <the fact, one or two sentences>
Why:   <the reason, or the root cause>
How to apply: <what a future reader should do differently>
```

---

## Building context

`build_context` assembles one budgeted brief for a question: what is already
known, then the code that ranks highest, then the memories recorded against
exactly those files. Use it when you are starting on unfamiliar ground and want
one call instead of four.

It returns prose, capped at `max_tokens`, and **names what did not fit** rather
than dropping it silently. If it says items were dropped, either raise the
budget or narrow the query — do not assume you saw everything.

---

## The failure this protocol exists to prevent

An agent that indexes a repository, works for an hour, solves something hard,
and saves nothing. Next session it hits the same wall, and the reason it was a
wall is in nobody's context.

The second failure is subtler and worse: an agent that saves diligently but
never fills in `files`, so nothing is linked, and every future lookup falls back
to text matching on wording that nobody will guess. The memories exist and are
unreachable, which looks exactly like having none.
