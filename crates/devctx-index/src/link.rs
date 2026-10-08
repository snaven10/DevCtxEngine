//! The link pass (PLAN-009 DD-6): after a run's files and before the
//! extractor stamp, resolve the branch's edges to the symbols they reach —
//! `dst_id`, `confidence`, `resolution`, `external` — or drop the calls
//! nothing can type (DD-7).
//!
//! The rules live in `devctx_parse::resolve::link` (pure, tested on parses);
//! this module loads the branch, decides which edges to re-resolve and writes
//! the answers back, one transaction per file, checking for a stop between
//! files. `contains` is never loaded nor written: it is intra-file and
//! resolved at parse time (DD-6), and the writer deletes every kind but it.
//!
//! **Incremental** (DD-6): re-resolved are (a) the edges of the files this run
//! wrote or deleted, (b) the edges of other files whose destination no longer
//! exists (its `dst_id` is gone: a deleted or renamed symbol) or whose
//! destination's last segment names a symbol of a written file (an added
//! one), (c) those that stayed unresolved, and (d) every edge of a
//! TypeScript/JavaScript or Python file importing a written file, directly or
//! through barrels re-exporting it (what an import binds can change though no
//! name of the written file does: a barrel's `export … from` moved, a
//! package `__init__.py`'s `from .a import X`). A different
//! link environment — `tsconfig` aliases, `package.json`, `Cargo.toml`,
//! `go.mod`, Python manifests (`crate::env`) — relinks the branch in full. When the written files are
//! more than a fifth of the branch, every edge is (the full pass). A file is
//! rewritten only if one of its rows changed.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use devctx_parse::resolve::env::LinkEnv;
use devctx_parse::resolve::link::{LinkEdge, LinkSymbol, Outcome, RepoIndex};
use devctx_store::{Store, StoredSymbolEdge};

use crate::error::Result;

/// Version of the link rules (`devctx_parse::resolve::link` and this
/// module): what the branch's edges were resolved under. A build with other
/// rules relinks the branch in full on its next run, as the extractor
/// version does for the parse.
pub(crate) const LINK_VERSION: &str = "14";

/// `index_meta` key of [`LINK_VERSION`].
pub(crate) const LINK_VERSION_META_KEY: &str = "link_version";

/// `index_meta` key of the fingerprint of the link environment the branch
/// was linked under — the `tsconfig` and `package.json` files (TASK-006),
/// every `Cargo.toml`, `go.mod` and Python manifest (TASK-007): a changed
/// alias, dependency, crate or module relinks it in full, though no source
/// file changed. (The key keeps its TASK-006 name.)
pub(crate) const LINK_CONFIG_META_KEY: &str = "link_tsconfig";

/// `index_meta` key of that environment itself, the last one that could be
/// read: a run that cannot read a file of it (a merge conflict) links under
/// that file's version here. An environment stored before TASK-007 (only
/// TypeScript's) reads back as one.
pub(crate) const LINK_ENV_META_KEY: &str = "link_script_env";

/// `index_meta` key set while a run owes the branch a link pass (from before
/// its first file to the end of the pass): a run cut short leaves it, and the
/// next run links in full.
pub(crate) const LINK_PENDING_META_KEY: &str = "link_pending";

/// What a link pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// Every edge was re-resolved, not only the incremental selection.
    pub full: bool,
    /// Edges re-resolved.
    pub resolved: usize,
    /// Calls of the branch marked `discarded` after the pass: an untypable
    /// receiver and a name the repository does not define (DD-7). Kept, so
    /// a later definition reopens them.
    pub discarded: usize,
    /// `calls` of the branch left with neither a destination nor the
    /// `external` mark, after the pass.
    pub unresolved_calls: usize,
    /// Files whose edges were rewritten.
    pub files_written: usize,
    /// Stopped between files; what it wrote is committed.
    pub cancelled: bool,
    /// Wall time, milliseconds.
    pub ms: u128,
    /// Of which loading the branch and building the index.
    pub load_ms: u128,
    /// Of which writing the rewritten files.
    pub write_ms: u128,
}

