# Design Decisions

> 🇪🇸 [Leer en español](es/08-decisiones-de-diseno.md)

Why the system is shaped the way it is. Each entry states the decision, the
reasoning, and what it costs — decisions without costs are advertising.

---

## ADR-01: One Rust binary, no sidecar

**Decision.** Parsing, chunking, embedding, reranking, storage and the MCP
server all live in one process. The only external program invoked is `git`.

**Why.** The predecessor split the work across two runtimes joined by a JSON-RPC
bridge over stdio. That bought language-native ML libraries and cost a process
lifecycle to manage, a serialization boundary on every call, ~880 MB resident
per client because the sidecar could not be shared, and a class of failure where
one half was alive and the other was not.

**Cost.** The Rust ML ecosystem is narrower. Some models exist in Python and not
here, and ONNX export is sometimes the only path.

## ADR-02: MCP over stdio, no network service

**Decision.** The MCP server speaks JSON-RPC 2.0 over stdin/stdout. Everything
else that listens — `api`, `web` — binds to loopback.

**Why.** The client already manages the child process lifecycle, so stdio gets
process isolation and cleanup for free. There is no port to collide, no
credential to store, and no authentication to get wrong: the trust boundary is
the process boundary.

**Cost.** One server per client. Remote use is not supported.

## ADR-03: DuckDB for everything

**Decision.** One embedded database holds vectors, full-text index, symbol
graph, routes and memories. No separate vector store.

**Why.** The alternative — a vector store beside a relational one — means two
lifecycles, two backup stories, and no way to filter vectors by a relational
predicate without pulling both sides into memory. DuckDB does vector search
(VSS/HNSW), BM25 (FTS) and ordinary SQL over the same rows, so a filtered
semantic search is one query.

**Cost.** DuckDB allows **one writer per file**. This constraint shapes ADR-04.

## ADR-04: A long-lived server owns the database

**Decision.** `devctx serve` holds the connection; CLI commands and MCP sessions
route to it rather than opening the file themselves.

**Why.** Direct consequence of ADR-03. Without an owner, an indexing run and a
search contend for the same lock and one of them fails.

**Cost.** A process to supervise. It is spawned on demand and idles out, and the
`serve.json` handshake file has to be written and removed carefully — a server
that deletes the file on its way out can strand a healthy one, which is a bug
this project has actually shipped and fixed.

## ADR-05: The WAL must not outlive the process that wrote it

**Decision.** Every path that ends the server checkpoints first, and so does the
end of an indexing run.

**Why.** This is the sharpest edge in the system. DuckDB replays the WAL on
open, but **a replayed append does not restore the entries of an ART index** —
the structure behind every `PRIMARY KEY` and `UNIQUE` in the schema. The table
then holds rows the index has never heard of, and the next `DELETE` touching
them aborts with *"Failed to delete all rows from index"* and takes the
connection down permanently. Re-indexing cannot fix it, because re-indexing
begins by deleting.

**Cost.** A checkpoint at the end of every run. `devctx repair` exists for
databases already in that state: it copies each table aside, drops it, recreates
it from the schema and writes the rows back, so the ART index is rebuilt from
the data.

## ADR-06: Drop the derived indexes during a bulk load

**Decision.** HNSW and FTS indexes are dropped before a large indexing run and
rebuilt after.

**Why.** DuckDB maintains an HNSW index on **every insert**, which is
catastrophic during a bulk load — measured at 7 files/minute with the index
present against 58 files/minute without it, on the same repository. FTS has a
harder version of the problem: DuckDB cannot maintain an FTS index across row
deletions on the indexed table, so a re-index that deletes rows aborts outright.

**Cost.** A rebuild at the end, and a window during the run where approximate
search is unavailable.

## ADR-07: Per-project stores; share only what has no owner

**Decision.** Each repository gets its own database. A central store holds only
the project registry and the memories that are explicitly global or
group-scoped.

**Why.** An earlier design pointed every repository at one database. Per-project
stores mean re-indexing one repository never blocks another, each may use a
different embedding model, and no search needs a repository filter to be
correct.

**Cost.** Cross-project search is an explicit call (`search_project`), not a
default.

