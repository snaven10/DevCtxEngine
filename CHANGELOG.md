# Changelog

Notable changes per release. Dates are tag dates. The plan behind 0.8.3 – 0.9.0
is `plans/PLAN-008-robustez-y-salida-para-agentes/`; the one behind 0.10.0,
`plans/PLAN-009-grafo-estatico-y-contexto/`.

## 0.10.0 — unreleased

Not tagged yet: the release waits for the field verification and for the
maintainer's go-ahead. This is PLAN-009, a static symbol graph and context built
on it (`plans/PLAN-009-grafo-estatico-y-contexto/`).

The call graph of 0.9 — one relation, names matched by suffix, library calls
mixed in — becomes a **symbol graph**: symbols with stable ids, seven relations
recorded per occurrence, and a link pass that resolves each one, per language,
with a confidence and the reason. Three new tools read it (`traverse`,
`repo_map`, `skeleton`); `read_symbol`, `get_references`, `impact_analysis` and
search's identifier anchoring move onto it. **Every index needs one `devctx
index --full`**, which reuses the stored vectors.

### Upgrading from 0.9.x

1. **Stop the servers and restart your AI sessions.** `devctx serve --stop`; a
   running `devctx mcp` keeps the code it started with (`mcp.binary_replaced`).
   A 0.9 MCP does not list the new tools nor pass the new parameters; a 0.10
   client against a 0.9 server gets a 404 for the new tools, reported as
   "restart it", and a `warning` when the server ignored new parameters.
2. **Run `devctx index --full` once per repository and per branch you query**
   (`devctx index --full --branch <b>`; `devctx reindex --all --full` for every
   registered project's default branch). The extractor is now version 21 and
   the link rules version 21, so every branch indexed by 0.9 reports
   `extractor_stale: true`; hooks, `watch` and incremental runs never clear
   it. The run re-parses everything and reuses the vector of every chunk whose
   text did not change — measured during development at 24.8–54.9 s per
   `--full` against 278–442 s from scratch on repositories of 216–281 files,
   97.5–100 % of the chunks reused; a few chunks do change text and are
   re-embedded (constructors become chunks, a generic `impl`'s methods lose the
   type arguments in their header). Once one branch is rebuilt the others copy
   the files they share with it.
3. **Until then nothing breaks:** `read_symbol`, `get_references`,
   `impact_analysis`, keyword/hybrid `search` and the web graph view answer as
   0.9 did, with a `warning`; `traverse`, `repo_map` and `skeleton` answer an
   error that asks for `devctx index --full`; `build_context` with `related`
   says the section is not shown; a new branch re-embeds instead of copying.
4. **What an agent sees changes, with no parameter:** `impact_analysis`
   follows only `high`/`medium` calls, leaves test code and library callees
   out, lists 200 symbols per direction, follows dispatch through interfaces,
   and counts what it left out; a bare name means the repository's definitions
   of it, not every call written with it. `get_references` lists one row per
   occurrence, with its `confidence`, and counts the `low` ones instead of
   listing them. JSON contracts only grew.
5. **The database grows** (on a 216-file Java repository, 79 → 112 used blocks
   of 256 KiB, plus 66 free that DuckDB does not reclaim).

The guide, with the measurements and what each tool answers meanwhile:
`docs/13-keeping-the-index-fresh.md`, *Upgrading to 0.10*.

### Going back to 0.9.0

Install 0.9.0, `devctx serve --stop`, restart the AI sessions. The new tables
stay in the database, unread and harmless; nothing is migrated or rewritten.
`graph_edges` was kept current in its 0.9 format, so 0.9.0's graph tools answer
from it (verified with a build that has 0.9.0's read path: the same `impact`
answer byte for byte; the field verification repeats it with the released
binary). 0.9.0 reports `extractor_stale` and works anyway; its `--full` would
re-embed everything (0.9.0 does not reuse vectors). Coming back to 0.10 after
0.9.0 indexed files: the branch reads as stale (`graph_out_of_step`) and asks
for `devctx index --full` again, with vectors reused.

### Added

- **The symbol graph** (`symbols` and `edges`, PLAN-009 TASK-003 to TASK-007).
  One row per definition with a stable, branch-free id (`sym`, 16 hex digits),
  its kind (`file`, `module`, `class`, `interface`, `enum`, `record`, `struct`,
  `trait`, `impl`, `type`, `function`, `method`, `constructor`, `field`,
  `const`), qualified name (every enclosing scope), parent, package, signature
  (the head up to the body, on one line, 200 characters after leading
  annotations) and visibility. One row per *occurrence* of `calls`,
  `instantiates`, `imports`, `inherits`, `implements`, `references` (type uses)
  and `contains`: a second call to the same target from one method, and calls
  outside any function (from a per-file symbol), are no longer lost. New
  definitions: constructors, records, fields, constants, Rust `impl`s, TS/JS
  functions bound to a `const` (`export const x = () => …`, now sources of their
  calls), TS `enum`/`type`, Go `struct`/`interface`, interface and abstract
  methods. Rust test code inside a source file (`#[cfg(test)]` that requires
  `test`, `mod tests`, `#[test]`, `#[tokio::test]`, `#[rstest]`) is test code,
  like a test file.