/// Files whose rewritten edges commit together. Each file's rows are written
/// whole inside one transaction — never split across two — but a transaction
/// (and a `DELETE` scan) per file cost ~10 ms each (1 254 files of a Java
/// repository: 12.9 of the pass's 14.1 s), so several files share one. A
/// reader during a pass can see the branch half relinked (some files with
/// the old answers, some with the new): each answer is a whole file's.
const WRITE_BATCH: usize = 64;

/// Write `pending` (whole files) in one transaction, and empty it.
fn write_batch(
    store: &Store,
    repo: &str,
    branch: &str,
    pending: &mut Vec<(&str, Vec<StoredSymbolEdge>)>,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    store.replace_files_linked_edges(repo, branch, pending)?;
    pending.clear();
    Ok(())
}

/// The `resolution` of a call the link pass dropped (DD-7): kept as a row,
/// excluded from the counts and the readers, reopened by name.
pub const DISCARDED: &str = "discarded";

/// The names a hint mentions, by position — the receiver's type (`typed
/// <via> <Type>`), a receiver name (`name <r>`), a member path, an
/// anonymous class's supertype, each callee back along a chain — never its
/// words (`typed`, `field`, `name`…), so a callee called `name()` or a type
/// `Field` still counts and a field `name` reopens nothing by the word.
fn hint_names(hint: &str) -> Vec<&str> {
    let mut tokens: Vec<&str> = hint.split_whitespace().collect();
    if tokens.last().is_some_and(|t| t.starts_with('/')) {
        tokens.pop();
    }
    let mut out = Vec::new();
    let mut rest: &[&str] = &tokens;
    loop {
        match rest {
            ["typed", _via, ty, ..] => {
                // What a call returns (`make()`, TASK-007): the callee's path.
                let ty = ty.strip_suffix('?').unwrap_or(ty);
                let ty = ty.strip_suffix("()").unwrap_or(ty);
                out.extend(ty.split('.'));
                break;
            }
            ["name", r, ..] => {
                out.extend(r.split('.'));
                break;
            }
            ["member", path, tail @ ..] => {
                out.extend(path.split('.'));
                rest = tail;
            }
            ["anon", ty, tail @ ..] => {
                out.extend(ty.split('.'));
                rest = tail;
            }
            ["chain", callee, tail @ ..] => {
                // A Rust `f()?` is `f`.
                out.push(callee.trim_end_matches('?'));
                rest = tail;
            }
            // A Rust path (`path crate::a::Foo`): its segments.
            ["path", p, ..] => {
                out.extend(p.split("::"));
                break;
            }
            _ => break,
        }
    }
    out
}

/// Whether an importer's symbol can type a call after it (`a.f().g()`, a
/// field in `a.x.g()`, a type): a type, a field, or a callable with a
/// declared return that is something (not `void`, `None`, `()`). Only those
/// names reopen edges by mode (d): a constructor, a lifecycle hook or any
/// function returning nothing types no chain (TASK-007 review: it replaces
/// a fixed list of generic names, which dropped `pipe(): Repo`).
fn types_a_chain(s: &devctx_store::StoredSymbol) -> bool {
    use devctx_parse::resolve::{go, java, python, rust, typescript};
    let class = devctx_core::symbol_id::kind_class(&s.kind);
    // A `const`/`static`/module variable is typed by its declaration too
    // (second review, N2).
    if class == "type" || s.kind == "field" || s.kind == "const" {
        return true;
    }
    if class != "callable" {
        return false;
    }
    let Some(sig) = s.signature.as_deref() else {
        return false;
    };
    let lang = devctx_parse::detect_lang(std::path::Path::new(&s.file));
    let ret: Option<String> = match lang.map(|l| l.key()) {
        Some("java") => java::declared_type(sig, &s.name),
        Some("typescript" | "tsx" | "javascript") => typescript::return_type(sig),
        Some("python") => python::return_type(sig),
        Some("rust") => rust::return_type(sig).map(|t| t.base),
        Some("go") => go::return_type(sig),
        _ => None,
    };
    ret.is_some_and(|r| !matches!(r.as_str(), "void" | "None" | "undefined" | "never"))
}

