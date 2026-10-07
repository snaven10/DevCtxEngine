//! Stable symbol identity (PLAN-009 DD-3).
//!
//! A symbol's id is a 64-bit hash of *where* it is (repository and file) and
//! *what* it is (kind class, qualified name, disambiguator) — never of the
//! branch, the line or the body. So the same method on `main` and on a feature
//! branch has one id, a branch copy needs no re-tagging, and moving a function
//! within its file keeps the id that a memory or an agent wrote down.
//!
//! ```text
//! id = fnv1a64(repo ␟ file ␟ kind_class ␟ qualified ␟ disambiguator)
//! ```
//!
//! One run over the five fields. A renamed file is a new set of symbols:
//! its rows are deleted and the reindex writes them at the new path. (A
//! first cut hashed the file and the symbol apart and XORed them, so a rename
//! could re-key the rows without a parse; the qualified name carries the path
//! too once TASK-004 lands, which breaks that, and nothing renamed through it
//! anyway — the pipeline indexes a rename as a delete plus an add.)
//!
//! Lives in `devctx-core` because both the parser (which assigns ids) and the
//! store (which names a file's own symbol) need the same function.

/// The ASCII unit separator: keeps `["ab", "c"]` apart from `["a", "bc"]`.
const SEP: u8 = 0x1f;

/// The `kind` of the per-file symbol: the source of module-level calls and the
/// root of a file's containment.
pub const FILE_KIND: &str = "file";

/// FNV-1a 64 over `parts`, separated by [`SEP`]. Stable across platforms and
/// releases, unlike `DefaultHasher`.
pub fn fnv1a64(parts: &[&str]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            h = step(h, SEP);
        }
        for b in part.as_bytes() {
            h = step(h, *b);
        }
    }
    h
}

fn step(h: u64, b: u8) -> u64 {
    (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
}

/// The id of a symbol (see the module docs). `kind_class` is
/// [`kind_class`]`(kind)`, so a re-classification between interchangeable
/// kinds (`function` ↔ `method`) keeps the id.
pub fn symbol_id(
    repo: &str,
    file: &str,
    kind_class: &str,
    qualified: &str,
    disambiguator: &str,
) -> u64 {
    fnv1a64(&[repo, file, kind_class, qualified, disambiguator])
}

/// The id of a file's own symbol, whose qualified name is the path itself.
pub fn file_symbol_id(repo: &str, file: &str) -> u64 {
    symbol_id(repo, file, FILE_KIND, file, "")
}

/// Kinds that are the same thing to a caller share one class, so the id does
/// not move when the extractor re-labels one as the other.
pub fn kind_class(kind: &str) -> &str {
    match kind {
        "function" | "method" | "constructor" => "callable",
        "class" | "interface" | "enum" | "record" | "struct" | "trait" | "type" => "type",
        other => other,
    }
}

/// An id as it appears in JSON: 16 lowercase hex digits.
pub fn sym_hex(id: u64) -> String {
    format!("{id:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = "8c7a385edfa98a4f";

    #[test]
    fn the_id_is_a_pure_function_of_its_inputs() {
        let a = symbol_id("repo", "src/a.rs", "callable", "Point.mag", "");
        assert_eq!(
            a,
            symbol_id("repo", "src/a.rs", "callable", "Point.mag", "")
        );
        assert_ne!(
            a,
            symbol_id("repo", "src/b.rs", "callable", "Point.mag", "")
        );
        assert_ne!(
            a,
            symbol_id("other", "src/a.rs", "callable", "Point.mag", "")
        );
        assert_ne!(a, symbol_id("repo", "src/a.rs", "type", "Point.mag", ""));
        assert_ne!(
            a,
            symbol_id("repo", "src/a.rs", "callable", "Point.mag", "i32")
        );
        // Pinned: the value is part of the index format.
        assert_eq!(fnv1a64(&[]), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(&["a"]), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(fnv1a64(&["ab", "c"]), fnv1a64(&["a", "bc"]));
    }

    /// The id is part of the index format: a change here re-keys every
    /// stored id and needs an `EXTRACTOR_VERSION` bump.
    #[test]
    fn the_id_format_is_pinned() {
        assert_eq!(
            sym_hex(symbol_id("repo", "src/a.rs", "callable", "Point.mag", "")),
            GOLDEN
        );
        assert_eq!(
            file_symbol_id("repo", "src/a.rs"),
            fnv1a64(&["repo", "src/a.rs", "file", "src/a.rs", ""])
        );
    }

    #[test]
    fn interchangeable_kinds_share_a_class() {
        assert_eq!(kind_class("function"), kind_class("method"));
        assert_eq!(kind_class("class"), kind_class("struct"));
        assert_eq!(kind_class("file"), "file");
        assert_eq!(sym_hex(0xab), "00000000000000ab");
    }
}
