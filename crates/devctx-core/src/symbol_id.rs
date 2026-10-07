//! Stable symbol identity (PLAN-009 DD-3).
//!
//! A symbol's id is a 64-bit hash of *where* it is (repository and file) and
//! *what* it is (kind class, qualified name, disambiguator) — never of the
//! branch, the line or the body. So the same method on `main` and on a feature
//! branch has one id, a branch copy needs no re-tagging, and moving a function
//! within its file keeps the id that a memory or an agent wrote down.
//!
//! The two halves are hashed apart and combined with XOR:
//!
//! ```text
//! id = fnv1a64("F" ␟ repo ␟ file)  XOR  fnv1a64("S" ␟ kind_class ␟ qualified ␟ disambiguator)
//! ```
//!
//! rather than as one run over all five fields. Same inputs, same stability,
//! and one property a single run cannot give: renaming a file changes every id
//! in it by the same mask ([`rename_delta`]), so the store can re-key a renamed
//! file's rows without re-parsing it — the disambiguator, which it does not
//! keep, cancels out. It also makes two symbols of one file collide exactly
//! when their *what* halves do, wherever the file lives, so the ordinal that
//! settles a collision is the same before and after a rename.
//!
//! Lives in `devctx-core` because both the parser (which assigns ids) and the
//! store (which re-keys them on a rename) need the same function.

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

/// The *where* half of an id.
pub fn file_part(repo: &str, file: &str) -> u64 {
    fnv1a64(&["F", repo, file])
}

/// The *what* half of an id.
pub fn symbol_part(kind_class: &str, qualified: &str, disambiguator: &str) -> u64 {
    fnv1a64(&["S", kind_class, qualified, disambiguator])
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
    file_part(repo, file) ^ symbol_part(kind_class, qualified, disambiguator)
}

/// The id of a file's own symbol, whose qualified name is the path itself.
pub fn file_symbol_id(repo: &str, file: &str) -> u64 {
    symbol_id(repo, file, FILE_KIND, file, "")
}

/// What to XOR a symbol id with when its file moves from `old` to `new`.
///
/// Exact for every symbol but the file's own ([`file_symbol_id`]), whose
/// qualified name is the path and so changes with it.
pub fn rename_delta(repo: &str, old: &str, new: &str) -> u64 {
    file_part(repo, old) ^ file_part(repo, new)
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

    #[test]
    fn a_rename_shifts_every_id_by_one_mask() {
        let id = |file: &str, q: &str| symbol_id("r", file, "callable", q, "Long,String");
        let d = rename_delta("r", "a/Old.java", "b/New.java");
        assert_eq!(id("a/Old.java", "Svc.run") ^ d, id("b/New.java", "Svc.run"));
        assert_eq!(
            id("a/Old.java", "Svc.stop") ^ d,
            id("b/New.java", "Svc.stop")
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