- **The link pass.** Every run ends by resolving the branch's edges — those
  the run touched, or all of them when a fifth of the branch changed, the link
  rules changed or a manifest changed — to the definition they reach (`dst_id`),
  with a `confidence` (`high`, `medium`, `low`) and a `resolution` (`self`,
  `local`, `param`, `field`, `ctor_inject`, `import`, `same_file`,
  `same_package`, `inherited`, `return_type`, `unique_name`, `name_only`,
  `external_known`, `chain_external`). `external` only with evidence: the
  platform or a dependency a manifest declares (`package.json`, `Cargo.toml`,
  `go.mod`, `pyproject.toml`, `setup.cfg`, `requirements*.txt`), each of them
  read into the link environment (a changed one relinks the branch; an
  unreadable one keeps its last good version). A call on an untyped receiver to
  a name the repository lacks is discarded (kept as `resolution = discarded`,
  out of every answer, reopened when the name appears); `index_repo` reports
  `edges_discarded` and `edges_unresolved`. The incremental pass is tested equal
  to a full one, row by row. Per language:
  - *Java:* lexical scopes (each inner class its own fields), `var` from its
    initializer, constants such as `LOG` never taken for types, constructor
    and field injection, imports, same package, statics a Panache entity
    inherits, `Object` methods, static imports, Lombok accessors, receivers
    through fields, declared return types along a chain.
  - *TypeScript/JavaScript:* relative imports, the root `tsconfig`'s `paths`
    and `baseUrl`, barrels up to four levels, packages external only when a
    `package.json` declares them (or a Node builtin), Angular DI (constructor
    parameter properties, `inject(T)`), lexical scopes, declared return types.
  - *Python:* modules by path and package, relative and star imports,
    re-exports, `self.x` typed in methods, `x = Foo()`, the standard library,
    builtins and declared distributions as external, `typing` forms.
  - *Rust:* crates by `Cargo.toml`, modules, scoped `use`, `crate::`/`super::`/
    `Self::`, `pub use`, a type's `impl`s across its crate, `impl Trait for T`,
    every bound of a type parameter, `?`/`unwrap()`; macros are not calls.
  - *Go* (fixtures only): `go.mod` decides what is the repository's, methods
    by receiver type across a package's files, build-tag twins at `medium`.
  Measured on call sites labelled by hand: Java 20 of 25 right (19 of 19
  `high`), TypeScript 20 of 20, Python 26 of 27, Rust 26 of 26; calls left
  undecided 4–8 % (Java), 11 % (TypeScript), 2 % (Python), 17.5 % (Rust), from
  59–94 % unqualified before. `graph_edges` loses its `var.*` and `LOG.*`
  targets.
