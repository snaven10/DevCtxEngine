//! The extractor's output is versioned, not its code (PLAN-009 DD-3).
//!
//! `EXTRACTOR_VERSION` plus a hash of `languages/*.json` decide whether an
//! index is current. A change to the Rust side of the extraction that alters
//! what a re-parse produces (ids, qualified names, kinds) is invisible to the
//! hash, and must bump the version by hand — this test is what makes that
//! impossible to forget. It renders, for a fixture in every language, each
//! symbol's kind, qualified name and id, and compares it with the committed
//! snapshot in `tests/golden/extractor.txt`, whose first line records the
//! version it was generated under.
//!
//! - Output changed, version not bumped → fails: bump `EXTRACTOR_VERSION`
//!   and regenerate.
//! - Version bumped, snapshot not regenerated → fails: regenerate.
//!
//! Regenerate with `DEVCTX_UPDATE_GOLDEN=1 cargo test -p devctx-parse --test
//! extractor_golden`.

use devctx_parse::{parse, Lang, EXTRACTOR_VERSION};

const GOLDEN: &str = "tests/golden/extractor.txt";
const REGEN: &str = "DEVCTX_UPDATE_GOLDEN=1 cargo test -p devctx-parse --test extractor_golden";

const FIXTURES: &[(&str, &str, &str)] = &[
    (
        "java",
        "java/OrderService.java",
        "src/main/java/com/example/shop/OrderService.java",
    ),
    (
        "typescript",
        "typescript/token.interceptor.ts",
        "libs/auth/src/token.interceptor.ts",
    ),
    ("tsx", "tsx/Button.tsx", "web/Button.tsx"),
    ("javascript", "javascript/app.js", "web/app.js"),
    ("python", "python/orders.py", "shop/orders.py"),
    ("rust", "rust/cache.rs", "crates/demo/src/cache.rs"),
    ("go", "go/server.go", "api/server.go"),
];

fn render() -> String {
    let root = env!("CARGO_MANIFEST_DIR");
    let mut out = String::new();
    for (lang, fixture, path) in FIXTURES {
        let src = std::fs::read_to_string(format!("{root}/tests/fixtures/{fixture}")).unwrap();
        let mut pf = parse(Lang::named(lang).unwrap(), &src).unwrap();
        pf.assign_ids("repo", path);
        for s in std::iter::once(&pf.file_symbol).chain(&pf.symbols) {
            out.push_str(&format!(
                "{path}\t{}\t{}\t{}\n",
                s.kind,
                s.qualified,
                devctx_parse::symbol_id::sym_hex(s.id)
            ));
        }
    }
    out
}

#[test]
fn the_extractor_output_matches_its_version() {
    let path = format!("{}/{GOLDEN}", env!("CARGO_MANIFEST_DIR"));
    let now = render();
    if std::env::var_os("DEVCTX_UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, format!("version {EXTRACTOR_VERSION}\n{now}")).unwrap();
        return;
    }
    let golden = std::fs::read_to_string(&path).unwrap_or_default();
    let (head, body) = golden.split_once('\n').unwrap_or(("", ""));
    let version: Option<u32> = head.strip_prefix("version ").and_then(|v| v.parse().ok());
    if body != now {
        assert_ne!(
            version,
            Some(EXTRACTOR_VERSION),
            "la salida del extractor cambió: subí EXTRACTOR_VERSION y regenerá el golden ({REGEN})"
        );
        panic!("EXTRACTOR_VERSION cambió y el golden no: regenerá el golden ({REGEN})");
    }
    assert_eq!(
        version,
        Some(EXTRACTOR_VERSION),
        "EXTRACTOR_VERSION cambió y el golden no: regenerá el golden ({REGEN})"
    );
}
