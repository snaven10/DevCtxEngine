//! `skeleton` in the serve (PLAN-009 TASK-014, DD-17): one file's rows of
//! `symbols` and the summary of its imports, drawn by
//! [`crate::skeleton::render`]. Nothing is read from the file itself.
//!
//! There is no 0.9 path: a branch without a current symbol graph is an error
//! that asks for `devctx index --full` (DD-19), and a file the index does not
//! hold is an error with the nearest paths it does.

use std::path::Path;

use devctx_store::Store;

use super::lookup::{Legacy, Reader};
use super::BranchChoice;
use crate::skeleton::{render, SkeletonInput, SkeletonQuery, IMPORTS_SHOWN};

/// The error of a branch the current extractor has not indexed (DD-19).
pub const SKELETON_NEEDS_REINDEX: &str =
    "skeleton reads the symbol graph, and this branch's index predates it (an older \
     extractor, or no symbol rows): reindex with devctx index --full (vectors are reused)";

/// The error of a failed read of the symbol graph: not a reason to reindex.
const SKELETON_READ_ERROR: &str =
    "could not read the symbol graph of this branch (a read error, not a reason to \
     reindex); retry, and see serve.log if it persists";

/// Paths suggested for a file the index does not hold, at most.
const MAX_FILE_SUGGESTIONS: usize = 5;

/// `file` as the index spells it: relative to `root` (an absolute path
/// inside the repository is made relative, as `read_file` reads it), with
/// `/` separators and no leading `./`.
pub(super) fn repo_relative(root: &Path, file: &str) -> Result<String, String> {
    let f = file.trim().replace('\\', "/");
    let rel = if Path::new(&f).is_absolute() {
        let canonical = root.canonicalize().ok();
        let inside = std::iter::once(root)
            .chain(canonical.as_deref())
            .find_map(|r| Path::new(&f).strip_prefix(r).ok().map(Path::to_path_buf));
        match inside {
            Some(p) => p.to_string_lossy().replace('\\', "/"),
            None => {
                return Err(format!(
                    "{file} is outside this repository ({}): pass a repo-relative path",
                    root.display()
                ))
            }
        }
    } else {
        f
    };
    let mut rel = rel.as_str();
    while let Some(rest) = rel.strip_prefix("./") {
        rel = rest;
    }
    Ok(rel.trim_start_matches('/').to_string())
}

/// The skeleton `q` asks for on `chosen`, within `budget` tokens.
pub(super) fn skeleton_on(
    store: &Store,
    chosen: &BranchChoice,
    root: &Path,
    q: &SkeletonQuery,
    budget: usize,
) -> Result<String, String> {
    q.validate()?;
    match Reader::of(chosen, store) {
        Reader::SymbolGraph => {}
        Reader::Legacy(Legacy::Error) => return Err(SKELETON_READ_ERROR.into()),
        Reader::Legacy(_) => return Err(SKELETON_NEEDS_REINDEX.into()),
    }
    let file = repo_relative(root, &q.file)?;
    let (repo, branch) = (&chosen.repo, &chosen.branch);
    let symbols = store
        .file_symbols(repo, branch, &file)
        .map_err(|_| SKELETON_READ_ERROR.to_string())?;
    if symbols.is_empty() {
        return Err(not_found(store, chosen, root, &file));
    }
    let imports = store
        .file_imports(repo, branch, &file, IMPORTS_SHOWN)
        .map_err(|_| SKELETON_READ_ERROR.to_string())?;
    let head: Vec<String> = q.label.iter().cloned().collect();
    let mut tail = Vec::new();
    if let Some(f) = &chosen.fallback {
        tail.push(format!(
            "[devctx] branch_fallback: {}",
            f.to_json()["why"]
                .as_str()
                .unwrap_or("answered from another branch")
        ));
    }
    Ok(render(
        &SkeletonInput {
            file: &file,
            symbols: &symbols,
            imports: &imports,
            head: &head,
            tail: &tail,
        },
        budget,
    ))
}

