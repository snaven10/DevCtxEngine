# Keeping the Index Fresh

> 🇪🇸 [Leer en español](es/13-mantener-el-indice-al-dia.md)

Four ways to avoid running `devctx index` by hand, in the order most people want
them.

---

## The rule

**The index mirrors the work tree, minus what git ignores.** Not the last commit
— the work tree. A file you have written but not `git add`ed is exactly the code
you are most likely to ask about, so it is indexed like any other.

Two consequences worth internalising:

- `index --full` does not throw away uncommitted work.
- Anything git ignores never reaches the index, so `.gitignore` is the first
  place to control what gets indexed.

## 1. The git hooks

The cheapest automation that works. They fire exactly when the diff has
something new to look at, need no process running, and cost nothing when idle.

```bash
devctx hooks install
devctx hooks status
devctx hooks uninstall
```

**Two hooks are installed, and the second one is the one people miss:**

| Hook | Fires on |
|---|---|
| `post-commit` | your own commits |
| `post-merge` | merges **and fast-forward pulls** |

`post-commit` does not run on a merge or on a `git pull` — git uses `post-merge`
for both. Installing only `post-commit` leaves the index stale exactly after you
merge a PR or pull someone else's work, which is when it is most likely to be
asked a question it can no longer answer correctly.

Still uncovered: rebase, checkout and reset. Those are rare enough that
re-indexing by hand is the honest answer, rather than a third and fourth hook.
`devctx hooks status` says so.

The body is written between markers, so an existing hook is extended rather
than replaced, and removing ours leaves yours untouched:

```sh
#!/bin/sh
make lint                       # yours, kept

# >>> devctx (managed) >>>
(unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX …; "/home/you/.local/bin/devctx" index >/dev/null 2>&1 &) || true
# <<< devctx (managed) <<<
```

(The real line lists all twelve variables that select a repository.) It is
detached and `|| true`: git must never wait on indexing, and must never fail
because of it. Re-running `install` refreshes the block in place — which is also
how an older install that predates `post-merge`, or the `unset`, picks it up.

### The environment git hands to a hook

Git runs hooks with `GIT_DIR`, `GIT_INDEX_FILE` and friends pointing at the
repository — or *linked worktree* — that is committing. `devctx index` auto-starts
a server, and a server that inherits those variables keeps using them for its
whole life: delete the worktree and every git call in that server fails with
`not a git repository: …/.git/worktrees/…`. So there are two layers:

1. **The hook** `unset`s the repository-selecting variables inside its subshell
   (`unset` is a POSIX builtin, so it does not depend on an `env` binary and
   cannot fail the commit).
2. **devctx itself** strips them: every `git` subprocess and every re-execution
   of the `devctx` binary removes all `GIT_*` from its environment except
   transport and diagnostics (`GIT_SSH*`, `GIT_TERMINAL_PROMPT`, `GIT_ASKPASS`,
   `GIT_EXEC_PATH`, `GIT_CONFIG_GLOBAL/SYSTEM/NOSYSTEM`, `GIT_TRACE*`).

**Hooks installed by an older version keep the old block** until you run
`devctx hooks install` again in each repository; layer 2 protects them meanwhile.
There is no automatic migration. A side effect worth knowing: because devctx
discards `GIT_DIR` / `GIT_WORK_TREE`, setups that rely on them (a bare dotfiles
repository, say) are not honoured by devctx's subprocesses — each repository is
located from the directory it is given.

### From a linked worktree

A `post-commit` fired in a linked `git worktree` runs `devctx index` there, and
devctx resolves a linked worktree to its **main** worktree's project
(`--git-common-dir`). So it indexes the main worktree's `HEAD`, not the commit you
just made in the linked one. The linked worktree's commit reaches the index when
it lands on a tracked branch (see `indexing.branches`).

Uninstall handles each hook independently: a `post-commit` that also runs your
linter keeps the linter, while a `post-merge` that was ours alone is deleted.

## 2. `devctx watch`

Covers the one window the hook cannot — work written but not committed.