## ADR-08: Branch copies, driven by content hash

**Decision.** Chunks are stored per `(repo, branch)`. Indexing a second branch
copies rows for files whose content hash matches instead of re-embedding them.

**Why.** *This reverses an earlier design.* The predecessor used a branch
overlay: one base index plus a diff. Overlays are elegant and wrong here — every
read pays a merge, and the merge has to know which side wins for a file touched
on both. Copies make a read a plain filtered query.

Copying is affordable because embedding is the expensive part, not storage, and
branches share almost all of their content. Measured across three real
repositories: **95–96% of files copied rather than re-embedded**.

**Cost.** Storage grows roughly linearly with declared branches. And a known
caveat: changing `indexing.exclude` between runs is not reflected in the content
hash, so dedup can copy rows that the new exclusions would have dropped.

## ADR-09: AST-aware chunking, never split a symbol

**Decision.** Chunk boundaries come from tree-sitter parses, at file / class /
doc / function / block level.

**Why.** Fixed-window chunking splits a function across two chunks, and neither
half embeds as the thing the function does. Symbol boundaries are the units
people ask questions about.

**Cost.** A grammar per language. Files in unsupported languages fall back to
raw-text chunks with overlap.

## ADR-10: Incremental indexing from the git diff, against the work tree

**Decision.** Indexing computes what changed via git, but indexes the **work
tree**, not the last commit.

**Why.** A file you have written but not committed is exactly the code you are
most likely to ask about.

**Cost.** Anything git ignores never reaches the index — `.gitignore` is the
first place to control what gets indexed, which surprises people once.

## ADR-11: Memory identity by topic key, falling back to content hash

**Decision.** `--topic` upserts. Without it, identity is a hash over normalized
content.

**Why.** Two different failure modes need two different answers. A decision that
gets revised must replace itself or the store accumulates contradictory
versions — that is the topic key. An observation saved twice by an eager agent
must not become two rows — that is the content hash.

**Cost.** Whoever writes the memory has to decide which case they are in.

## ADR-12: Global and group memories are re-keyed, not tagged

**Decision.** Global rows carry the reserved project `@global`; group rows carry
`@group:<name>`. The contributing repository survives in `repo`.

**Why.** Identity derives from project + content hash. If a global row kept its
contributing project, the same lesson learned in two repositories would land as
two rows — dedup failing exactly where sharing matters most.

**Cost.** `project` is no longer a plain foreign key, and code reading it has to
know about the reserved values.

## ADR-13: The junction row lives in the project store

**Decision.** A memory↔symbol link is written to the **project** database and
carries only the memory's id, even when the memory itself lives centrally.

**Why.** The call graph is per-repository; a global memory is not. A memory
about this repository's `charge()` must be findable from `charge()` regardless
of which store holds its text. Resolving the id looks locally first, then
centrally.

**Cost.** A lookup indirection. The alternative — copying memory text into every
project that mentions it — would leave stale copies behind every edit.

## ADR-14: Link provenance is returned, not hidden

**Decision.** Every memory-by-code result carries `link_sources`: `files-field`,
`content-mention`, or `inference`.

**Why.** The first two mean something connected the memory to the code at write
time. The third means only that words matched. Collapsing them into one
"related" flag would present a guess with the same confidence as a fact.

**Cost.** Callers have to read a field to know how much to trust a result.

## ADR-15: Reranking off by default

**Decision.** The cross-encoder is disabled unless configured on.

**Why.** Measurement, not principle. On this repository a search costs 30 ms and
406 MB resident; the cheapest cross-encoder takes it to 8.6 s and 2.4 GB, and
`bge-reranker-base` to 30 s and 3.4 GB. What that buys is reordering a list the
retriever already had right — and the one model measured across the whole bench
made it worse, demoting a correct answer from first to twenty-first.

**Cost.** Ordering is retriever ordering. Everything the retrieval stage found
is still returned.

## ADR-16: Graceful degradation over hard failure

**Decision.** Hybrid search falls back to vector-only when the FTS index is
absent. Linking is best-effort and returns a count rather than an error.