/// The fraction of a branch's files over which a run re-resolves every edge.
const FULL_PASS_DIVISOR: usize = 5;

fn leaf(name: &str) -> &str {
    let after = name.rsplit("::").next().unwrap_or(name);
    after.rsplit('.').next().unwrap_or(after)
}

/// The name an edge's destination ends in: a TypeScript/JavaScript import
/// row's imported name (`rxjs#map` → `map`), else the last segment.
fn dst_key(e: &StoredSymbolEdge) -> &str {
    if e.kind == "imports" {
        if let Some((_, name)) = e.dst_name.rsplit_once('#') {
            return name;
        }
    }
    leaf(&e.dst_name)
}

fn to_link(e: &StoredSymbolEdge) -> LinkEdge {
    LinkEdge {
        kind: e.kind.clone(),
        src_id: e.src_id,
        dst_name: e.dst_name.clone(),
        hint: e.hint.clone(),
        file: e.file.clone(),
        line: e.line,
    }
}

/// Run the link pass over `branch`. `written`: the files whose graph rows
/// this run wrote or deleted; `full`: re-resolve every edge regardless;
/// `env`: the workspace's link environment (`tsconfig`, `package.json`,
/// `Cargo.toml`, `go.mod`, Python manifests).
pub(crate) fn link_branch(
    store: &Store,
    repo: &str,
    branch: &str,
    written: &HashSet<String>,
    full: bool,
    env: &LinkEnv,
    cancelled: &dyn Fn() -> bool,
) -> Result<LinkStats> {
    let started = Instant::now();
    let symbols = store.branch_symbols(repo, branch)?;
    let edges = store.branch_linkable_edges(repo, branch)?;
    let files: HashSet<&str> = symbols.iter().map(|s| s.file.as_str()).collect();
    let mut full = full || written.len() * FULL_PASS_DIVISOR > files.len();
    let ids: HashSet<u64> = symbols.iter().map(|s| s.id).collect();
    let facts: Vec<LinkEdge> = edges
        .iter()
        .filter(|e| matches!(e.kind.as_str(), "imports" | "inherits" | "implements"))
        .map(to_link)
        .collect();
    let index = RepoIndex::with_env(
        symbols
            .iter()
            .map(|s| LinkSymbol {
                id: s.id,
                parent_id: s.parent_id,
                file: s.file.clone(),
                kind: s.kind.clone(),
                name: s.name.clone(),
                qualified: s.qualified.clone(),
                package: s.package.clone(),
                signature: s.signature.clone(),
                exported: s.exported,
                start_line: s.start_line,
                end_line: s.end_line,
            })
            .collect(),
        &facts,
        env.clone(),
    );
    drop(facts);
    // What a written file changed that other files' edges may depend on
    // (DD-6 mode b): every name it defines — methods, fields, types — and
    // every type below one of its types (a changed `extends` moves what
    // their members inherit). An edge is reopened when its `dst_name` or a
    // token of its `hint` (the receiver's type, a chain's previous callee)
    // is one of those names, or when its source sits in one of those types.
    // (d) TypeScript/JavaScript and Python files importing a written one
    // (or a barrel, any Python module, re-exporting it): every edge, since
    // what an import binds can change without any name of the written file
    // changing (a package `__init__.py` that re-exports another module). With them the
    // selection can exceed the full-pass threshold: then the full pass.
    let mut importers = if full {
        HashSet::new()
    } else {
        index.importers_of(written)
    };
    if !full && (written.len() + importers.len()) * FULL_PASS_DIVISOR > files.len() {
        full = true;
        importers.clear();
    }
    let written_ids: HashSet<u64> = symbols
        .iter()
        .filter(|s| written.contains(&s.file))
        .map(|s| s.id)
        .collect();
    let affected = index.subtypes_of(&written_ids);
    let mut fresh: HashSet<&str> = symbols
        .iter()
        .filter(|s| written.contains(&s.file))
        .map(|s| s.name.as_str())
        .collect();
    fresh.extend(affected.iter().filter_map(|&id| index.name_of(id)));
    // An importer's own names too: what its symbols return or hold may now
    // be another type (`a.f().g()` in a third file, with `f` of an importer
    // returning a type the written barrel re-exports).
    fresh.extend(
        symbols
            .iter()
            .filter(|s| importers.contains(&s.file) && types_a_chain(s))
            .map(|s| s.name.as_str()),
    );
    let loaded = started.elapsed().as_millis();
    let mut writing = std::time::Duration::ZERO;

    let mut stats = LinkStats {
        full,
        ..Default::default()
    };
    let mut by_file: HashMap<&str, Vec<&StoredSymbolEdge>> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for e in &edges {
        let list = by_file.entry(e.file.as_str()).or_insert_with(|| {
            order.push(e.file.as_str());
            Vec::new()
        });
        list.push(e);
    }
    let reopened = |e: &StoredSymbolEdge| -> bool {
        // A Rust path call answered from outside (`std::fs::read`, a declared
        // crate's, a `use` of one) depends only on the environment and on
        // the `use`s that reach it, which the full pass on a new environment,
        // mode (a) and mode (d) cover: no name of a written file changes it
        // (TASK-007 review, performance).
        // Only an `external_known` path whose first segment names the
        // platform or a crate from outside directly (not a derivable's
        // `inherited`, not a path through a repository module or a `use`):
        // second review, N1.
        if e.external == Some(true) && e.resolution.as_deref() == Some("external_known") {
            if let Some(prefix) = e
                .hint
                .as_deref()
                .and_then(|h| h.strip_prefix("path "))
                .and_then(|h| h.split_whitespace().next())
            {
                if index.rs_external_root(&e.file, prefix) {
                    return false;
                }
            }
        }
        fresh.contains(dst_key(e))
            || e.hint
                .as_deref()
                .is_some_and(|h| hint_names(h).iter().any(|t| fresh.contains(t)))
            || (!affected.is_empty() && index.within(e.src_id, &affected))
    };
    let mut pending: Vec<(&str, Vec<StoredSymbolEdge>)> = Vec::new();
    for file in order {
        // Between files, never inside one: each file's rows are written
        // whole inside one transaction.
        if cancelled() {
            stats.cancelled = true;
            break;
        }
        let rows = &by_file[file];
        // A Rust importer (not written): only the edges that mention a name
        // its `use`s bind can change through them; the rest is the file's
        // own (its items, the prelude), which modes (b) and (c) cover
        // (TASK-007 review, performance).
        let use_names = if !written.contains(file) && importers.contains(file) {
            index.rs_use_names(file)
        } else {
            None
        };
        let through_use = |e: &StoredSymbolEdge| -> bool {
            let Some(names) = &use_names else {
                return true;
            };
            // A type use, an instantiation, a `use` row: by the names its
            // destination is written with.
            if e.kind != "calls" {
                return e
                    .dst_name
                    .split([':', '.'])
                    .filter(|x| !x.is_empty())
                    .any(|x| {
                        names.contains(x) || matches!(x, "crate" | "self" | "super" | "Self")
                    });
            }
            let Some(h) = e.hint.as_deref() else {
                return true;
            };
            let first = h.split_whitespace().next().unwrap_or_default();
            if matches!(first, "member" | "chain" | "anon") {
                return true;
            }
            let tokens = hint_names(h);
            names.contains(dst_key(e))
                || tokens.iter().any(|t| {
                    names.contains(*t) || matches!(*t, "crate" | "self" | "super" | "Self")
                })
        };
        let file_written = written.contains(file) || importers.contains(file);
        // Rows are cloned only once one of them changes.
        let mut out: Option<Vec<StoredSymbolEdge>> = None;
        for (i, e) in rows.iter().enumerate() {
            let pick = full
                || (file_written && through_use(e))
                || e.resolution.is_none()
                || e.dst_id.is_some_and(|d| !ids.contains(&d))
                // (c) the undecided — a discarded row waits for its name.
                || (e.dst_id.is_none()
                    && !e.external.unwrap_or(false)
                    && e.resolution.as_deref() != Some(DISCARDED))
                || reopened(e);
            let new = if pick {
                stats.resolved += 1;
                match index.resolve(&to_link(e)) {
                    None => None,
                    // Kept, marked: out of the counts and the readers, and
                    // reopened by name when the repository defines it later
                    // (the order files are indexed in must not decide it).
                    Some(Outcome::Discard) => Some(StoredSymbolEdge {
                        dst_id: None,
                        confidence: Some("low".to_string()),
                        resolution: Some(DISCARDED.to_string()),
                        external: Some(false),
                        ..(*e).clone()
                    }),
                    Some(Outcome::Resolved(r)) => Some(StoredSymbolEdge {
                        dst_id: r.dst_id,
                        confidence: Some(r.confidence.to_string()),
                        resolution: Some(r.resolution.to_string()),
                        external: Some(r.external),
                        ..(*e).clone()
                    }),
                }
                .filter(|n| n != *e)
            } else {
                None
            };
            match (new, out.as_mut()) {
                (Some(n), Some(o)) => o.push(n),
                (Some(n), None) => {
                    let mut o: Vec<StoredSymbolEdge> = Vec::with_capacity(rows.len());
                    o.extend(rows[..i].iter().map(|r| (*r).clone()));
                    o.push(n);
                    out = Some(o);
                }
                (None, Some(o)) => o.push((*e).clone()),
                (None, None) => {}
            }
        }
        let changed = out.is_some();
        let tally = |rows: &mut dyn Iterator<Item = &StoredSymbolEdge>, stats: &mut LinkStats| {
            for e in rows.filter(|e| e.kind == "calls") {
                if e.resolution.as_deref() == Some(DISCARDED) {
                    stats.discarded += 1;
                } else if e.dst_id.is_none() && !e.external.unwrap_or(false) {
                    stats.unresolved_calls += 1;
                }
            }
        };
        match &out {
            Some(o) => tally(&mut o.iter(), &mut stats),
            None => tally(&mut rows.iter().copied(), &mut stats),
        }
        let out = out.unwrap_or_default();
        if changed {
            pending.push((file, out));
            if pending.len() >= WRITE_BATCH {
                let w = Instant::now();
                write_batch(store, repo, branch, &mut pending)?;
                writing += w.elapsed();
            }
            stats.files_written += 1;
        }
    }
    let w = Instant::now();
    write_batch(store, repo, branch, &mut pending)?;
    writing += w.elapsed();
    stats.ms = started.elapsed().as_millis();
    stats.load_ms = loaded;
    stats.write_ms = writing.as_millis();
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::hint_names;

    #[test]
    fn hint_names_are_read_by_position() {
        assert_eq!(hint_names("typed field Foo /1"), ["Foo"]);
        assert_eq!(hint_names("name a.b.Office"), ["a", "b", "Office"]);
        assert_eq!(hint_names("chain name bare /0"), ["name"]);
        assert_eq!(
            hint_names("chain find chain path typed param R /1"),
            ["find", "path", "R"]
        );
        assert_eq!(
            hint_names("member item.owner typed local H"),
            ["item", "owner", "H"]
        );
        assert_eq!(hint_names("anon Runnable bare"), ["Runnable"]);
        assert_eq!(hint_names("typed local mod.make() /1"), ["mod", "make"]);
        assert_eq!(
            hint_names("typed field Store.open()? /0"),
            ["Store", "open"]
        );
        assert!(hint_names("bare /2").is_empty());
    }
}