```bash
devctx watch                  # until interrupted
devctx watch --debounce 5     # seconds to wait after the last change
```

Saves are coalesced before indexing: editors write in bursts (format on save,
then the write, then a temp-file rename) and a build touches hundreds of files at
once. Three seconds by default.

What it ignores: everything in `.gitignore`, everything in `indexing.exclude`,
DevCtxEngine's own directories, and the temporary files editors leave behind
mid-save (`~`, `.swp`, `.#foo`, `foo.rs___jb_tmp___`).

**Known limits.**

- A `git checkout` fires thousands of events at once against a different
  branch's index state. Stop the watcher across branch switches for now.
- On Linux each watched directory costs an inotify watch. A large repository can
  exhaust the per-user cap; the error says how to raise it.

## 3. `devctx reindex`

Drive the registry rather than one repository:

```bash
devctx reindex                       # this project
devctx reindex --all                 # every active registered project
devctx reindex --project api --project web
devctx reindex --all --full
```

Each project is indexed through its own server, so this never takes a second lock
on a database another process owns. One project failing does not stop the rest;
the failures are collected and reported at the end.

## 4. The central scheduler

For repositories you are not currently sitting in. See
[The Central Store §7](12-central-store.md#7-background-reindex). Off by default.

---

## Controlling what gets indexed

Beyond `.gitignore`, the project config takes patterns for code git *does* track
but that is not worth searching:

```yaml
# .devctx/config.yaml
indexing:
  exclude:
    - vendor/
    - "*.generated.rs"
    - docs/third-party/**
```

These are `.gitignore` patterns, not literal globs — `vendor/` covers everything
beneath it, `*.generated.rs` matches at any depth — so a pattern behaves the same
here as it would there. They apply identically however a file arrives: `index`,
the hook, `watch`, or an explicit path list.

Adding an exclude **prunes what it now covers**, so the config is the whole
truth rather than only applying to files seen later. A malformed pattern is
dropped rather than failing the run.

### Excluded by default

Since 0.9, even without a line of yours, these are kept out of the index
although git may track them: `node_modules/`, `dist/`, `vendor/`,
`third_party/`, `bower_components/`, `*.min.js`, `*.generated.*` (any depth) and
`/build/`, `/target/` (repository root only, so `src/x/build/Builder.java` is
still code). Turn them off with `indexing.default_excludes: false`, or bring back
only `/build/` with `indexing.include_build: true` — see
[Configuration](11-configuration.md#default-excludes). Your own `exclude` list is
applied after them, so `"!vendor/"` re-includes.

### When the exclude set changes: reconcile

The index remembers a fingerprint of the effective exclude set. When it differs —
you edited `exclude`, flipped an option, or upgraded to a release with new
defaults — the next `devctx index` is still **incremental** and reconciles:

- tracked-and-indexed files the set now excludes are removed (`files_pruned`);
- tracked files it no longer excludes, and that are not in the index, are added
  and embedded;
- nothing else is re-embedded, and the log says how many files each pattern
  excluded: `the exclude set changed: 4 indexed file(s) are now excluded (/build/: 3, *.min.js: 1)`.

The new fingerprint is recorded only when the reconcile finishes, so a cancelled
one is simply redone. Two limits: a run on an explicit path list (hook, `watch`)
neither reconciles nor stamps; and the reconcile acts on the branch being
indexed, so a *fallback* branch keeps showing newly excluded files until that
branch is reindexed. Files git tracks but the indexer never takes (binaries,
languages without a parser, oversized files) are re-skipped on each reconcile
and show up in `files_skipped`.

## What a stale index looks like

Two honest signals, both on `index_status`:

- `up_to_date: false` — the recorded commit is not `HEAD`.
- `extractor_stale: true` — the index was produced by an older parser/extractor
  than this binary (or by one that left no record), or its symbol graph is out
  of step with its files (below). The graph tools add a `warning` to every
  answer and the fix is `devctx index --full`; incremental runs never clear it.
  **After upgrading from 0.8.2 or earlier, and again after upgrading to 0.10,
  every index is in this state once**, and until you reindex, a *new* branch
  re-embeds all of its files instead of copying them from one already indexed —
  correct, but slow.

`extractor_stale` is per branch, and it is the sum of two checks:

- **The stamp.** Every `--full` run records, in `index_meta`, the extractor
  that built the branch: `EXTRACTOR_VERSION` (21 in 0.10) and a hash of the
  language definitions. A different stamp, or none, is stale.
- **`graph_out_of_step`.** The stamp alone can lie: a binary that does not
  write the symbol graph (0.9.0 after a downgrade) can index incrementally
  under a 0.10 stamp. So the branch is also stale when a file of a parsed
  language has no file symbol (added by such a binary), when a file symbol's
  file is gone (deleted by it), or when a file symbol's content hash is not
  the file's (modified by it). A branch in that state is never a source for the
  cross-branch copy. The answer is cached per branch until something writes
  it, since every graph tool asks.

The link rules have a version of their own (`LINK_VERSION`, 21 in 0.10): when it
changes, the next `devctx index` — incremental is enough — relinks and re-ranks
the branch, with nothing re-parsed or re-embedded, and nothing reports stale.

## Upgrading to 0.10: the symbol graph

0.10 indexes a symbol graph — symbols with stable ids, seven kinds of
relations, a link pass that resolves them — that no 0.9 index has (see
[Symbol Graph](03-core-concepts/symbol-graph.md#what-is-in-the-graph)). So after
the upgrade every indexed branch reports `extractor_stale: true` until it is
rebuilt once.

### What to run

```bash
devctx serve --stop                    # a running server keeps the old code
devctx index --full                    # this repository: the branch it indexes by default
devctx index --full --branch release   # each other branch you query
devctx reindex --all --full            # or every registered project (its default branch)
```

- **Per repository and per branch.** The stamp is per branch, and a run
  re-stamps only the branch it indexes. A branch you never rebuild keeps
  answering the 0.9 way. Once one branch is rebuilt, the others copy the files
  they share with it instead of parsing them again.
- **Only `--full` clears it.** Hooks, `watch` and a plain `devctx index` write
  the new tables for the files they touch, but never re-stamp the branch.
- **Restart your AI sessions** after installing the binary: an MCP keeps the
  code it started with (`index_status` says `mcp.binary_replaced`).

### What it costs

The full run parses every file again and runs the link pass, but **reuses the
stored vector of every chunk whose text did not change** (same content hash,
same embedding setup), so it does not pay the embedder again; when no chunk
needs embedding it does not even load the model. A few chunks do change text in
0.10 and are re-embedded: constructors become chunks of their own, a class's
list of methods gains them, and the context header of a generic `impl`'s
methods loses its type arguments (`Cache<T>` → `Cache`).

Measured in a sandbox, release build, `minilm-l6`, while the extractor was
being built — so each row is one step of it, not 0.9.0 → 0.10.0 in one go:

| Date | Repository | From scratch | `--full` after the extractor changed | Chunks reused |
|---|---|---|---|---|
| 2026-10-06 | this one, 281 files, 4 928 chunks | 278 s | 54.9 s (19.7 %) | all |
| 2026-10-06 | this one, over an index with 0.9.0's extraction | 345 s | 31.4 s | 4 888 of 4 928 (40 re-embedded) |
| 2026-10-06 | a Java repository, 216 files, 3 253 chunks | 442 s | 36.9 s (8.3 %) | all |
| 2026-10-07 | the same Java repository, with the 7 relations | — | 24.8 s | 3 204 of 3 284 |
| 2026-10-06 | a `--full` with nothing changed: this one / the Java one | 240 s / 169 s | 4.4 s / 2.4 s (1.8 % / 1.4 %) | all, model not loaded |

The link pass and the ranks on top of that: 0.2–0.4 s on this repository, and
on a 1 250-file Java repository 1.9–2.6 s with the machine quiet and up to
5.6 s under load (the ranks are 69–165 ms of it), measured 2026-10-07 to
2026-10-09; it holds the branch's symbols in memory while it runs (+114 MiB
peak on that Java repository).

The database grows. With the full 0.10 graph, a 216-file Java repository went
from 79 to 112 used blocks of 256 KiB, plus 66 free (2026-10-07); a rewrite
leaves blocks free that DuckDB does not reuse on its own in this pattern, so
the file does not shrink back.

The upgrade of a real 0.9.0 index with the final 0.10.0 binary is measured in
the field verification before the release (PLAN-009 TASK-016); its figures will
be added here.

### What the tools answer meanwhile

Nothing breaks while a branch waits for its `--full`:

| Tool | Answers |
|---|---|
| `index_status` | `extractor_stale: true` and a `hint` that says to run `devctx index --full` |
| `read_symbol`, `get_references`, `search` (`keyword`/`hybrid`), the web graph view | As 0.9 did, by name over `graph_edges`, with a `warning` |
| `impact_analysis` | The 0.9 walk — by name, no confidence, no filters, no cap — with the `warning`, which adds, when filters were passed, that they were not applied |
| `build_context` with `related: true` | No section, and `[devctx] related: not shown: …` |
| `traverse`, `repo_map`, `skeleton` | **An error**: `… reads the symbol graph, and this branch's index predates it (an older extractor, or no symbol rows): reindex with devctx index --full (vectors are reused)`. They have no 0.9 answer to fall back to |
| hybrid search with `search.centrality_weight` > 0 | Vector + keyword only |
| a new branch indexed from a stale one | Re-embeds its files instead of copying them |

A branch indexed by 0.10 whose language has no symbol extraction (only text
files) has no symbol rows either: the old tools answer the 0.9 way with a
`warning` of their own, and `skeleton` sends to `read_file` instead of asking
for a reindex that would change nothing.

### Going back to 0.9.0

Install the 0.9.0 binary, run `devctx serve --stop` and restart your AI
sessions. Then:

- The 0.10 tables (`symbols`, `edges`, the `live_edges` view, extra `index_meta`
  keys) stay in the database, unread and harmless. Nothing is migrated or
  rewritten.
- `graph_edges` was kept up to date in its 0.9 format, so 0.9.0's
  `read_symbol`, `get_references` and `impact_analysis` answer from it. Checked
  with a build whose read path is 0.9.0's — the same `impact` answer, byte for
  byte, as the 0.10 binary on the same database (2026-10-06); the field
  verification repeats it with the released binary.
- 0.9.0 sees a stamp it did not write and reports `extractor_stale` with its own
  warning. It works anyway; its `--full` would re-embed every chunk (0.9.0 does
  not reuse vectors), and is not needed for it to answer.
- **Coming back to 0.10** after 0.9.0 indexed files: those files have no symbol
  rows, so `graph_out_of_step` marks the branch stale and asks for `devctx index
  --full` again — with vectors reused.
- The file keeps its size: free blocks are not reclaimed.

## What is never indexed

DevCtxEngine's own working directories are skipped whatever their git status:
`.devctx/` (state and config) and the legacy `.fastembed_cache/`. Without that
guard a full re-index would swallow its own database and the downloaded model
cache, and then answer questions with them.

Models now live outside any repository — see
[The Central Store §2](12-central-store.md#2-locations). A `.fastembed_cache/`
left in an old checkout is unused and can be deleted.

## Incremental, full, and explicit paths

| Run | Selects | Prunes |
|---|---|---|
| `devctx index` | commit diff since the last indexed commit, plus untracked files | no — except it reconciles a changed exclude set |
| `devctx index --full` | the whole work tree | yes — files gone, or newly excluded |
| explicit paths (`watch`) | exactly the files named | only those, when deleted |

An explicit-path run deliberately does **not** advance the recorded commit: it
covered uncommitted work, so moving the marker would make the next incremental
diff skip past commits whose other files were never looked at.

Deletions of untracked files are not noticed incrementally — the commit diff
cannot see them. A full pass cleans up.