- **`traverse`** (TASK-013): a new tool (MCP, `POST /traverse`,
  `devctx traverse`) to walk the symbol graph hop by hop from a symbol — a bare,
  qualified or `file::name` symbol, a file path, or one `sym` — along the
  relations asked (`calls`, `instantiates`, `imports`, `inherits`,
  `implements`, `contains`, `references`; default `calls`), `in`, `out`
  (default) or `both`, 1 to 4 levels (more is refused). It is the walk of
  `impact_analysis` (one query per level and direction, live edges only, by id,
  the same `min_confidence`/`include_tests`/`include_external` filters with what
  they leave out counted, dispatch through supertypes on `calls`) and answers
  `{root, candidates?, nodes, edges, total, next_offset?, omitted?, partial?,
  filters, …}`: nodes with `sym`, `kind`, `file`, `line`, `depth`,
  `confidence`, `via`; the edges that reached them (`from`/`to` as `sym`s, one
  per pair and relation with its occurrences). Paged (`limit` 50, `offset`):
  once a page is full no deeper level is read (`partial`, and then
  `total_is_lower_bound: true`), so a type referenced from hundreds of files is
  paged, not dumped. Calls to the name the index could not decide are counted
  (`undecided_calls`) or, with `min_confidence: "low"`, listed at depth 1.
- **`repo_map`** (TASK-011): a new tool (MCP, `GET /repo-map`, `devctx map`)
  that answers, in about a thousand tokens (`budget`, default 1024), a map of
  the repository to orient an agent: the signatures — never bodies — of the
  symbols most relevant to its focus (`focus_files`: files it has open, whose
  uses rank first and which are not listed themselves; `focus_symbols`;
  `query`, whose identifiers count), grouped by file, members under their
  containers, ranked by PageRank personalized to that focus over the same
  weighted matrix as the global rank (each focus file and each focus term
  weighs the same, shared among its symbols; live edges only; test code and
  libraries never appear). Text, closed by `[devctx]` lines that say what the
  focus matched and how many symbols did not fit. The serve caches the
  branch's adjacency per index generation (dropped when an `index` ends and
  when idle, like a model; `status.memory.repo_map` reports it).
- **`skeleton`** (TASK-014): a new tool (MCP, `GET /skeleton?file=…`,
  `devctx skeleton <file>`) that answers the structure of one file to locate
  where something is: its symbols with their signatures and line ranges
  `[l1-l2]`, nested by container, without bodies, and its imports in one line
  (`imports (N): …`, the first 8). Read from the symbol graph, never from the
  file; signatures cleaned as in `repo_map`. Test code is marked `[test]`, not
  hidden. Within `DEVCTX_MAX_OUTPUT_TOKENS` (at least 256), closed by
  `[devctx] N more symbols (lines a-b)` when cut. A file the index does not
  hold is an error naming the nearest indexed paths, and a path with `..` or
  outside the repository is refused before anything is looked up. Measured on
  12 files of 623-4 684 lines in four repositories: 4.9-10.2 % of the file's
  tokens, p95 ≤ 26 ms.
- **Related signatures in `build_context`** (TASK-012), **off by default**:
  with `related: true` (MCP and `POST /context`; `devctx context --related`)
  the brief adds `## Related (signatures)` between the code and *Recorded
  against this code*: the callers, callees, instantiations and supertypes (and
  the supertype method a method overrides) of its first 5 code hits, as
  signatures with their relation and `file:line`, never bodies — `high`/`medium`
  edges only, no tests or libraries, nothing already shown, by PageRank, 4 per
  hit. `graph_hops: 2` (`--hops 2`) adds the neighbors' neighbors. The section
  takes at most 20 % of the budget from the code but never the best hit, no
  line stands without its hit in the code, the whole brief stays within the
  budget, and what is left out is counted in the brief's `omitted` line. Off,
  it reads nothing and the brief is laid out byte for byte as in 0.9. On the
  evaluation set it raised the expected-file rate 21 → 22 of 26, short of the
  10 points a default required.
- **Symbol centrality in hybrid search, opt-in** (TASK-010): every link pass
  that completes computes a global PageRank of the branch's symbols
  (`symbols.rank`, written only where it changed) and their `in_degree`, over
  live edges: calls and instantiations weigh 1, inheritance 0.8, type uses 0.5,
  imports 0.3, `contains` 0; `high` 1, `medium` 0.6, `low` 0.2, from a test
  × 0.1, `sqrt(n)` for repeated calls; damping 0.85, at most 100 iterations,
  deterministic. Hybrid search — and so `build_context` — can add it as a third
  list of the rank fusion over the candidates the retrievers brought,
  `search.centrality_weight` (0-1; a value outside, or not a number, is
  bounded and warned about). **Off by default** (`0`): on 12 of the 30
  evaluation cases turning it on was within the noise and the one Spanish case
  that moved got worse; it is re-evaluated on all of them, by language. When
  on, hybrid `score`s and order change and a ranked hit carries `raw_score`;
  vector search is unchanged and an anchored definition still comes first.
  Cost: 21–165 ms per link pass; hybrid search p50 within a few ms.
