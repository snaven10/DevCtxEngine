# Example: Refactoring Safely

> 🇪🇸 [Leer en español](../es/05-ejemplos/refactorizacion.md)

**Scenario:** `UserService.getUser()` returns a full user object everywhere, and
three endpoints only need the id and email. You want to split it. It is called
from places you have not read.

---

## The rule

**Run `impact_analysis` before you change any public symbol.** Not after, not
"if it looks risky". Before.

The size of a diff has no relationship to the size of its blast radius. A
one-line signature change can reach forty call sites; a two-hundred-line rewrite
of a private helper can reach none.

## Step 1 — Map the blast radius

```
impact_analysis("getUser")
```

Two directions:

- **Callers (upstream)** — everything that could break. This is the work.
- **Callees (downstream)** — what `getUser` depends on. This is what constrains
  how you can split it.

### Edges are reliable. Empty is not.

**Coverage is binary per symbol.** Measured on a Java/Quarkus repository,
`crearNotificacion` returned 8 edges for its 8 call sites — complete — while
`actualizar` and `cambiarEstado` returned **zero** despite real callers. Nothing
says in advance which group your symbol is in.

| Limit | Consequence for you |
|---|---|
| Coverage is binary per symbol | A symbol may have every edge, or none at all. **The empty case is silent.** |
| Dynamic dispatch leaves no edge | Callbacks, reflection and string-keyed registries are **invisible**. |
| Only 7 languages produce edges | In a polyglot repository, part of it is not in the graph. |

So a report **with** edges you can act on. A **clean** one proves nothing —
cross-check with a keyword search before you trust it:

```bash
devctx search "getUser" --keyword
```

Keyword search finds the string in files the graph never parsed — templates,
config, another language. This is exactly the case BM25 exists for.

## Step 2 — Confirm each call site

```
get_references("getUser")
```

Gives you file and line for every call. Now read them. `impact_analysis` tells
you *how far* the change reaches; `get_references` tells you *what to look at*.

## Step 3 — Find out why it is like this

Before you improve the design, check whether the design is deliberate:

```
memories_by_symbol("getUser")
```

This is the step that prevents the most expensive class of refactor: undoing a
decision someone made for a reason that is no longer visible. "Returns the full
object because the ORM lazy-loads and three partial queries were slower than one
full one" is exactly the kind of thing that lives in a memory and nowhere else.

Weight results by `link_sources`: `files-field` and `content-mention` are
deliberate connections, `inference` is a word match.

## Step 4 — Check the surface

If it is reachable over HTTP, the blast radius includes clients you cannot see:

```
routes_for_handler("getUser")
```

An internal refactor that changes a response shape is not internal.

## Step 5 — Refactor

Now you can work. You know every caller, why the current shape exists, and
whether the change is externally visible.

## Step 6 — Verify against the new index

Re-index, then confirm the old symbol is genuinely gone:

```bash
devctx index
devctx search "getUser" --keyword
```

The index mirrors the work tree, so this reflects your uncommitted change
immediately. A lingering hit is a call site you missed — usually in a file the
graph never parsed, which is why this check is keyword rather than semantic.

## Step 7 — Record the decision

```bash
devctx remember "Split getUser into getUser and getUserSummary. The full object
was being loaded for three endpoints that only needed id and email, and the ORM's
lazy loading made that a second query per field access. Kept getUser rather than
changing its shape because two external clients depend on the response." \
  --type decision \
  --topic user-service-getuser-split \
  --files src/services/user.rs,src/api/handlers/user.rs
```

Record the **rejected alternative** — "kept getUser rather than changing its
shape, because two external clients depend on it". Six months from now, someone
will look at the redundant-seeming pair and want to merge it back. That sentence
is what stops them.

## The flow

```
impact_analysis          → how far does this reach?
search --keyword         → what the graph could not see
get_references           → the exact call sites
memories_by_symbol       → is the current design deliberate?
routes_for_handler       → is it externally visible?
[refactor]
index && search --keyword → did I miss anything?
remember --files --topic  → the decision and the rejected alternative
```

## What goes wrong without this

**Missing a caller in a language the graph does not parse.** Compiles fine,
breaks at runtime, and the impact report said the change was clean.

**Undoing a deliberate decision.** The code looked redundant. It was load-bearing
for a reason nobody wrote down — until now.

**Changing a public response shape.** No internal caller broke, so it looked
safe. The clients were not in the repository.
