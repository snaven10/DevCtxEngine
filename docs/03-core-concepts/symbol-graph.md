# Symbol Graph

> 🇪🇸 [Leer en español](../es/03-conceptos-fundamentales/grafo-de-simbolos.md)

A call graph over the indexed code: who calls what, and what a change would
reach.

---

## What it is

While indexing, tree-sitter parses each supported file and extracts two things:
the symbols it declares, and the calls it makes. The calls become edges.

```bash
devctx symbol authenticate          # the definition and its code
devctx impact authenticate          # transitive callers and callees
```

Agents use `read_symbol`, `get_references` and `impact_analysis`.

## Why it exists

Semantic search answers *"where is the code about X?"*. It cannot answer *"what
breaks if I change this?"*, because that question is about structure, not
meaning — and the answer includes code that never mentions X.

The graph answers the structural question without ranking: an edge either
exists or it does not. But **its absence proves nothing** — see the limits
below, because that distinction is what stands between you and a broken
refactor.

## What is actually in the graph

**One edge kind: `calls`.**

This is worth stating plainly, because it is easy to assume otherwise. The
parser also extracts imports and type bindings, but those are used to *resolve*
call targets — they are not stored as edges. There are no `inherits`,
`implements` or `references` edges.

So: this is a call graph. Not a dependency graph, not a type hierarchy.

Symbol kinds recognised by the queries:

`function` · `method` · `class` · `struct` · `enum` · `interface` · `type`

## Supported languages

**Full parsing — symbols and call edges (7):**

| Language | Extensions |
|---|---|
| Python | `.py` `.pyi` |
| JavaScript | `.js` `.mjs` `.cjs` `.jsx` |
| TypeScript | `.ts` `.mts` `.cts` |
| TSX | `.tsx` |
| Go | `.go` |
| Java | `.java` |
| Rust | `.rs` |

**Indexed as raw text — searchable, but no symbols and no edges:**

`.html` `.htm` `.css` `.scss` `.sass` `.less` `.json` `.yaml` `.yml` `.xml`
`.md` `.markdown` `.sql` `.graphql` `.gql` `.proto` `.kt` `.kts`

These are chunked with overlap and embedded, so search finds them. They simply
do not appear in the graph.

Kotlin is the notable case: it has no tree-sitter grammar wired up, so it is
indexed as text — **but its Spring routes are still extracted**, because route
detection has a separate path.

Anything else is not indexed.

## Storage

Edges live in the project's DuckDB database, in `graph_edges`:

| Column | Holds |
|---|---|
| `source` / `target` | Symbol names |
| `kind` | Always `calls` |
| `source_file` / `target_file` | Where each side lives |
| `line` | Where the call appears |
| `repo` / `branch` | Scope — the graph is per-branch, like everything else |

Uniqueness is `(source, target, kind, repo, branch, source_file)`, so the same
call from two different files is two edges, and re-indexing does not duplicate.

## Operations

### `get_references(symbol)` — who calls this?

Every call site of a symbol across the indexed code. The direct answer to *"is
this safe to change?"* at one hop.

### `impact_analysis(symbol)` — blast radius

Transitive callers *and* callees. Callers are the blast radius: everything that
could break. Callees are what this symbol depends on to work.

Run it before refactoring anything public. This is the operation people forget
exists and then regret not running.

### `read_symbol(name)` — the definition

Code, file, line range and kind. Use this when you know the name and want the
thing itself; use `search` when you want code *about an idea*.

## Limits worth knowing

**Coverage is binary per symbol, not uniformly partial.** Measured on a
1,300-file Java/Quarkus repository: `crearNotificacion` returns 8 edges for its
8 call sites — complete — while `actualizar` and `cambiarEstado` return **zero**
despite having real callers. Nothing tells you in advance which group a symbol
falls into, which is why an average coverage figure misleads: it invites
"I am missing some" when you may be missing *all* of the one you are about to
rename.

**A result with edges is reliable. An empty result means "nothing found", never
"nothing there".** On an empty result, cross-check with
`search --keyword` before renaming or deleting.

**Call resolution is name-based**, informed by imports and type bindings where
the grammar supports it.

**Dynamic dispatch is invisible.** A call made through a callback, a reflection
API, or a string-keyed registry leaves no syntactic call edge. The graph will
under-report exactly where a language is most dynamic.

**Only 7 languages produce edges.** In a polyglot repository, the graph covers
part of it, and there is no warning that says which part. `devctx status` shows
the symbol count; a suspiciously low one usually means the code is in a language
that is being indexed as text.

## How it complements search

| Question | Tool |
|---|---|
| *Where is the code about authentication?* | `search` |
| *What calls `authenticate`?* | `get_references` |
| *What breaks if I change it?* | `impact_analysis` |
| *Why is it written this way?* | `memories_by_symbol` |
| *What should I read before answering?* | `build_context` |

Search is fuzzy and ranked. The graph is exact and unranked. The fourth row is
the one people do not think to ask, and it is the one the code cannot answer at
all.

## Mental model

Search is a map of the territory: it shows you what is near what, by meaning.
The graph is the road network: it shows you what actually connects to what, and
therefore where traffic goes when you close a road.

You want the map to find the neighbourhood, and the road network before you dig
anything up.