**Why.** These are enrichments. A repository not yet fully indexed must not turn
a successful `remember` into a failure, and a missing keyword index should
narrow results rather than break the query.

**Cost.** Silent degradation is a real hazard — it is acceptable here only
because the degraded result is still correct, just less good. Where truncation
would be *misleading* rather than merely worse, the system says so instead: see
`build_context`, which names what did not fit.

## ADR-17: `build_context` returns prose

**Decision.** One tool returns text rather than JSON.

**Why.** Its output is meant to be read straight into a model's context. A JSON
envelope around code and prose spends budget on punctuation and buys structure
nobody parses.

**Cost.** Inconsistent with most other tools, which return JSON. `repo_map` and
`skeleton` (0.10) return text for the same reason, and so does `summarize`.

## ADR-18: A workspace root binds to its group, not to nothing

**Decision.** When the MCP server's working directory holds no project but
*contains* registered ones, it descends into the registry: one project under it
binds that project, several sharing a `project.group` bind the group. Bound to a
group, `remember` defaults to `scope: group`. Code tools take an optional
`project` — a name or any path inside one — that resolves a single call without
moving the session.

**Why.** A globally-registered server inherits the client's working directory,
and for anyone whose repositories live side by side under one folder that
directory is the workspace root. Looking only *upwards* from there finds
nothing, so the server came up unbound — and unbound is not a degraded mode:
`remember` fails outright and the memory is lost. The registry knew where every
one of those projects was the whole time.

The `project` hint exists because the process's working directory is fixed at
spawn. A binding resolved at startup cannot follow an agent that moves between
repositories, so cross-repository work needs a signal carried by the call.

**Cost.** Two changes in observable behaviour. From a workspace root the server
now starts bound where it used to start unbound; and in group mode a `remember`
without an explicit `scope` lands in `group` rather than `local`. The second is
softer than it looks — the calls it changes used to fail rather than write.

A memory's `repo` field still comes from the binding, so in group mode it names
the default member. Attribution is a separate model from scope and changing it
was left out.

**Rejected.** Binding one member and calling it the workspace: it would attribute
every memory to a repository nobody chose. Fanning code search across a group's
stores: useful, but a different feature with its own ranking problem.

## ADR-19: A static graph from tree-sitter and per-language rules — not stack-graphs, LSP or SCIP, yet

**Decision.** Symbols and their relations come from tree-sitter queries kept in
`languages/*.json`; resolving them across files is a link pass of ordered,
per-language rules written in Rust (imports, lexical scopes, declared types,
manifests). Every edge records how sure it is (`confidence`) and why
(`resolution`), and what a rule cannot decide stays undecided rather than
guessed.

