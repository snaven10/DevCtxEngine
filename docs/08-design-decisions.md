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

**Cost.** Inconsistent with the other 22 tools, which return JSON.

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
