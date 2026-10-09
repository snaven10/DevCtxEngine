# Changelog

Notable changes per release. Dates are tag dates. The plan behind 0.8.3 – 0.9.0
is `plans/PLAN-008-robustez-y-salida-para-agentes/`.

## Unreleased (0.10.0)

### Changed

- **Reindex reuses vectors by chunk `content_hash`** (PLAN-009 TASK-002): a
  `--full` over unchanged source embeds nothing, loads no model and rebuilds no
  HNSW/BM25 index. The branch's embedding fingerprint (provider, model, width,
  engine read from `Cargo.lock`, character cap, normalization, HTTP endpoint)
  must equal the active one; a run under another setup marks the branch
  "transition" until it completes.
- **Indexes from before fingerprints** are reused only when their recorded model
  and width equal the active ones (and no run left them in "transition"); the
  run seals them. The same rule decides which branches may be a source for the
  cross-branch copy, which now also requires the source's fingerprint to match.
- `index` reports `files_unchanged` (files a `--full` found identical and did
  not write) separately from `files_indexed`.
- **New `symbols` and `edges` tables** (PLAN-009 TASK-003): one row per
  symbol with a stable, branch-free id, and one row per call *occurrence* —
  the second call to the same target from one method, and module-level calls
  (sourced from a per-file symbol), are no longer lost. Written in the same
  per-file transaction as the vectors and maintained on delete, branch copy and
  branch prune; a renamed file's rows are dropped and rewritten by the reindex
  at the new path (the id carries the file, so its symbols get new ids). The
  qualified name carries every enclosing scope (module, function, class,
  namespace, Go receiver) and a Rust trait impl carries its trait, so adding a
  homonym elsewhere in a file does not move an existing symbol's id.
  `graph_edges` is still written in the same format, so 0.9.0 can read a
  downgraded index. **Extractor version 2:** existing indexes report
  `extractor_stale` until `devctx index --full`; so does one whose graph lost
  step with its files (files added, deleted or modified by 0.9.0 after a
  downgrade), and such a branch is no longer a source for the cross-branch
  copy.