- **Dispatch in `impact_analysis`** (TASK-017), on by default: a caller that
  holds a service by its interface (an injected field, a parameter typed by a
  trait) is in the blast radius of the implementation. At every depth,
  upstream, a method reaches the callers of the methods it overrides in its
  supertypes, and downstream an interface or abstract method reaches the
  methods that implement it and what they call; through resolved
  `inherits`/`implements` edges, up to four supertype levels, same name and an
  arity that can match (in TS/JS an implementation may take fewer parameters),
  never a private or static method, not in Go (embedding is no override).
  Such a node has `via: "dispatch"` and `through`, and confidence
  `min(medium, the edge it stands for)` — never `high`. At most 16 equivalents
  per node, 16 origins per equivalent and 8 192 dispatch rows per call; what
  that cuts is counted in `omitted_by_limit.dispatch` (`count`, `beyond_depth`,
  `origins_capped`, `rows_capped`). `dispatch: false` (`--no-dispatch`,
  `?dispatch=false`) follows direct calls only, `min_confidence: "high"` leaves
  it out without reading it, and `dispatch: {"evaluated": false, reason}` says
  so. Also in `traverse` on `calls`.

### Changed

- **Lookups by symbol** (TASK-008): on a branch with a current symbol graph,
  `read_symbol`, `get_references`, the identifier anchoring of `search` and the
  web graph view read the `symbols` table and the resolved edges by id instead
  of matching names and suffixes. Definitions add `sym`, `kind` and
  `qualified`; anchored hits add `sym`; references add `confidence`, `via` and
  `sym`, plus `external`, `test` and `undecided` when true. `get_references`
  lists one reference per occurrence of calls and instantiations; **`kinds`**
  adds type uses, imports and inherits/implements (or `all`) — the calls always
  stay; **`min_confidence`** (`high`, `medium` — default —, `low`) and what the
  default leaves out is counted in `below_confidence`. `external` comes from
  the link pass's evidence: `Foo::new` that does not exist is no longer external
  because some `new` is called (it gets `suggestions`), and a `file::name` miss
  is never external. A call the link pass discarded reaches no answer.
  Anchoring reads every definition's code in one query and pins one chunk per
  definition.
- **`impact_analysis` walks the symbol graph by levels** (TASK-009): one query
  per depth and direction over ids, whatever the size of the frontier (an `IN`
  list up to 512 ids, one `UBIGINT[]` parameter above). **A bare name means the
  repository's definitions of it**, as in `get_references`; a name with no
  definition stands for its external call sites and answers `external` +
  `called_from`, or `suggestions`. **New defaults:** only `high`/`medium` calls,
  no symbols of test files, no library callees, 200 symbols per direction; what
  that leaves out is counted (`below_confidence` in symbols, `undecided_calls`
  in calls, `excluded`, `omitted` + `omitted_by_limit`) and not walked, and
  `min_confidence`, `include_tests`, `include_external`, `max_nodes` (tool,
  `GET /impact`, `devctx impact` flags) change it; `filters` says which applied.
  Each symbol adds `confidence`, `via`, `sym`, `file`, `line` and
  `test`/`external`/`undecided` when true, ordered within a depth by
  confidence, rank and name. On a 1 250-file Java repository, p95 of `get` and
  `map` went from 4.2 and 5.7 s to 41 and 86 ms. The TUI's local view gives
  the tool's answer.
- **Signatures:** leading annotations no longer count against the
  200-character cap: the head is capped on its own, ending in `…` when cut,
  and the annotations that fit whole in another 200 stay in front.
- **The type of an `impl` is named as its definition is** (`impl<T> Foo<T>`,
  `impl Display for &'a Foo`, `impl crate::x::Foo` → `Foo`): the `graph_edges`
  source of such a method is `Foo.get` (was `Foo<T>.get`) and its chunks'
  header changes with it.
