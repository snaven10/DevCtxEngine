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
- **The type of an `impl` is named as its definition is** (`impl<T> Foo<T>`,
  `impl Display for &'a Foo`, `impl crate::x::Foo` → `Foo`). This changes
  content, not format: the `graph_edges` source of such a method is now
  `Foo.get` (was `Foo<T>.get`), and its chunks' context header changes with
  it, so the first `--full` after upgrading re-embeds those chunks instead of
  reusing them (measured on this repository: 40 of 4928 chunks, 143 of 16 123
  `graph_edges` rows).

### Fixed

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