/// Why `file` has no skeleton on `chosen`, with what to do: indexed as text
/// (no symbols: `read_file`), or not indexed (on disk or not), with the
/// nearest indexed paths.
fn not_found(store: &Store, chosen: &BranchChoice, root: &Path, file: &str) -> String {
    let (repo, branch) = (&chosen.repo, &chosen.branch);
    if store.has_file_rows(repo, branch, file).unwrap_or(false) {
        return format!(
            "{file} is indexed on branch {branch} but has no symbols (a language without \
             symbol extraction); read it with read_file"
        );
    }
    let why = if root.join(file).is_file() {
        "it exists on disk, but the index does not hold it: excluded, not committed, or \
         added since the last index (devctx index)"
    } else {
        "no such file in the repository"
    };
    let near = store
        .file_suggestions(repo, branch, file, MAX_FILE_SUGGESTIONS)
        .unwrap_or_default();
    let mut msg = format!("{file} is not in the index of branch {branch}: {why}");
    if !near.is_empty() {
        msg.push_str(&format!("; nearest indexed paths: {}", near.join(", ")));
    }
    msg.push_str("; paths are repo-relative, as read_file takes them");
    msg
}

#[cfg(test)]
mod tests {
    use super::super::lookup::tests::{indexed_files, Fake};
    use super::super::*;
    use crate::skeleton::SkeletonQuery;

    const JAVA_PATH: &str = "src/main/java/app/core/OrderService.java";

    /// (a) A class with an inner class, a constructor, methods and a field;
    /// a stacked and a long annotation; a multi-line signature with long
    /// generics. Bodies carry `BODY_` markers (e).
    fn java() -> String {
        format!(
            r#"package app.core;

import java.util.List;
import java.util.Map;
import app.core.repo.OrderRepo;

@ApplicationScoped
public class OrderService {{
    private final OrderRepo repo = new OrderRepo("BODY_FIELD_INIT");

    public OrderService(OrderRepo repo) {{
        this.repo = repo; // BODY_CTOR
    }}

    @Override
    @Transactional
    public List<String> place(
            String id,
            Map<String, List<Integer>> lines) {{
        String marker = "BODY_METHOD";
        return List.of(marker);
    }}

    @Operation(summary = "{long}")
    public void annotated() {{
        System.out.println("BODY_ANNOTATED");
    }}

    public <T extends Comparable<? super T>> Map<String, List<Map<Integer, T>>> group(
            List<T> items,
            java.util.function.Function<T, String> key) {{
        return null; // BODY_GENERIC
    }}

    static class Line {{
        int qty;

        Line(int qty) {{
            this.qty = qty; // BODY_INNER_CTOR
        }}

        int total() {{
            return qty * 2; // BODY_INNER
        }}
    }}
}}
"#,
            long = "s".repeat(240)
        )
    }

    const TS_PATH: &str = "web/src/app/session.store.ts";

    /// (b) An exported arrow-const, a decorated class, a decorated member and
    /// a decorated field with a value; bodies marked.
    const TS: &str = r#"import { Injectable, HostListener, Input } from '@angular/core';
import { HttpClient, HttpInterceptorFn } from '@angular/common/http';

export const authInterceptor: HttpInterceptorFn = (req, next) => {
  const marker = 'BODY_ARROW';
  return next(req);
};

@Injectable({
  providedIn: 'root',
})
export class SessionStore {
  @Input() name = 'BODY_FIELD';

  constructor(private readonly http: HttpClient) {
    console.log('BODY_TS_CTOR');
  }

  @HostListener('window:resize', ['$event'])
  onResize(event: UIEvent): void {
    console.log('BODY_DECORATED');
  }
}
"#;

    const RS_PATH: &str = "rs/src/merge.rs";

    /// A multi-line Rust signature with `where`, an `impl` with `where`, and
    /// a `#[cfg(test)]` module.
    const RS: &str = r#"use std::collections::BTreeMap as Map;

pub fn merge<K, V>(
    left: Map<K, V>,
    right: Map<K, V>,
) -> Map<K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    let _marker = "BODY_RUST";
    let _ = right;
    left
}

pub struct Cache<T> {
    items: Vec<T>,
}