**Why.** It fits ADR-01: one binary, no compiler, language server or JVM/Node
runtime on the user's machine, and it works on any checkout — a repository that
does not build, a branch half way through a refactor — file by file, in the
same pass that chunks it, incrementally. What a coding agent needs from the
graph is localization (where is it, what is around it, what does it reach),
and published systems that do that well (LocAgent, RepoGraph, Aider's repo map)
use graphs of exactly this kind, coarse but clean. Measured on hand-labelled
call sites: Java 20 of 25 right (19 of 19 `high`), TypeScript 20 of 20, Python
26 of 27, Rust 26 of 26.

The alternatives, and why not now:

| Alternative | Why not |
|---|---|
| stack-graphs | Archived upstream, with an open advisory (RUSTSEC-2026-0303); its rule language would still have to be written per language |
| An LSP server on demand | One server and one toolchain per language, a project that builds, seconds of cold start, a second process lifecycle (ADR-01's cost again) and per-query latency |
| SCIP | Compiler-grade, but an index produced by each language's own indexer inside a build (`scip-java` compiles the project). Kept as a future import path: the `edges.edge_source` column is reserved for it |
| Kythe, universal-ctags | Kythe needs a build integration per language; ctags gives definitions, not resolved references |

**Cost.** It is not a compiler. No type inference beyond declarations and
initializers, overloads chosen by arity, no flow analysis; calls through
callbacks or reflection leave no edge; Go is covered by fixtures only and
Kotlin not at all. Rules are code to maintain per language, and the extractor's
output is pinned by a golden test, so every change of it is an
`EXTRACTOR_VERSION` and a `devctx index --full` for every user.

## ADR-20: The symbol graph is additive; the old call table stays a release

**Decision.** 0.10 adds `symbols` and `edges` (no primary key, no `UNIQUE`: the
writer replaces a file's rows) and keeps writing 0.9's `graph_edges` in its old
format. A branch not yet rebuilt is answered by 0.9's readers with a warning;
the tools that only exist on the new graph refuse with an error that names the
fix. A reindex is asked for, never done behind the user's back.

**Why.** An in-place migration would be the index's first, and the one place a
crash mid-way corrupts what was already there. Without keys there is no ART
index to lose on WAL replay (ADR-05). Keeping `graph_edges` means going back to
0.9.0 leaves a working graph, and an un-rebuilt branch keeps its old answers
instead of losing them. A full reindex is honest about what changed, and reusing
the vectors of unchanged chunks makes it cost parsing rather than embedding.

**Cost.** `devctx index --full` once per repository and per branch. The
database grows — the new tables, and free blocks DuckDB does not reclaim until
the compaction of ADR-22 — and two read paths live side by side until
`graph_edges` is dropped in a later release.

## ADR-21: Graph signals in ranking are opt-in until they are measured to help

**Decision.** In 0.10, symbol centrality in hybrid search
(`search.centrality_weight: 0`) and the related signatures of `build_context`
(`related: false`) are off by default. Dispatch through interfaces in
`impact_analysis`, which closes a false negative, is on.

**Why.** Measurement, as in ADR-15. Centrality moved one evaluation case out of
twelve — within the harness's noise — and the one Spanish case that moved got
worse. Related signatures raised the brief's hit rate 3.8 points against the 10
the plan required for a default, by a path in a `called by` line. Off, they cost
nothing and change nothing: no bonus, no extra read of the graph, and the brief
laid out as 0.9 laid it out.

**Cost.** Users who never turn them on never benefit from them. Turned on, they
change hybrid scores and the brief's contents, which is why they are documented
as changes, not as tuning.

## ADR-22: Space is given back by a verified copy and an atomic `rename`

**Decision.** After the `devctx index --full` that migrates an index made
before the graph, and on `devctx compact`, the database is copied row by row
into `<index>.compact.tmp` with this binary's schema, its HNSW and BM25 indexes
are rebuilt there, the copy is checked against the original (row counts, a
checksum of every column of every row, and `EXCEPT ALL` both ways for the
memories, their references and the sessions), and it replaces the original with
one `rename`. The client does it with the project's server stopped, never the
server.

**Why.** A reusing full run deletes and writes again every row of `vectors`,
and an upgraded index ends at about twice its content. `VACUUM` gives nothing
back, and a checkpoint only truncates free blocks at the very end of the file.
DuckDB's `COPY FROM DATABASE` was measured and rejected: it recreates the HNSW
index with the `l2sq` metric, which the queries then stop using. Copying keeps
the original untouched until the `rename`, so any failure leaves it as it was.

**Exclusion.** An operating-system lock on a file of its own,
`<index>.compact.lock`: every `Store::open` holds it shared for as long as the
store lives, a compaction holds it exclusively. Not DuckDB's lock on the
database, which is an `fcntl` lock and vanishes the moment the process closes
*any* descriptor of the file. That lock still does its part: one in-memory
connection holds the original attached read-only from before the copy until
after the `rename`, which is what keeps out a binary that does not know the
lock file (0.9.0), and the compacting process never opens another descriptor of
the original. Its write-ahead log must be absent, and its size, mtime and inode
unchanged, both right after the attach and right before the `rename` — DuckDB
applies a log it finds next to a file without checking it belongs there.

**Cost.** As much free disk as the file while it runs, the server of the
project stopped for those seconds (announced; only with a terminal, and
switched off by `DEVCTX_NO_AUTO_COMPACT`), and the HNSW index rebuilt a second
time on the upgrade. A rebuilt HNSW index is not the same graph, so its
approximate top-k can differ in a few results; the data and the exact top-k do
not. Group, ACLs and extended attributes of the file are not carried over.
