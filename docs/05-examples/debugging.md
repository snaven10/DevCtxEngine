# Example: Debugging in a Large Codebase

> 🇪🇸 [Leer en español](../es/05-ejemplos/depuracion.md)

A worked flow for a bug in code you did not write.

**Scenario:** payments are occasionally double-charged. You have a ticket, a
stack trace fragment mentioning `processPayment`, and no familiarity with the
module.

---

## Step 0 — Check the index is current

```bash
devctx index_status
```

A search over a stale index returns "nothing found", which looks exactly like
"this code does not exist". Rule this out first — it is thirty seconds and it
saves an hour.

## Step 1 — Ask what is already known

Before reading any code:

```
recall("double charge payment idempotency")
```

If someone has hit this before, the answer is here and you are done in one call.
If not, you have learned the area is undocumented, which is itself useful — it
means you will be the one writing the memory at the end.

## Step 2 — Orient with one budgeted brief

```
build_context("where is a payment charged and how is a retry handled")
```

This returns three things in one artifact: recalled memories about the area, the
highest-ranking code, and — the part manual searching never reaches — memories
recorded against exactly the files that came back.

That third section is where "we deliberately made retries non-idempotent because
the gateway deduplicates" would surface, even though nothing in your query used
those words.

## Step 3 — Read the actual definition

Once you know the symbol name:

```
read_symbol("processPayment")
```

Returns the definition, its file, line range, kind, and code. Note that it
returns **all** definitions matching the name — if two exist, that ambiguity is
often the bug.

Use `read_symbol` when you know the name. Use `search` when you know only what
the code *does*.

## Step 4 — Find every caller

```
get_references("processPayment")
```

Every call site across the indexed code. For a double-charge bug this is the
highest-value single call: two callers where you expected one is a complete
explanation.

## Step 5 — Check the blast radius before you touch anything

```
impact_analysis("processPayment")
```

Transitive callers and callees. Callers tell you what a change could break;
callees tell you what this depends on to work at all.

**Edges are reliable; empty is not.** Coverage is binary per symbol — measured,
some symbols carry every edge and others carry none despite real callers, with
nothing to tell them apart. A clean report means "nothing found", never
"nothing there".

## Step 6 — Before concluding the code is wrong

```
memories_by_symbol("processPayment")
```

The most expensive mistake available to you right now is "fixing" a deliberate
decision. The call graph cannot warn you about that. Only this can.

Check `link_sources` on each result:

- `files-field` / `content-mention` — someone connected this memory to this code
  on purpose. Weight it.
- `inference` — the words happened to match. Weight it less.

## Step 7 — Record the finding

The fix goes in the diff. The *reason* does not, and that is what the next
person needs.

```bash
devctx remember "Double charge came from the retry wrapper calling processPayment
after the gateway had already committed. The gateway is idempotent by request id,
but the wrapper generated a fresh id per attempt." \
  --type bug \
  --topic payments-double-charge \
  --files src/payments/processor.rs,src/payments/retry.rs
```

Three things make this memory useful rather than decorative:

- **`--files`** — this is what makes it reachable from `processPayment` later,
  via `memories_by_symbol`. Without it, only a text search finds it, and only if
  you guess the wording.
- **`--topic`** — if the understanding is revised, the revision replaces this
  entry rather than contradicting it.
- **The root cause, not the symptom.** "Fixed double charge" is worthless. The
  sentence about the fresh id per attempt is the whole value.

## The whole flow

```
index_status                      → is the index current?
recall                            → has anyone solved this?
build_context                     → orient: known + code + linked memories
read_symbol                       → the definition itself
get_references                    → who calls it
impact_analysis                   → what a change would reach
memories_by_symbol                → why it is written this way
remember --files --topic          → what the next person needs
```

## What this bought you

The naive version of this task is: grep for `processPayment`, open four files,
read until something looks wrong, guess.

The difference is not speed. It is that steps 1 and 6 surface reasoning that
does not exist anywhere in the code — and that step 7 means the next person does
not repeat any of it.