impl<T: Clone> Cache<T>
where
    T: Default,
{
    pub fn get(&self) -> T {
        T::default() // BODY_IMPL
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn checks() {
        let _ = "BODY_TEST";
    }
}
"#;

    const PY_PATH: &str = "py/app/handlers.py";

    const PY: &str = r#"import json


def decorator(f):
    return f  # BODY_PY_DECO


@decorator
def handler(event, context):
    return json.dumps("BODY_PY")
"#;

    fn files(java: &str) -> Vec<(&'static str, String)> {
        vec![
            (JAVA_PATH, java.to_string()),
            (TS_PATH, TS.to_string()),
            (RS_PATH, RS.to_string()),
            (PY_PATH, PY.to_string()),
            (
                "db/schema.sql",
                "CREATE TABLE things (id INT);\n".to_string(),
            ),
        ]
    }

    fn indexed(tag: &str) -> (AppState, std::path::PathBuf) {
        let java = java();
        let owned = files(&java);
        let refs: Vec<(&str, &str)> = owned.iter().map(|(p, t)| (*p, t.as_str())).collect();
        indexed_files(tag, &refs)
    }

    fn skeleton(state: &AppState, file: &str) -> Result<String, String> {
        do_skeleton(state, &SkeletonQuery::of(file))
    }

    /// The 1-based line of the first line of `text` that contains `needle`.
    fn line_of(text: &str, needle: &str) -> usize {
        text.lines().position(|l| l.contains(needle)).unwrap() + 1
    }

    /// (a) Java: the class, its field, constructor and methods one level
    /// down, the inner class's members two; signatures without annotations
    /// or bodies; ranges from the annotation to the closing brace.
    #[test]
    fn a_java_class_with_an_inner_class() {
        let (state, repo) = indexed("skel_java");
        let src = java();
        let out = skeleton(&state, JAVA_PATH).unwrap();
        let total = src.lines().count();
        assert!(
            out.starts_with(&format!("{JAVA_PATH}: 10 symbols\n")),
            "{out}"
        );
        assert!(
            out.contains("imports (3): java.util.List, java.util.Map, app.core.repo.OrderRepo\n"),
            "{out}"
        );
        let class = line_of(&src, "@ApplicationScoped");
        let end = total;
        assert!(
            out.contains(&format!("\npublic class OrderService [{class}-{end}]\n")),
            "{out}"
        );
        let field = line_of(&src, "private final OrderRepo");
        assert!(
            out.contains(&format!(
                "\n  private final OrderRepo repo [{field}-{field}]\n"
            )),
            "{out}"
        );
        let ctor = line_of(&src, "public OrderService(");
        assert!(
            out.contains(&format!(
                "\n  public OrderService(OrderRepo repo) [{ctor}-{}]\n",
                ctor + 2
            )),
            "{out}"
        );
        let place = line_of(&src, "@Override");
        assert!(
            out.contains(&format!(
                "\n  public List<String> place(String id, Map<String, List<Integer>> lines) [{place}-{}]\n",
                place + 7
            )),
            "{out}"
        );
        let inner = line_of(&src, "static class Line");
        assert!(
            out.contains(&format!("\n  static class Line [{inner}-{}]\n", end - 1)),
            "{out}"
        );
        let qty = line_of(&src, "int qty;");
        assert!(
            out.contains(&format!("\n    int qty [{qty}-{qty}]\n")),
            "{out}"
        );
        let ictor = line_of(&src, "Line(int qty)");
        assert!(
            out.contains(&format!("\n    Line(int qty) [{ictor}-{}]\n", ictor + 2)),
            "{out}"
        );
        let total_m = line_of(&src, "int total()");
        assert!(
            out.contains(&format!("\n    int total() [{total_m}-{}]\n", total_m + 2)),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Multi-line signatures: an annotation longer than the 200 characters
    /// the index keeps no longer eats the signature (the extractor skips
    /// leading annotations before the cap, review of TASK-014 MAJOR 3), in
    /// the skeleton and in the repo map; long generics and a parameter list
    /// over lines are one line.
    #[test]
    fn multi_line_signatures_are_one_line() {
        let (state, repo) = indexed("skel_multiline");
        let src = java();
        let java_out = skeleton(&state, JAVA_PATH).unwrap();
        let at = line_of(&src, "@Operation(");
        assert!(
            java_out.contains(&format!("\n  public void annotated() [{at}-{}]\n", at + 3)),
            "{java_out}"
        );
        let map = do_repo_map(
            &state,
            &crate::repo_map::RepoMapQuery {
                focus_symbols: vec!["annotated".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(map.contains("public void annotated()"), "{map}");
        assert!(!map.contains("sssss"), "{map}");
        assert!(!java_out.contains("sssss"), "{java_out}");
        let g = line_of(&src, "public <T extends");
        assert!(
            java_out.contains(&format!(
                "\n  public <T extends Comparable<? super T>> Map<String, List<Map<Integer, T>>> \
                 group(List<T> items, java.util.function.Function<T, String> key) [{g}-{}]\n",
                g + 4
            )),
            "{java_out}"
        );
        let rs = skeleton(&state, RS_PATH).unwrap();
        let m = line_of(RS, "pub fn merge");
        assert!(
            rs.contains(&format!(
                "\npub fn merge<K, V>(left: Map<K, V>, right: Map<K, V>) -> Map<K, V> where K: \
                 Ord + Clone, V: Clone, [{m}-{}]\n",
                RS.lines().position(|l| l == "    left").unwrap() + 2
            )),
            "{rs}"
        );
        // The extractor makes an `impl` a member of its type (`parent_id`):
        // the tree follows it, the range says where it is.
        let i = line_of(RS, "impl<T: Clone>");
        assert!(
            rs.contains(&format!(
                "\n  impl<T: Clone> Cache<T> where T: Default, [{i}-"
            )),
            "{rs}"
        );
        let get = line_of(RS, "pub fn get");
        assert!(
            rs.contains(&format!(
                "\n    pub fn get(&self) -> T [{get}-{}]\n",
                get + 2
            )),
            "{rs}"
        );
        // A TypeScript member's decorator is no part of its node: the range
        // starts at the name and the decorator is not shown.
        let ts = skeleton(&state, TS_PATH).unwrap();
        let on = line_of(TS, "onResize(");
        assert!(
            ts.contains(&format!(
                "\n  onResize(event: UIEvent): void [{on}-{}]\n",
                on + 2
            )),
            "{ts}"
        );
        assert!(
            !ts.contains("@HostListener") && !ts.contains("@Injectable"),
            "{ts}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// (b) TypeScript: the exported arrow-const is a function at the top,
    /// its signature up to the arrow; the decorated class and its members.
    #[test]
    fn a_typescript_arrow_const() {
        let (state, repo) = indexed("skel_ts");
        let out = skeleton(&state, TS_PATH).unwrap();
        let a = line_of(TS, "export const authInterceptor");
        assert!(
            out.contains(&format!(
                "\nconst authInterceptor: HttpInterceptorFn = (req, next) => [{a}-{}]\n",
                a + 3
            )),
            "{out}"
        );
        assert!(out.contains("\nclass SessionStore ["), "{out}");
        let c = line_of(TS, "constructor(");
        assert!(
            out.contains(&format!(
                "\n  constructor(private readonly http: HttpClient) [{c}-{}]\n",
                c + 2
            )),
            "{out}"
        );
        let f = line_of(TS, "@Input()");
        assert!(out.contains(&format!("\n  name [{f}-{f}]\n")), "{out}");
        // One target per imported name, by line and then by text.
        assert!(
            out.contains(
                "imports (5): @angular/core#HostListener, @angular/core#Injectable, \
                 @angular/core#Input, @angular/common/http#HttpClient, \
                 @angular/common/http#HttpInterceptorFn\n"
            ),
            "{out}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// (e) No line of a body ever appears: not a statement, not a field's
    /// initializer, not a decorator's argument, in any language.
    #[test]
    fn no_body_line_ever_appears() {
        let (state, repo) = indexed("skel_bodies");
        let java = java();
        for (path, src) in [
            (JAVA_PATH, java.as_str()),
            (TS_PATH, TS),
            (RS_PATH, RS),
            (PY_PATH, PY),
        ] {
            let out = skeleton(&state, path).unwrap();
            assert!(!out.contains("BODY_"), "{path}:\n{out}");
            for body in [
                "return",
                "console.log",
                "this.",
                "System.out",
                "let _",
                "providedIn",
            ] {
                assert!(!out.contains(body), "{path}: {body}\n{out}");
            }
            // Every shown line is a symbol line, the header or the imports.
            for l in out.lines().skip(1) {
                assert!(
                    l.starts_with("imports (") || l.trim_end().ends_with(']'),
                    "{path}: {l}\n{out}"
                );
            }
            assert!(out.lines().count() > 2, "{path}:\n{out}\n{src}");
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Rust test code is marked `[test]`, not hidden; Python's decorated
    /// function keeps its own signature.
    #[test]
    fn test_code_is_marked_and_python_is_drawn() {
        let (state, repo) = indexed("skel_tests");
        let rs = skeleton(&state, RS_PATH).unwrap();
        let t = line_of(RS, "mod tests");
        assert!(rs.contains(&format!("\nmod tests [{t}-")), "{rs}");
        assert!(rs.contains("] [test]\n  fn checks() ["), "{rs}");
        assert!(rs.trim_end().ends_with("[test]"), "{rs}");
        let py = skeleton(&state, PY_PATH).unwrap();
        let h = line_of(PY, "def handler");
        assert!(
            py.contains(&format!("\ndef handler(event, context): [{h}-")),
            "{py}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// (c) A file the index does not hold: an error naming the nearest
    /// indexed paths, and whether it is on disk; indexed as text: read_file.
    /// The path is resolved as read_file resolves it.
    #[test]
    fn a_file_not_indexed_is_an_error_with_nearby_paths() {
        let (state, repo) = indexed("skel_missing");
        let e = skeleton(&state, "src/main/java/app/core/OrderServise.java").unwrap_err();
        assert!(e.contains("is not in the index"), "{e}");
        assert!(e.contains("no such file"), "{e}");
        assert!(e.contains(JAVA_PATH), "{e}");
        let e = skeleton(&state, "OrderService.java").unwrap_err();
        assert!(
            e.contains(&format!("nearest indexed paths: {JAVA_PATH}")),
            "{e}"
        );
        std::fs::write(repo.join("web/src/app/draft.ts"), "export const x = 1;\n").unwrap();
        let e = skeleton(&state, "web/src/app/draft.ts").unwrap_err();
        assert!(e.contains("exists on disk"), "{e}");
        assert!(e.contains(TS_PATH), "{e}");
        let e = skeleton(&state, "db/schema.sql").unwrap_err();
        assert!(
            e.contains("has no symbols") && e.contains("read_file"),
            "{e}"
        );
        // Absolute inside the repository, or `./`: the same file.
        let abs = repo.join(JAVA_PATH).to_string_lossy().into_owned();
        let rel = skeleton(&state, JAVA_PATH).unwrap();
        assert_eq!(skeleton(&state, &abs).unwrap(), rel);
        assert_eq!(skeleton(&state, &format!("./{JAVA_PATH}")).unwrap(), rel);
        let e = skeleton(&state, "/elsewhere/X.java").unwrap_err();
        assert!(e.contains("outside this repository"), "{e}");
        let e = skeleton(&state, " ").unwrap_err();
        assert!(e.contains("file is required"), "{e}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// (d) A branch indexed by an older extractor, or with no symbol rows:
    /// an error that asks for `devctx index --full`, never an empty skeleton.
    #[test]
    fn an_old_index_asks_for_a_full_reindex() {
        let (state, repo) = indexed("skel_stale");
        {
            let store = state.open_store().unwrap();
            let (_, b) = state.repo_branch().unwrap();
            store
                .set_index_meta(
                    &state.repo_path(),
                    &b,
                    devctx_store::EXTRACTOR_META_KEY,
                    "v0-old",
                )
                .unwrap();
        }
        let e = skeleton(&state, JAVA_PATH).unwrap_err();
        assert!(e.contains("devctx index --full"), "{e}");
        let _ = std::fs::remove_dir_all(&repo);
        let (state, repo) = indexed_files(
            "skel_norows",
            &[("db/schema.sql", "CREATE TABLE things (id INT);\n")],
        );
        let e = skeleton(&state, "db/schema.sql").unwrap_err();
        assert!(e.contains("devctx index --full"), "{e}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// The measurement of TASK-014 (ignored): indexes `$DEVCTX_SKEL_BENCH_REPO`
    /// into `$DEVCTX_SKEL_BENCH_STATE` with a model-less embedder (the symbol
    /// graph is real; vectors do not matter to a skeleton), then for each file
    /// of `$DEVCTX_SKEL_BENCH_FILES` (comma-separated, repo-relative; an
    /// optional `alias=` in front names it in the output) prints its lines,
    /// the tokens of the whole file (what `read_file` costs uncapped), the
    /// tokens of its skeleton uncapped and as the tool answers it (the default
    /// budget), and `$DEVCTX_SKEL_BENCH_RUNS` (40) warm calls' p50 / p95;
    /// with `$DEVCTX_SKEL_BENCH_PRINT`, each skeleton on stderr.
    ///
    /// `cargo test --release -p devctx-mcp --lib skeleton_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn skeleton_bench() {
        use crate::repo_map::tokens;
        let var = |k: &str| std::env::var(k).ok();
        let (Some(repo), Some(state_dir), Some(files)) = (
            var("DEVCTX_SKEL_BENCH_REPO"),
            var("DEVCTX_SKEL_BENCH_STATE"),
            var("DEVCTX_SKEL_BENCH_FILES"),
        ) else {
            eprintln!("set DEVCTX_SKEL_BENCH_REPO, _STATE and _FILES");
            return;
        };
        let runs: usize = var("DEVCTX_SKEL_BENCH_RUNS")
            .and_then(|r| r.parse().ok())
            .unwrap_or(40);
        let mut cfg = ProjectConfig::default();
        cfg.project.path = repo.clone();
        cfg.state_dir = state_dir;
        cfg.storage.hnsw = false;
        let state = AppState::build(cfg).unwrap();
        let dim = configured_dimension(&state.cfg);
        *state.embedder.lock().unwrap() = Some(Cached {
            value: Arc::new(Fake(dim)),
            last_used: Instant::now(),
            key: "fake/fake".into(),
            loaded_at: procmem::unix_now(),
        });
        let t = Instant::now();
        do_index(&state, false).unwrap();
        eprintln!("index: {:.1} s", t.elapsed().as_secs_f64());
        println!("| file | lines | file tokens | skeleton (uncapped) | % | skeleton (tool) | % | symbols | p50 ms | p95 ms |");
        for entry in files.split(',').map(str::trim).filter(|f| !f.is_empty()) {
            let (alias, file) = entry.split_once('=').unwrap_or((entry, entry));
            let text = std::fs::read_to_string(std::path::Path::new(&repo).join(file)).unwrap();
            let file_tokens = tokens(&text);
            let store = state.open_store().unwrap();
            let chosen = graph_branch(&state, &store).unwrap();
            let q = SkeletonQuery::of(file);
            let uncapped = super::skeleton_on(&store, &chosen, &state.root, &q, 0).unwrap();
            drop(store);
            if var("DEVCTX_SKEL_BENCH_PRINT").is_some() {
                eprintln!("{uncapped}");
            }
            let tool = do_skeleton(&state, &q).unwrap();
            let mut ms: Vec<f64> = (0..runs)
                .map(|_| {
                    let t = Instant::now();
                    do_skeleton(&state, &q).unwrap();
                    t.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |p: f64| ms[((ms.len() as f64 * p).ceil() as usize).clamp(1, ms.len()) - 1];
            let symbols = uncapped
                .lines()
                .next()
                .and_then(|l| l.rsplit(": ").next())
                .unwrap_or("")
                .to_string();
            println!(
                "| {alias} | {} | {file_tokens} | {} | {:.1} | {} | {:.1} | {symbols} | {:.1} | {:.1} |",
                text.lines().count(),
                tokens(&uncapped),
                100.0 * tokens(&uncapped) as f64 / file_tokens as f64,
                tokens(&tool),
                100.0 * tokens(&tool) as f64 / file_tokens as f64,
                pct(0.5),
                pct(0.95),
            );
        }
    }

    /// The same file through the local CLI's entry point: the same text.
    #[test]
    fn the_cli_entry_point_draws_the_same() {
        let (state, repo) = indexed("skel_at");
        let via_state = skeleton(&state, TS_PATH).unwrap();
        let store = state.open_store().unwrap();
        let via_cli = skeleton_at(&store, &repo, None, &SkeletonQuery::of(TS_PATH)).unwrap();
        assert_eq!(via_cli, via_state);
        drop(store);
        let _ = std::fs::remove_dir_all(&repo);
    }
}
