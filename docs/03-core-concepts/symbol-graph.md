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

Every call site of a symbol across the indexed code — calls and instantiations
(`new Foo()`), one per occurrence, so two calls from the same method are two
references. The direct answer to *"is this
safe to change?"* at one hop. `symbol` is a bare name (`charge`), a qualified one
(`Card.charge`, `Card::charge`) or `file::name` (`src/pay.rs::charge`).

On an index made by 0.10 or later, each reference keeps `file`, `line` and
`source` and adds:

| Field | Meaning |
|---|---|
| `confidence` | `high`, `medium` or `low`: how sure the index is that this occurrence means *this* definition |
| `via` | The relation: `calls` or `instantiates` by default; with `kinds`, also `references` (a type use), `imports`, `inherits`, `implements` |
| `sym` | The id (16 hex digits) of the definition referenced, when it resolved to one |
| `external` / `test` / `undecided` | Present (and `true`) only when the occurrence is a library call, sits in a test file, or could not be decided |

**`min_confidence`** (`"high"`, `"medium"` — the default — or `"low"`) sets the
lowest confidence listed. By default `high` and `medium` are listed, `medium`
marked by its field; ambiguous references and calls the index could not decide
are left out and counted in `below_confidence: {count, min_confidence, hint}`,
never dropped in silence. Pass `min_confidence: "low"` to list them, each marked.
For a name with no definition in the repository (a library function), the answer
lists its external call sites.

**`kinds`** adds relations to the default calls and instantiations: any of
`references` (type uses), `imports`, `inherits`, `implements`, or `all`
(`GET /references/<symbol>?kinds=references,imports` over HTTP). A server older
than 0.10 ignores `min_confidence` and `kinds`; the client then says so in
`warning`.

### `impact_analysis(symbol)` — blast radius

Transitive callers *and* callees. Callers are the blast radius: everything that
could break. Callees are what this symbol depends on to work.

Run it before refactoring anything public. This is the operation people forget
exists and then regret not running.

