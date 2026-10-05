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
  than this binary (or by one that left no record). The graph tools add a
  `warning` to every answer and the fix is `devctx index --full`; incremental
  runs never clear it. **After upgrading from 0.8.2 or earlier every index is in
  this state once**, and until you reindex, a *new* branch re-embeds all of its
  files instead of copying them from one already indexed — correct, but slow.

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