- **Reindex reuses vectors by chunk `content_hash`** (TASK-002): a `--full`
  embeds only the chunks whose text changed, and over unchanged source embeds
  nothing, loads no model and rebuilds no HNSW/BM25 index (measured: 4.4 s
  against 240 s from scratch on this repository). The branch's embedding
  fingerprint (provider, model, width, engine read from `Cargo.lock`,
  character cap, normalization, HTTP endpoint) must equal the active one; a run
  under another setup marks the branch "transition" until it completes. Indexes
  from before fingerprints are reused only when their recorded model and width
  equal the active ones; the run seals them. The same rule, and a current
  graph, decide which branches may be a source for the cross-branch copy.
- With `storage.hnsw: true`, an index built through the server now leaves the
  HNSW index behind too (before, only the direct path did, and a server-built
  index searched by full scan); a database without one pays for building it at
  its next run.
- `index` reports `files_unchanged` separately from `files_indexed`.
- `extractor_stale` is also true when a branch's graph is out of step with its
  files (files added, deleted or modified by a binary that does not write the
  graph, such as 0.9.0 after a downgrade); such a branch is not a copy source.

### Fixed

- Opening an index that has an HNSW index loads VSS *before* creating tables
  added by a release: the checkpoint after the DDL failed fatally ("unknown
  index type 'HNSW'") and invalidated the database for the process. Where VSS
  cannot be loaded at all (offline), every checkpoint over such an index is
  skipped instead, leaving the write-ahead log for an open that can.
- `file::symbol` subjects (`memories_by_symbol`, `read_symbol`) take the left
  part as a file only with a `/` or a known language extension: `Foo.Bar::baz`
  is the member `baz` of `Foo.Bar`, no longer the file `Foo.Bar`; an npm scope
  (`@scope/pkg::fn`) is not a directory.
- `devctx symbol` without a server answers as the tool does: branch fallback,
  warnings, and on a miss its suggestions or external call sites.
- A `--full` over several indexed branches no longer copies and rewrites every
  file another branch holds; and a branch made by another embedding setup is no
  longer a copy source.
- Deleting a file the index never held no longer drops the HNSW/BM25 indexes.
- Indexing a TypeScript repository no longer panics on a type whose `>` (an
  arrow's) comes before its first `<`.

### Known issues

- 0.8.3 – 0.9.0: the first open of an HNSW database created with 0.8.2 or
  earlier fails once with "unknown index type 'HNSW'"; retrying resolves it.
  Fixed in this release (see above); there is no 0.9.1.
- Not measured yet on equal terms against 0.9.0: indexing from scratch,
  database size and peak memory, and the upgrade of real 0.9.0 indexes with the
  final binary (field verification before the tag).

## 0.9.0 — 2026-10-05

The agent-facing half of PLAN-008 (P1): tool output that fits in a context
window, honest ranking, and a project-aware `build_context`.

### Breaking

- **`search`, `search_routes`, `routes_for_handler`, `search_project` and the
  group-wide `search` always answer with an object**, never a bare array. Rows
  live under `results` (search, `search_project`, group search) or `routes`
  (`search_routes`, `routes_for_handler`). Group search and `search_project`
  used `hits` before; `hits` is gone. `devctx search --format json` follows:
  `{"results": [...]}` instead of an array.
- **`search_routes` returns 20 routes per page, `plan_status` 25 plans per page,
  `memories_by_symbol` / `memories_by_file` 5 memories** with `content` cut to
  600 characters. Pass `limit` / `offset` to page, `full: true` for whole
  memories.
- **`/build/` and `/target/` (repository root) and `node_modules/`, `dist/`,
  `vendor/`, `third_party/`, `bower_components/`, `*.min.js`, `*.generated.*`
  are excluded from the index by default**, even when git tracks them.
  Opt out with `indexing.include_build: true` (only `/build/`) or
  `indexing.default_excludes: false` (all of them).
- Ranking changes results: tests, docs and config files rank below code.
- **`read_symbol` with `external: true` no longer carries a `suggestions` key**
  (it used to be `[]`). Suggestions are only given for names that look like a
  typo of something defined in the repository.

### Added

- **Pagination and `omitted`.** `limit` / `offset` / `next_offset` on
  `search_routes`, `plan_status` and `memories_by_*`; every answer that cut
  something carries `omitted: {count, reason: "limit" | "budget", next_offset?}`.
  `plan_status` gains `active_only`. `omitted_for_budget` stays one release as
  an alias.
- **`project` on the tools that lacked it**: `build_context`,
  `memories_by_symbol`, `memories_by_file`.
- **`kind` (`code|test|doc|config`) and `include_tests`** on `search`,
  `search_project` and `build_context`; `devctx search --kind … --no-tests`.
  Config `search.penalty.{test,doc,config}` (default `0.6`: a hit is demoted
  about four positions).
- **Dedup and identifier anchoring.** The same chunk is returned once; a query
  that contains an identifier (`state.rs plan_status_list`, `Foo.bar`,
  `snake_case`) puts that definition first (`anchored: true`, at most 3
  identifiers per query). Hits carry `kind`, `language` and, when the score was
  rewritten, `raw_score`.
- **`build_context` in a group.** Without `project`, the best-matching member is
  chosen among the members whose server is already running (never spawns
  anything, 2.5 s deadline) and named with its coverage (`k of N members
  scored`); a close call adds an `ambiguous` warning. The budget is split by
  relevance and a chunk that does not fit no longer ends the brief.
- **`index_meta` and `extractor_stale`.** Indexes remember which extractor made
  them; `index_status` reports `extractor_stale: true` with a hint and the graph
  tools add a `warning` until `devctx index --full`.
- **`branch_fallback`.** `read_symbol`, `get_references`, `impact_analysis`,
  `search_routes`, `routes_for_handler`, `search`, `build_context` and
  `memories_by_symbol` answer from the default / most recently indexed branch
  when the checked-out one has no rows, and say so
  (`{current, used, why}`). With no index at all they fail with an explicit error.
- **`read_symbol` not found** returns `suggestions` (up to 5) and, for names that
  are only ever called, `external: true`, `called_from` and a `next_step`.
  Qualified names (`serde_json::from_str`, `Panache.withTransaction`) are
  matched as written and by their last segment, and an external no longer gets
  fuzzy suggestions from this repo.
- **Group `build_context` selection is actionable.** When comparable members
  could not be scored, the warning says `Likely (named by the question): X, Y —
  pass project=X` and the brief carries `[devctx] selection:
  {"scored","total","candidates"}`. It still never starts a server to select.
- **`memories_by_symbol("file::symbol")`** splits the file part off: the symbol
  is looked up alone and a memory whose `files` names that file is
  `files-field`, not `inference`.
- **`link_sources: "files-field"`** when a memory's `files` names the file, also
  in the text fallback.
- **Exclude changes reconcile incrementally.** Changing the effective exclude
  set (including upgrading to this release) prunes the files now excluded and
  adds the ones no longer excluded, without re-embedding the rest, and logs the
  count per pattern.
- **Lifecycle.** `serve` answers `503` + `X-Devctx-Exiting` while it exits, and
  the MCP, CLI and central client retry that one status. `serve.json` stays
  until the final checkpoint is done. Idle exit, `serve --stop` and SIGTERM
  cancel a running index cleanly. A client that meets a server in its exit
  window waits for it (up to 3 s) instead of reporting it as busy or hung, and
  a spawn that dies on a lock held by a server still starting waits for that
  server and reuses it. The central daemon's idle exit now withdraws its
  `serve.json` too.
  The wait for a holder that is starting is 60 s, and only for a server a client
  spawned less than 60 s ago (or one checkpointing): an old server that lost its
  `serve.json` fails fast with its PID.
- A server run from a renamed copy of the binary is recognised by the same
  executable (device and inode), so `serve --stop` stops it instead of deleting
  its `serve.json` and orphaning it. A central daemon that loses the start-up
  race logs one line and exits instead of an error.
- **macOS:** `serve --stop` waits for the real exit with kqueue.
- `CONTRIBUTING.md`: the test model cache (`.devctx-test-models`).

### Fixed

- A transaction that panics in `Store::in_transaction` rolls back and resets its
  state; `drop_branch` is transactional.
- `ps`-based process identification outside Linux no longer accepts
  `/usr/bin/vim /x/devctx serve`.
- `devctx index` / `projects reindex` exit non-zero when the server cancelled
  the run.

### Upgrading from 0.8.x

1. **Restart your AI sessions.** A running `devctx mcp` keeps the code it
   started with. A 0.8.4 MCP talking to a 0.9 server still works, but it
   reads the old array shapes and cannot pass the new parameters.
   `index_status` / `list_projects` report `mcp.binary_replaced` and a hint when
   this happens.
2. **Update JSON consumers** of `search`, `search_routes`, `routes_for_handler`,
   `search_project` and group search to read `results` / `routes`.
3. **The first `devctx index` after upgrading reconciles excludes** (the stored
   fingerprint is absent): expect a pass that prunes `build/`, `node_modules/`
   and friends if git tracks them — but it does not re-embed anything else.
4. **An index built by 0.8.2 or earlier has no `index_meta`** and is reported as
   `extractor_stale`. Run `devctx index --full` once; until then a new branch
   re-embeds every file instead of copying from one that is already indexed.
5. **Existing git hooks keep the old block** until `devctx hooks install` is run
   again in each repository (the server cleans the environment itself, so they
   are protected meanwhile).

## 0.8.5 — 2026-10-05

Memory hotfix.

- Embedding batches run one after another instead of in parallel, and the
  default batch drops from 32 to 8 (`DEVCTX_EMBED_BATCH_SIZE` still wins). Peak
  memory scales with the batch.
- The central daemon releases its embedder after `DEVCTX_MODEL_IDLE_SECS`
  (300 s) without use and reloads it on the next `remember` / `recall`.

## 0.8.4 — 2026-10-04

- Fix-forward of 0.8.3: the Windows build failed (`errno_result` of
  `procown` used `libc` without `cfg(unix)`). On non-unix, `terminate` now
  reports `Unsupported` instead of declaring a process dead.
- CI: a `cross-check` job (Windows, macOS) and runs on pushes to `fix/**`.

## 0.8.3 — 2026-10-04 (tag only, no release)

Tagged but never released: the Windows build failed. Everything below shipped
in 0.8.4. This is PLAN-008 P0, robustness.

- **The MCP never opens the database.** `devctx mcp` is a lazy remote client of
  `devctx serve`: no local backend, no lock held between calls. A missing
  server is started on demand; the error carries the cause from `serve.log`.
- **Fail fast on a locked database.** The CLI names the PID that holds the
  lock, its age and the remedy in under 2 s (it used to take ~63 s).
- **Reconnects.** A tool call after `serve --stop` or an idle exit reconnects
  and retries once, only when the failure happened before anything was sent.
- **The MCP exits on stdin EOF** (about 6 s worst case, with a tool in flight)
  and reports `mcp.version` / `mcp.binary_replaced` in `index_status` and
  `list_projects` instead of leaving a silent stale process.
- **Clean git environment.** Hooks unset `GIT_DIR` and friends; the server and
  every `git` subprocess strip the repository-selecting `GIT_*` variables.
- **Honest graph tools.** `branch_fallback` (see 0.9.0) first shipped here as
  array-or-object output; `index_status` adds `indexed_branches` and hints.
- **Extractor version** in the new `index_meta` table; `extractor_stale`.
- **Federated `recall`** no longer re-executes a deleted binary; failures name
  the member and path, and members missing on disk are listed in
  `skipped_missing`.
- **Process ownership.** `serve --stop` verifies the PID is a devctx server of
  this project before signalling it (Linux: pidfd and `/proc`; macOS: `ps` and
  `lsof`; Windows: reported as unsupported) and never deletes `serve.json` of a
  process that survived.
- **Graceful shutdown.** SIGTERM cancels a running index, checkpoints and exits
  (`DEVCTX_SHUTDOWN_GRACE_SECS`, clamped to 3 s); idle exit works with an index
  stuck (`DEVCTX_INDEX_STALL_SECS`, 900 s); a server whose project directory
  vanished exits by itself (`DEVCTX_VANISH_POLL_MS`); a model download with no
  progress fails instead of hanging (`DEVCTX_MODEL_STALL_SECS`, 120 s).
- `devctx init` never downloads model files unless asked (`--download`, or an
  interactive terminal) and never writes an empty `model_dir`;
  `devctx models --download <model>` fetches one.
- **Tests no longer leak servers.** Every integration test that starts a
  `devctx serve` stops it when it ends and sweeps any server still running under
  its temporary directory; a test that leaves one behind fails the run, since
  stray servers were what made later runs flaky.
