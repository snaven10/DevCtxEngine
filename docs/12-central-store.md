# The Central Store

> 🇪🇸 [Leer en español](es/12-store-central.md)

One registry of every project DevCtxEngine tracks, plus the memory worth carrying
between them. This is what lets an agent working in one repository know the
others exist — and recall what was learned there.

---

## 1. What lives where

Each project keeps its own database: vectors, call graph, routes and its own
memories. Only what has no single owner moves to the central store.

| | Project store | Central store |
|---|---|---|
| Code vectors | yes | no |
| Call graph and routes | yes | no |
| `local` memories | yes | no |
| `global` memories | copy, for working offline | **source of truth** |
| Project registry | no | yes |
| Project config | source of truth (`.devctx/config.yaml`) | cached copy |

Keeping vectors per project is deliberate. Re-indexing one repository never
touches or locks another, each may use a different embedding model without
corrupting anything, and no search needs a repo filter to avoid drowning in
results from elsewhere.

```
   repo-a/            repo-b/            repo-c/
   .devctx/state/     .devctx/state/     .devctx/state/
   index.duckdb       index.duckdb       index.duckdb
        |                  |                  |
   devctx serve       devctx serve       devctx serve
        |                  |                  |
        +---------+--------+---------+--------+
                  |
        devctx serve --central          <- single writer
                  |
        ~/.local/share/devctx/central.duckdb
          projects + global memories
```

## 2. Locations

| What | Default | Override |
|---|---|---|
| Central database | `~/.local/share/devctx/central.duckdb` | `DEVCTX_HOME` |
| Central config | `~/.config/devctx/config.yaml` | `DEVCTX_HOME` |
| Downloaded models | `~/.local/share/devctx/models` | `DEVCTX_MODEL_CACHE` |

`DEVCTX_HOME` collapses config and data under one directory — how tests and CI
stay off your real directories. `$XDG_DATA_HOME` / `$XDG_CONFIG_HOME` are
honoured when set.

The model cache is shared on purpose: the files are identical whoever asks for
them and run to hundreds of megabytes, so one copy is downloaded and reused
everywhere.

## 3. The registry

```bash
devctx projects add .                    # register the current repository
devctx projects add ~/code/api --init    # create its config from central defaults
devctx projects list                     # name · model · freshness · path
devctx projects show api                 # everything recorded about one
devctx projects refresh api              # re-read its .devctx/config.yaml
devctx projects rm api --deactivate      # hide it, keeping its history
```

`devctx init` registers the repository it initializes, so `projects add` is only
needed for repositories initialized before the registry existed.

Each row records where the repository is, which embedding model it uses, its
description and how fresh its index is. Indexing updates the freshness itself.

**Names are unique.** Registering a second repository under a name already taken
is refused rather than silently repointing it; pass `--name` to choose another.
Re-registering the same path updates the existing row instead of duplicating it,
preserving its registration time and index statistics.

## 4. Global memories

A memory is either `local` — this project only — or `global`, shared with every
project on the machine.

```bash
devctx remember "always verify webhook signatures" --type insight --scope global
devctx recall "how do I validate a webhook"          # both scopes (default)
devctx recall "..." --scope global                   # only the shared ones
devctx recall "..." --scope global --repo api        # only what `api` contributed
```

Two properties are worth knowing about.

**A lesson converges.** Global memories are keyed by content, not by the project
that contributed them, so the same insight saved from two repositories becomes
one memory with a duplicate count — not two rows. The contributing repository is
kept as provenance and is what `--repo` filters on.

**Local memories never leave.** Nothing marked `local` is visible from another
project. That is the privacy model, and it is why the default for `remember` is
`local`: sharing is a decision you make explicitly.

Results from the two stores are fused by **rank**, never by score. A project may
embed with a different model than the central store, so their similarity numbers
are not on comparable scales; position is the one thing they agree on, and a
memory surfacing in both lists is rewarded for it.

## 5. The daemon

DuckDB permits one writing process per file. Project databases are never shared,
so their servers never contend — but the central store is shared by all of them,
and two processes opening it concurrently does not degrade, it fails:

```
$ devctx projects add ./a & devctx projects add ./b
Error: opening the central store
```

`devctx serve --central` is the single writer. Everything else reaches the
central store through it.

```bash
devctx serve --central              # foreground, on a port derived from DEVCTX_HOME
devctx serve --central --stop       # stop it
```

You rarely start it by hand: any command that needs the central store spawns one
in the background and it idles out after 15 minutes. `DEVCTX_NO_AUTOSERVE=1`
disables that, in which case a lone command opens the store directly — correct
when nothing else is running, and the reason a single `projects list` still works
with no daemon at all.

Unlike a project server it loads no model, so startup is a database open and
nothing more.

## 6. Configuration

`~/.config/devctx/config.yaml`, written with defaults on first run:

```yaml
memory:
  provider: local
  model: minilm-l6       # pins the global vector space
defaults:                # inherited by `projects add --init`
  embeddings:
    provider: local
    model: minilm-l6
  reranking:
    enabled: true
    model: bge-base
reindex:
  every_seconds: 0       # 0 = off; see §7
```

`memory.model` is a constraint, not a default. Every global memory lives in one
vector space, so it cannot vary per project — and changing it after the store
exists is refused at open time rather than corrupting it:

```
central store at ~/.local/share/devctx/central.duckdb holds 384-dimensional
vectors but `memory.model` resolves to 768; changing the central memory model
requires re-creating the store
```

When a project's embedding model matches `memory.model` — the common case — the
already-loaded model is reused and global memory costs no extra memory.

## 7. Background reindex

The daemon can refresh registered projects on a timer:

```yaml
reindex:
  every_seconds: 900
```

A sweep compares `git rev-parse HEAD` against each project's recorded commit
without opening any database, so projects with nothing to do cost nothing. It is
**off by default**: silently indexing every repository you have ever registered
is surprising, and expensive on a laptop.

See [Keeping the index fresh](13-keeping-the-index-fresh.md) for the per-repo
alternatives (`hooks`, `watch`) which most people want first.

## 8. Reaching it from an agent

Over MCP, three tools cover this:

| Tool | Use |
|---|---|
| `list_projects` | Discover which repositories exist, and how fresh each index is |
| `recall` | `scope: local \| global \| all`, `repo:` to narrow; hits are tagged with their scope |
| `search_project` | Search another repository's code by name |

`search_project` federates: it wakes exactly the one server you named. Memory
recall does not — global memories are already in the central store, so a
cross-project question is one local query rather than N cold starts.

---

## Transition from the pre-Rust layout

Earlier versions shared *everything* by pointing every repository at one store
via `DEVAI_STATE_DIR`. That is not the model any more and should not be
recreated: it forced one embedding model across all repositories, made every
re-index contend on one file, and left every search needing a repo filter to be
readable.

If you have such a setup, register the repositories instead (`devctx projects
add`) and let each keep its own index. `DEVCTX_HOME` remains for relocating
DevCtxEngine's own directories, which is a different thing.
