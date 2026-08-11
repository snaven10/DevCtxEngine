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

## 1. The post-commit hook

The cheapest automation that works. It fires exactly when the commit diff has
something new to look at, needs no process running, and costs nothing when idle.

```bash
devctx hooks install
devctx hooks status
devctx hooks uninstall
```

The body is written between markers, so an existing `post-commit` hook is
extended rather than replaced, and removing ours leaves yours untouched:

```sh
#!/bin/sh
make lint                       # yours, kept

# >>> devctx (managed) >>>
("/home/you/.local/bin/devctx" index >/dev/null 2>&1 &) || true
# <<< devctx (managed) <<<
```

It is detached and `|| true`: a commit must never wait on indexing, and must
never fail because of it. Re-running `install` refreshes the block in place.

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

Adding an exclude **prunes what it now covers** on the next full pass, so the
config is the whole truth rather than only applying to files seen later. A
malformed pattern is dropped rather than failing the run.

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
| `devctx index` | commit diff since the last indexed commit, plus untracked files | no |
| `devctx index --full` | the whole work tree | yes — files gone, or newly excluded |
| explicit paths (`watch`) | exactly the files named | only those, when deleted |

An explicit-path run deliberately does **not** advance the recorded commit: it
covered uncommitted work, so moving the marker would make the next incremental
diff skip past commits whose other files were never looked at.

Deletions of untracked files are not noticed incrementally — the commit diff
cannot see them. A full pass cleans up.