- **Structured extraction** (PLAN-009 TASK-004, **extractor version 3**:
  existing indexes report `extractor_stale` until `devctx index --full`, which
  reuses the vectors of unchanged chunks). New symbol kinds: `constructor`,
  `record`, `field`, `const`, `impl` (Rust), TS/JS functions bound to a `const`
  (`export const x = () => …`) and arrow-valued class fields, TS `enum`/`type`,
  Go `struct`/`interface`. Symbols carry a package (declared or derived from
  the path), a signature (the head up to the body, multi-line collapsed) and
  `exported`. `edges` now holds `imports`, `inherits`, `implements`,
  `instantiates`, `references` (type uses) and `contains` besides `calls`;
  only `contains` is resolved at write time (both ends in the file,
  `resolution = 'structural'`). Calls inside an arrow-const have it as their
  source (they used to be dropped); a callback, an arrow bound to an object
  key or to `this.x` is not a source, its calls belong to the enclosing
  function; TS/JS re-exports (`export { x } from './y'`) are imports;
  Rust path calls keep their path (`Foo::bar()` → `Foo.bar`, `Self::new()` →
  the impl's type, `std::fs::read()` → `std::fs::read`); a Go method's calls
  come from `Type.method`. Constructors are chunks of their own; fields,
  constants and `impl`s are not (chunk churn on a Java repo: 80 of 3 284).
  Parsers are cached per language and thread (queries were compiled per file).
  A golden test pins the extractor's output per version: symbols with their
  parent, visibility and signature, and every edge with its source.
- **The type of an `impl` is named as its definition is** (`impl<T> Foo<T>`,
  `impl Display for &'a Foo`, `impl crate::x::Foo` → `Foo`). This changes
  content, not format: the `graph_edges` source of such a method is now
  `Foo.get` (was `Foo<T>.get`), and its chunks' context header changes with
  it, so the first `--full` after upgrading re-embeds those chunks instead of
  reusing them (measured on this repository: 40 of 4928 chunks, 143 of 16 123
  `graph_edges` rows).
- **Extractor version 4** (review of TASK-004; `devctx index --full` again):
  a Rust `impl`'s ids no longer change when `rustfmt` lays its `where` clause
  or generic arguments out vertically (trailing commas dropped); the
  `graph_edges` source of a named function is its symbol's qualified name
  (`traced.wrapper` for a decorator's `wrapper`, `C.init.run` for an anonymous
  class's method, `C.m.f` for a function nested in a method; `const x =
  function named() {}` is `x`), so `impact_analysis`/`get_references` no longer
  lose those callers; `export default foo;` marks `foo` exported.
- **Extractor version 5** (`devctx index --full` again): a comment inside an
  `impl`'s `where` clause or generic arguments no longer changes its ids, a
  one-element tuple argument (`impl Foo<(u8,)>`) no longer shares the ids of
  `impl Foo<(u8)>`, and `export default (foo);` marks `foo` exported.

- **Link pass** (PLAN-009 TASK-005, **extractor version 6**: `devctx index
  --full`): every run ends by resolving the branch's edges to the symbol they
  reach (`dst_id`, `confidence`, `resolution`, `external`), incrementally after
  the files it wrote. A call's receiver is typed by lexical scope in Java (each
  inner class has its own fields; `var` from its initializer; for-each
  elements; constructor injection), recorded per edge as `hint`, and a
  constant such as `LOG` is never taken for a type, so `graph_edges` loses its
  `var.*`/`LOG.*` targets. Java rules: imports, same package, nested types,
  inheritance, Panache statics and `Object` methods as inherited externals,
  static imports, the declared return type along a fluent chain. Calls on a
  receiver nothing types, to a name the repository does not define, are
  dropped and counted (`index_repo` reports `edges_discarded` and
  `edges_unresolved`). Measured on two Java repositories: 2.7 % of calls left
  undecided (was 100 %), 18 of 20 hand-labelled call sites resolved correctly,
  all 18 of those marked `high`.
- **Link pass, after review** (**extractor version 7**): a dotted receiver
  whose first segment is a variable is typed through its fields, never taken
  for a package; a call after an external one is no longer external by
  default (`name_only` when the repository has the name, `chain_external`,
  `medium`, otherwise); lambda parameters and pattern variables no longer take
  the type of a field of their name; confidence never escalates along a chain;
  an anonymous class scopes its calls; constructor injection keeps the field's
  declared type. The incremental pass now matches a full one (tested), a pass
  cut short is redone by the next run, a change of link rules relinks the
  branch, and dropped calls are kept (`resolution = 'discarded'`) so a later
  definition reopens them. Measured on two Java repositories: 8.1 % and 4.2 %
  of calls undecided, 20 of 25 hand-labelled sites correct, all 19 `high`
  ones right.
- **Extractor version 8, link rules version 2:** `@lombok.Data` is recognised
  like `@Data`; discarded calls wait for their name instead of being
  re-resolved on every run, and readers get them filtered out through the
  `live_edges` view; a call in an anonymous class that only the enclosing
  class defines resolves to it (`medium`); `Config.INSTANCE.m()` is typed.
  Link rules version 3: a hint's names are read by position, so a callee
  called `name()` still reopens the edges after it.
- **TypeScript/JavaScript resolution** (PLAN-009 TASK-006, **extractor
  version 9**, link rules version 4): calls, type uses and imports resolve
  through what the file imports — relative paths (`.js` written for `.ts`,
  directory `index` files), the `paths` aliases and `baseUrl` of the root
  `tsconfig.base.json`/`tsconfig.json` (comments, trailing commas, one
  relative `extends`), barrels re-exporting up to four levels deep — and npm
  packages are external only when the repository does not define the imported
  name. Angular DI types receivers: constructor parameter properties and
  `inject(T)` (in a field or a local) are `ctor_inject`. TS/JS get lexical
  scopes: parameters, destructured names and `catch` variables shadow fields,
  a bare name is never a member, `this` outside a declared class (an object
  literal's method, a class expression) is untyped, and the declared return
  type of a function types the next call of a chain. A changed `tsconfig`
  relinks the branch; the incremental pass re-resolves the files importing a
  written one, through barrels. Measured on three libraries of an Angular/Nx
  workspace: 10.8 % of calls undecided (was 73.1 %), 20 of 20 hand-labelled
  call sites correct, all `high`.
- **TypeScript/JavaScript, after review** (**extractor version 10**, link rules
  version 5): a package is external only with evidence — a dependency of the
  root or nearest `package.json`, or a Node builtin — so an unmapped alias or
  an undeclared module stays undecided; a call to a name imported from where
  nothing can follow is undecided rather than matched to a repository
  homonym; only Angular's `inject` injects; a function bound in a sibling
  block no longer shadows. An unreadable `tsconfig.base.json` or
  `package.json` (a merge conflict) no longer relinks the branch without its
  aliases: the last good environment is kept; a byte-order mark and an
  `extends` array are read. The incremental pass reopens a TS import row by
  its imported name and follows an importer's return types, and falls back to
  the full pass when the importers exceed a fifth of the branch.
- **TypeScript/JavaScript, second review** (**extractor version 11**, link
  rules version 6): an unreadable `tsconfig` or `package.json` now falls back
  per file, so a broken template manifest no longer freezes alias and
  dependency changes; an import from a declared package is external even when
  the repository defines the same name; manifests under `dist/`, `vendor/`
  and other excluded directories are not workspace packages. A receiver bound
  to a function or class (`fn.call()`, `Klass.make()` with `const Klass =
  class {…}`) is read as that name: `fn.call()` is now undecided instead of
  discarded, and `Klass.make()` resolves to the method. Two same-named
  functions in sibling blocks resolve at `medium`.
- **Link environment** (PLAN-009 TASK-007): besides `tsconfig` and
  `package.json`, every `Cargo.toml`, `go.mod`, `pyproject.toml`, `setup.cfg`
  and `requirements*.txt` of the branch is read; changing one relinks the
  branch in full, an unreadable one keeps its last good version (per file)
  and relinks nothing. Manifests under `target/`, `vendor/`, `dist/`, `.venv/`
  and similar are ignored.
- **Python resolution** (PLAN-009 TASK-007, **extractor version 12**, link
  rules version 7): modules by path and package (`__init__.py`, the
  repository root, `src/`, a script's own directory), relative imports,
  re-exports through a package or any module, `import *`; a call to a class
  is its instantiation; `self.x = …` in a method types the attribute
  (`ctor_inject` from a typed parameter of `__init__`); `x = Foo()` and
  `x = make()` type `x` by the class or the return annotation. Parameters,
  lambda and comprehension variables and loop targets shadow attributes.
  External only with evidence: the standard library, builtins, or a
  distribution a manifest declares (also when the repository has a module of
  that name); a name Python cannot see, or an import that cannot be followed,
  is undecided — never a guess by name, so some calls that used to resolve by
  their unique name are now undecided. Calls on untyped receivers to names the
  repository lacks are discarded (more of them, now that parameters no longer
  borrow an attribute's type). Measured on a small worker repository: 1.6 % of
  calls undecided (was 49.2 %), 24 of 25 hand-labelled call sites correct, all
  of them `high`.
- **Rust resolution** (PLAN-009 TASK-007, **extractor version 13**, link
  rules version 8): crates by their `Cargo.toml`, modules by path and inline
  `mod`, `use` (groups, `self`, aliases, globs) scoped to its module,
  `crate::`/`self::`/`super::`/`Self::`, `pub use` re-exports; a type's
  `impl`s in any file of its crate; `impl Trait for T`; fields typed by their
  declaration (`self.store.open()`), `let x = T::new()?` by the return type;
  trait methods without a body are symbols. External: `std`/`core`/`alloc`,
  the prelude, and the crates a manifest declares; a missing derivable method
  (`clone`, `default`…) is external at `medium`. Macros are not calls. A path
  call's target drops generic arguments (`Vec::<u8>::new` → `Vec.new`).
  Measured on this repository: 19.8 % of Rust calls undecided (was 54.1 %),
  21 of 21 hand-labelled call sites correct, all `high`.
- **Go resolution** (PLAN-009 TASK-007, **extractor version 14**, link rules
  version 9): an import under the `module` of `go.mod` (or a local
  `replace`) is the repository's, the standard library and other modules are
  external; a method's receiver is `self` (also for a method in another file of
  the package) and `s.m()` writes `Type.m` to `graph_edges`; fields are typed
  by their declaration, `x := New()` by the declared result; interface methods
  are symbols. Covered by fixtures only (no Go repository was measured).
- **Python, Rust and Go, after review** (PLAN-009 TASK-007, **extractor
  version 16**, link rules version 13): a Rust workspace crate counts only as
  the file's own or a path dependency of its `Cargo.toml` (a `package =`
  rename follows the path; `workspace = true` follows the root's entry), so a
  registry dependency named like a workspace crate is external and an
  undeclared one undecided; every bound of a type parameter is tried; an
  inherent method wins over a trait impl's, and several candidates (two
  `From` impls, two `#[cfg]` definitions, Go build-tag files) are `medium`; a
  `use` inside a function is that function's; a missing method of a type with
  an external supertype is external at `medium` in Rust and Go; `let x =
  a.f()` makes `x.g()` a chain typed by `f`'s return type. Go needs a
  `go.mod` above a file before an import outside its module counts as
  external, and names an import by its package clause. Python: `Any`,
  `object`, a `TypeVar` and a multi-member union type nothing, `type[X]` is
  `X`, `Self` is the class; only an exactly declared distribution name beats
  a repository module of that name. A barrel or star import with a branch
  that cannot be followed no longer counts as external. Manifests: nested
  multi-line TOML arrays, `requirements/*.txt`, `name @ url`, Poetry
  dependency tables. A readable `tsconfig.json` beside a broken
  `tsconfig.base.json` is used fresh; a broken `extends` base keeps the last
  good aliases. The incremental pass reopens fewer Rust edges (a widely
  imported file: 24 307 → 18 179) and follows a Python package's
  `__init__.py` through `import pkg.sub`. Measured: 17.5 % of Rust calls
  undecided (was 19.8 %), 26 of 26 hand-labelled Rust sites and 26 of 27
  Python sites correct.
- **Lookups by symbol** (PLAN-009 TASK-008): on a branch with a current symbol
  graph, `read_symbol`, `get_references`, the identifier anchoring of `search`
  and the web graph view read the `symbols` table and the resolved edges by id
  instead of matching names and suffixes. Contracts only grow: definitions add
  `sym` (16 hex digits), `kind` and `qualified`; anchored hits add `sym`;
  references add `confidence`, `via` (the relation) and `sym`, plus `external`,
  `test` and `undecided` when true. `get_references` lists one reference per
  occurrence (two calls from one method are two) of calls and instantiations,
  as 0.9.0 listed calls; **`kinds`** adds type uses, imports and
  inherits/implements (or `all`) to them — the calls always stay. It takes **`min_confidence`** (`high`,
  `medium` — default —, `low`): what the default leaves out is counted in
  `below_confidence`. A server older than 0.10 ignores both, and the client
  says so in `warning`.
  `external` comes from the link pass's evidence, and a qualified name falls
  back to its last segment only when the repository defines nothing by that
  name (`Foo::new` that does not exist is no longer external because some
  `new` is called; it gets `suggestions`). The graph view marks a node
  external by that evidence (not "never calls anything") and an edge from a
  test file `test`. A call the link pass discarded reaches no answer. An
  external answers no `suggestions`, as in 0.9.0, and a `file::name` miss is
  never external. An index from an older extractor, or a branch with no
  symbol rows, answers as 0.9.0 did and says so in `warning` (`search`
  included, whose identifier anchoring then goes by name); a failed read of
  the symbol table has its own warning, not a reindex hint. Anchoring reads
  the code of every definition of an identifier in one query.
- **`impact_analysis` walks the symbol graph by levels** (PLAN-009 TASK-009):
  one query per depth and direction over ids (through `live_edges`: a discarded
  call reaches no answer), whatever the size of the frontier; the frontier goes
  as an `IN` list up to 1 024 ids and as one `UBIGINT[]` parameter above.
  **A bare name means the repository's definitions of it**, as in
  `get_references` (0.9 merged every call written with that name, libraries'
  included); a name with no definition stands for its external call sites (its
  upstream is their callers) and answers `external` + `called_from`, or
  `suggestions`, as `read_symbol` does. **New defaults** (the answer an agent
  sees changes): only `high`/`medium` calls are followed, symbols of test files
  and library callees are left out, and 200 symbols per direction are listed;
  what that leaves out is counted (`below_confidence` — calls to the name the
  index could not decide included —, `excluded`, `omitted` +
  `omitted_by_limit`) and not walked, and `min_confidence`, `include_tests`,
  `include_external` and `max_nodes` (tool, `GET /impact` query, `devctx
  impact` flags) change it; `filters` says which applied. Each symbol adds
  `confidence`, `via`, `sym`, `file`, `line` and `test`/`external`/`undecided`
  when true, ordered within a depth by confidence, rank and name. Two causes of
  the speed-up, measured apart on this repository for a bare `new`: 0.9 walked
  1 853 names, one query per name, in 1.4 s p50; the new walk with every filter
  and the cap off reaches 547 symbols (the repository's definitions of `new`, by
  id) in about 50 ms — the batching —, and the defaults list 252 (56 more counted
  past the cap) in about 57 ms — the meaning of a bare name and the defaults. The
  0.9 fields keep their meaning; an index from an older extractor, or without
  symbol rows, walks as 0.9 did and says so in `warning`, and a client that
  passed filters to a server that ignored them says so too. The TUI's local view
  gives the tool's answer.
- **Link rules version 19:** in a Rust `impl Trait for T` with `T` from
  outside the repository, `self.m()` resolves to the `impl`'s own method
  (an override), else to the repository trait's method (a default one
  included) at `medium` — an inherent method of the outside type would win
  and cannot be known — instead of staying undecided.
- **Link rules version 20:** in that same `impl Trait for T` with `T` from
  outside, the `impl`'s own method is `medium` too, no longer `high`: an
  inherent method of `T` of that name wins in Rust (`self.len()` in an
  `impl Counted for String` calls `String::len`).

### Fixed

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
- Opening an index that has an HNSW index loads VSS *before* creating tables
  added by a release: the checkpoint after the DDL failed fatally ("unknown
  index type 'HNSW'") and invalidated the database for the process. Where VSS
  cannot be loaded at all (offline), every checkpoint over such an index is
  skipped instead, leaving the write-ahead log for an open that can.

### Known issues

- 0.8.3 – 0.9.0: the first open of an HNSW database created with 0.8.2 or
  earlier fails once with "unknown index type 'HNSW'"; retrying resolves it.
  Fixed in this release (see above); there is no 0.9.1.

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