Each side is a list of `{symbol, depth}` (depth 1 = direct), deepest last. On an
index made by 0.10 or later the walk follows the symbol graph **by id** — one
query per level, whatever the size of the frontier — and each symbol also
carries `confidence` (of the call that reached it), `via` (`calls`,
`instantiates` or `dispatch`), `sym`, `file` and `line` (of its definition), and
`test`, `external` or `undecided` when true. Within a depth, symbols are ordered by
confidence, then by rank (the global PageRank each index run computes; see
[Search](search.md#centrality-hybrid-only-opt-in)), then by name.

What the defaults leave out is **counted, never dropped in silence**, and not
walked:

| Parameter (CLI flag) | Default | Left out by default, counted in |
|---|---|---|
| `min_confidence` (`--min-confidence`) | `medium` | symbols reached only by calls of `low` confidence → `below_confidence: {count, min_confidence, hint}` (counted in symbols); calls to the name the index could not decide (`svc.update()` on an untyped receiver) → `undecided_calls: {count, hint}` (counted in calls, none from a test or from a caller already listed); `low` lists both, the callers of the undecided calls marked `undecided` and not walked |
| `include_tests` (`--include-tests`) | `false` | symbols of test files → `excluded.tests` |
| `include_external` (`--include-external`) | `false` | library callees → `excluded.external` |
| `max_nodes` (`--max-nodes`) | `200` per direction (`0` = no cap) | the symbols of the depth that overflows it → `omitted: {count, reason: "limit"}` and `omitted_by_limit: {count, max_nodes, upstream/downstream: {count, depth}}`; no deeper level is read |
| `dispatch` (`--no-dispatch`) | `true` | nothing: dispatch through interfaces, abstract classes and traits is followed (below); `false` follows direct calls only. A node with more override-equivalent methods than it is expanded into (16) → `omitted_by_limit.dispatch: {count, max_per_node}`, added to `omitted_by_limit.count` and `omitted.count` |

**Dispatch.** A caller that holds a service by its interface (a field injected as
`IService`, a parameter typed by a trait) calls `IService.update`, never
`ServiceImpl.update`; the walk follows that call anyway, through the resolved
`inherits`/`implements` edges, at every depth (not only from the symbol asked
about):

- **upstream**, `ServiceImpl.update` reaches the callers of the methods it
  overrides in its supertypes (`IService.update`, `BaseService.update`), at the
  depth of a direct caller;
- **downstream**, an interface or abstract method reaches the methods that
  implement it, one level below it, and what those call.

A method is override-equivalent when it has the same name and an arity that can
match (defaults, optional and variadic parameters widen it); a private or static
method overrides nothing. The chain of supertypes is followed up to four levels
(`ServiceImpl` → `BaseService` → `IService` → its super-interface). Such a node
has `via: "dispatch"` and `through` (the method it went through), and its
confidence is `min(medium, the edge it stands for)` — never `high`, and a `low`
call stays `low` (counted in `below_confidence` by default). To leave dispatch
out: `dispatch: false` (`--no-dispatch`, `?dispatch=false`), or
`min_confidence: "high"`, which also leaves out every `medium` call. Java,
TypeScript (`implements`, abstract classes) and Rust traits are covered; Python
base classes on a best-effort basis; Go not at all (embedding promotes methods, it
does not override them, and an interface is satisfied without an edge to follow).

Over HTTP: `GET /impact/<symbol>?depth=3&min_confidence=low&include_tests=true&max_nodes=500&dispatch=false`.
The answer says which filters it applied in `filters`. A bare name means the
repository's definitions of it (as in `get_references`), not every call written
with that name.
The fields of 0.9 keep their meaning: `resolved_symbols` (a bare name that stood
for several declarations), `branch_fallback`, `warning`, and `omitted` /
`omitted_for_budget` (half the output budget per direction; `omitted.count`
adds what the budget and `max_nodes` cut). A name with no definition in the
repository answers as `read_symbol` does: `external: true` with `called_from`
(its upstream is then the callers of its call sites) or `suggestions`. On an
index made before 0.10 the walk is 0.9's — by name, no confidence, no filters, no
cap — with the `warning` that says so (and, when filters were passed, that they
were not applied); a server older than 0.10 ignores the parameters, and the
client then says so in `warning`.

### `read_symbol(name)` — the definition

Code, file, line range and kind. Use this when you know the name and want the
thing itself; use `search` when you want code *about an idea*. `limit` (default
5) caps the definitions — a name can be defined many times. The name can be bare,
qualified (`Card.charge`, `Card::charge`, with its package or crate in front) or
`file::name`; `Foo.Bar::baz` is the member `baz` of `Foo.Bar`, not a file.

On an index made by 0.10 or later each definition also carries `sym` (its id, 16
hex digits, the same on every branch), `kind` (`function`, `method`, `class`,
`field`…) and `qualified`. A definition the chunker gives no code of its own (a
field, a constant) answers with its signature as `code`.

**A miss is explained, not just empty.** When no definition is found,
`read_symbol` adds:

- `suggestions` — up to 5 names to try: the qualified forms the call graph knows
  for a bare name (`charge` → `Card.charge`), then the closest defined names by
  prefix, suffix, containment or edit distance (`AuthServce` → `AuthService`);
- `external: true`, `called_from: N` and a `next_step` when the name is only ever
  a call *target* — most likely a library or runtime function, though it may live
  in a file the index excludes or on another branch. A qualified name
  (`serde_json::from_str`, `Panache.withTransaction`) is looked up as written and
  then by its last segment, since the graph often holds only the bare callee. An
  external gets no fuzzy `suggestions` (`with_context` is not a typo of
  `build_context`). On a 0.10 index an external answers no `suggestions` at all,
  as 0.9 did; and on a 0.10 index
  `external` comes from the index's own evidence (a declared dependency, the
  platform), and the last segment is only tried when the repository defines
  nothing by that name: `Foo::new` that does not exist is not "external" because
  some `new` is called — it gets `suggestions` (`Thing.new`). A `file::name` that
  matches nothing in that file is never external either.

Small functions that the chunker grouped into one chunk (`a, b, c`) are found
inside it, so they are not misreported as external.

**An index made before 0.10** (or by another extractor) has no symbol table:
`read_symbol`, `get_references`, the anchoring of `search` and the web graph view
answer as 0.9 did, by name over the call graph, and say so in `warning` — reindex
with `devctx index --full` to get the fields above. `search` in `keyword`/`hybrid`
says it too: its identifier anchoring went by name. A failed read of the symbol
table gets its own `warning` (a read error, not a reason to reindex).

## Which branch it answers from

The graph is per branch. When the checked-out branch has no indexed rows,
`read_symbol`, `get_references`, `impact_analysis`, `search_routes`,
`routes_for_handler` (and `search`, `build_context`, `memories_by_symbol`) answer
from the default or most recently indexed branch and add
`branch_fallback: {current, used, why}`; with no index at all they fail with an
explicit error instead of `[]`. See
[Search → Branch awareness](search.md#branch-awareness).

## Limits worth knowing

**Names in the graph are qualified.** An edge's `source` is written
`Class.method` whenever the calling function sits inside a container; its
`target` is written that way when the call site had a receiver whose type could
be resolved, and as a bare name when it did not. A question asked with a bare
name expands to every qualified form carrying it, and the answer reports which
declarations it merged. Twenty-one methods named `actualizar` is an ordinary
number for one Quarkus repository — a blast radius that folds them together
without saying so would be worse than an empty one, so read that line.

Ask with the qualified name when you mean exactly one method. That is the only
way to narrow it, and it is why qualified names are never expanded.

> This is the correction to an earlier claim in these pages that coverage was
> "binary per symbol, with nothing to tell the cases apart". It was not. A bare
> `actualizar` returned nothing while `OfficeService.actualizar` returned one
> caller and twenty-three callees: the edges were there the whole time, under a
> key nobody was asking with.

**An empty result still means "nothing found", never "nothing there".** Two of
the reasons used to be silent and no longer are: a branch with no rows
(`branch_fallback`) and an index built by an older extractor (`warning` on every
graph answer, `extractor_stale` on `index_status` — run `devctx index --full`).
The ones below still announce nothing. Cross-check an empty result with
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
