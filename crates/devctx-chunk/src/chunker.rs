//! The semantic multi-level chunker: file → class → function → block.
//!
//! Never splits mid-symbol. Small symbols are grouped; large functions are split
//! into block chunks that re-include the signature line. Mirrors the legacy
//! `semantic_chunker.py` (rewrite plan §3).

use std::path::Path;

use devctx_parse::{ParsedFile, Symbol};

use crate::chunk::{estimate_tokens, Chunk, ChunkConfig};

/// Symbol kinds that act as class-level containers.
const CONTAINER_KINDS: &[&str] = &["class", "struct", "enum", "trait", "interface"];

/// Chunk one parsed file into embeddable chunks.
pub fn chunk_file(path: &str, source: &str, parsed: &ParsedFile, cfg: &ChunkConfig) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let total_lines = source.lines().count().max(1) as u32;

    chunks.push(file_chunk(path, parsed, total_lines));

    for sym in parsed.symbols.iter().filter(|s| is_container(s)) {
        chunks.push(class_chunk(path, source, sym, parsed));
    }

    chunks.extend(doc_chunks(path, source, parsed));

    let mut small: Vec<PendingSmall> = Vec::new();
    let mut small_tokens = 0usize;

    for sym in parsed.symbols.iter().filter(|s| is_callable(s)) {
        let body = slice(source, sym.start_byte, sym.end_byte);
        let header = context_header(path, sym.parent.as_deref(), &sym.name);
        let toks = estimate_tokens(body);

        if toks >= cfg.large_function_threshold {
            flush_small(&mut chunks, &mut small, &mut small_tokens);
            chunks.extend(block_chunks(&header, body, sym, cfg));
        } else if toks < cfg.min_chunk_tokens {
            let text = format!("{header}\n{body}");
            small_tokens += estimate_tokens(&text);
            small.push(PendingSmall {
                text,
                name: sym.name.clone(),
                start: sym.start_line,
                end: sym.end_line,
            });
            if small_tokens >= cfg.max_chunk_tokens {
                flush_small(&mut chunks, &mut small, &mut small_tokens);
            }
        } else {
            flush_small(&mut chunks, &mut small, &mut small_tokens);
            let text = format!("{header}\n{body}");
            chunks.push(Chunk::new(
                text,
                "function",
                &sym.name,
                &sym.kind,
                sym.start_line,
                sym.end_line,
                header,
            ));
        }
    }
    flush_small(&mut chunks, &mut small, &mut small_tokens);

    chunks
}

/// Chunk a raw-text (non-parseable) file: one `file` chunk when small, else
/// line-interval `block` chunks. No symbols/headers.
pub fn chunk_raw_text(path: &str, content: &str, cfg: &ChunkConfig) -> Vec<Chunk> {
    if content.trim().is_empty() {
        return Vec::new();
    }
    let base = basename(path);
    let total_lines = content.lines().count().max(1) as u32;

    if estimate_tokens(content) <= cfg.max_chunk_tokens {
        return vec![Chunk::new(
            content.to_string(),
            "file",
            base,
            "file",
            1,
            total_lines,
            String::new(),
        )];
    }

    let lines: Vec<&str> = content.lines().collect();
    let parts = (estimate_tokens(content) / cfg.max_chunk_tokens).max(2);
    let target = (lines.len() / parts).max(1);
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut end = (i + target).min(lines.len());
        if end < lines.len() {
            end = snap_to_blank(&lines, end, 3);
        }
        if end <= i {
            end = (i + target).min(lines.len()).max(i + 1);
        }
        let text = lines[i..end].join("\n");
        out.push(Chunk::new(
            text,
            "block",
            base,
            "file",
            i as u32 + 1,
            end as u32,
            String::new(),
        ));
        if end >= lines.len() {
            break;
        }
        // Step back a little, so a paragraph that straddles the cut survives
        // whole on one side of it. `max(i + 1)` is what guarantees the loop
        // still advances when the overlap is as large as the block.
        i = end.saturating_sub(overlap_lines(target)).max(i + 1);
    }
    out
}

/// How far each raw-text block reaches back into the one before it.
///
/// Code is cut where the language says a symbol ends, so its boundaries carry
/// meaning and losing nothing across them is free. Prose is cut at whatever
/// blank line falls near the target size, which is an arbitrary place: a
/// sentence explaining a decision can begin in one block and finish in the
/// next, leaving neither able to answer the question it addresses. Repeating a
/// few lines on each side is the standard remedy.
///
/// It is not free. Every overlapped line is indexed twice, and a larger index
/// means more chunks competing for the same top-20 — which is measurable here,
/// so this stays small enough to rescue a paragraph and no larger.
fn overlap_lines(target: usize) -> usize {
    (target / 8).clamp(1, 6)
}

/// Shortest doc comment worth its own chunk, in characters. `/// The name.`
/// restates the signature; a sentence about what happens does not.
const MIN_DOC_CHARS: usize = 60;

/// One chunk per documented symbol, holding the prose above it.
///
/// Measured on this repository, questions that name a thing — an identifier, a
/// term — are found 13 times in 14. Questions about behaviour, "what happens
/// when two processes open the same database", are found 5 times in 10, in
/// English and Spanish alike. The reason is that nothing in the index describes
/// behaviour: the answering code says `pid_alive` and `elapsed`, and shares no
/// word with the question. The doc comment above it is the only place anyone
/// wrote what the code *does*.
///
/// Kept apart from the code rather than folded into it. Prepending the prose to
/// the body was tried first and measured worse — a code chunk carrying a
/// paragraph reads like the documentation it now competes with, and both lose.
/// Separate chunks let a behaviour question match prose and an identifier
/// question match code, which is what each is good at.
fn doc_chunks(path: &str, source: &str, parsed: &ParsedFile) -> Vec<Chunk> {
    parsed
        .symbols
        .iter()
        .filter_map(|sym| {
            let doc = slice(source, sym.doc_start_byte, sym.start_byte).trim();
            if doc.len() < MIN_DOC_CHARS {
                return None;
            }
            let header = context_header(path, sym.parent.as_deref(), &sym.name);
            // The signature travels with the prose: it is what ties "what
            // happens when the lock is held" to the function that does it.
            let signature = slice(source, sym.start_byte, sym.end_byte)
                .lines()
                .next()
                .unwrap_or("")
                .trim_end();
            Some(Chunk::new(
                format!("{header}\n{doc}\n{signature}"),
                "doc",
                &sym.name,
                &sym.kind,
                sym.doc_start_line,
                sym.start_line,
                header,
            ))
        })
        .collect()
}

struct PendingSmall {
    text: String,
    name: String,
    start: u32,
    end: u32,
}

fn is_container(s: &Symbol) -> bool {
    CONTAINER_KINDS.contains(&s.kind.as_str())
}

fn is_callable(s: &Symbol) -> bool {
    s.kind == "function" || s.kind == "method"
}

fn file_chunk(path: &str, parsed: &ParsedFile, total_lines: u32) -> Chunk {
    let base = basename(path);
    let mut text = format!("# File: {base}\n# Imports:\n");
    if parsed.imports.is_empty() {
        text.push_str("(none)\n");
    } else {
        for imp in &parsed.imports {
            text.push_str(&imp.statement);
            text.push('\n');
        }
    }
    text.push_str("# Symbols:\n");
    for s in &parsed.symbols {
        text.push_str(&format!("- {} ({})\n", s.name, s.kind));
    }
    Chunk::new(text, "file", base, "file", 1, total_lines, String::new())
}

fn class_chunk(path: &str, source: &str, sym: &Symbol, parsed: &ParsedFile) -> Chunk {
    let body = slice(source, sym.start_byte, sym.end_byte);
    let signature = body.lines().next().unwrap_or("").trim_end();
    let methods: Vec<&str> = parsed
        .symbols
        .iter()
        .filter(|s| is_callable(s) && s.parent.as_deref() == Some(sym.name.as_str()))
        .map(|s| s.name.as_str())
        .collect();

    let mut text = signature.to_string();
    if !methods.is_empty() {
        text.push_str(&format!("\n# methods: {}", methods.join(", ")));
    }
    let header = context_header(path, sym.parent.as_deref(), &sym.name);
    Chunk::new(
        text,
        "class",
        &sym.name,
        &sym.kind,
        sym.start_line,
        sym.end_line,
        header,
    )
}

fn block_chunks(header: &str, body: &str, sym: &Symbol, cfg: &ChunkConfig) -> Vec<Chunk> {
    let lines: Vec<&str> = body.lines().collect();
    let sig = lines.first().copied().unwrap_or("");
    let total = estimate_tokens(body);
    let parts = (total / cfg.max_chunk_tokens).max(2);
    let target = (lines.len() / parts).max(1);

    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut end = (i + target).min(lines.len());
        if end < lines.len() {
            end = snap_to_blank(&lines, end, 3);
        }
        if end <= i {
            end = (i + target).min(lines.len()).max(i + 1);
        }
        let slice = lines[i..end].join("\n");
        let text = if i == 0 {
            format!("{header}\n{slice}")
        } else {
            format!("{header}\n{sig}\n{slice}")
        };
        let start_line = sym.start_line + i as u32;
        let end_line = sym.start_line + (end.saturating_sub(1)) as u32;
        out.push(Chunk::new(
            text,
            "block",
            &sym.name,
            &sym.kind,
            start_line,
            end_line,
            header.to_string(),
        ));
        i = end;
    }
    out
}

/// Find the nearest blank line to `idx` within `window` lines.
fn snap_to_blank(lines: &[&str], idx: usize, window: usize) -> usize {
    for k in 0..=window {
        if idx >= k && lines[idx - k].trim().is_empty() {
            return idx - k;
        }
        if idx + k < lines.len() && lines[idx + k].trim().is_empty() {
            return idx + k;
        }
    }
    idx
}

/// How many names a grouped chunk lists before summarising the rest as `+n`.
const GROUPED_NAME_CAP: usize = 4;

fn flush_small(chunks: &mut Vec<Chunk>, small: &mut Vec<PendingSmall>, tokens: &mut usize) {
    if small.is_empty() {
        return;
    }
    let start = small.first().map(|p| p.start).unwrap_or(1);
    let end = small.last().map(|p| p.end).unwrap_or(start);
    let text = small
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    // Name the group after what is in it. Small functions are common enough
    // that leaving this blank would strip the symbol from a large share of all
    // search hits — the one field that tells a reader what they found.
    let names: Vec<&str> = small
        .iter()
        .map(|p| p.name.as_str())
        .filter(|n| !n.is_empty())
        .take(GROUPED_NAME_CAP)
        .collect();
    let symbol = if names.len() < small.len() {
        format!("{} +{}", names.join(", "), small.len() - names.len())
    } else {
        names.join(", ")
    };
    chunks.push(Chunk::new(
        text,
        "function",
        symbol,
        "grouped",
        start,
        end,
        String::new(),
    ));
    small.clear();
    *tokens = 0;
}

fn context_header(path: &str, parent: Option<&str>, name: &str) -> String {
    let base = basename(path);
    match parent {
        Some(p) => format!("# {base} > {p} > {name}"),
        None => format!("# {base} > {name}"),
    }
}

fn basename(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

fn slice(source: &str, start: usize, end: usize) -> &str {
    source.get(start..end).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(n: u32) -> Vec<PendingSmall> {
        (1..=n)
            .map(|i| PendingSmall {
                text: format!("fn f{i}() {{}}"),
                name: format!("f{i}"),
                start: i,
                end: i,
            })
            .collect()
    }

    /// A grouped chunk must still say what is inside it: small functions are
    /// common, so leaving the symbol blank would strip the name from a large
    /// share of all search hits.
    #[test]
    fn a_grouped_chunk_is_named_after_its_contents() {
        let (mut chunks, mut tokens) = (Vec::new(), 0);
        let mut small = pending(3);
        flush_small(&mut chunks, &mut small, &mut tokens);

        assert_eq!(chunks[0].symbol_name, "f1, f2, f3");
        assert_eq!(chunks[0].symbol_type, "grouped");
        assert_eq!((chunks[0].start_line, chunks[0].end_line), (1, 3));
        assert!(small.is_empty(), "the pending set is drained");
        assert_eq!(tokens, 0);
    }

    /// Long groups are summarised rather than producing an unbounded label.
    #[test]
    fn a_long_group_summarises_the_tail() {
        let (mut chunks, mut tokens) = (Vec::new(), 0);
        let mut many = pending(7);
        flush_small(&mut chunks, &mut many, &mut tokens);
        assert_eq!(chunks[0].symbol_name, "f1, f2, f3, f4 +3");
    }

    /// Behaviour lives in prose, not in identifiers: the code that answers
    /// "what happens when the lock is held" says `pid_alive`, and the sentence
    /// above it is the only place the answer is written in words.
    #[test]
    fn a_documented_symbol_gets_a_chunk_for_its_prose() {
        let src = "\
/// Waits for the process holding the lock to exit, so the next command does
/// not open a database another process still owns.
fn wait_for_exit(pid: u32) -> bool { true }
";
        let parsed = devctx_parse::parse(devctx_parse::Lang::Rust, src).unwrap();
        let chunks = chunk_file("remote.rs", src, &parsed, &ChunkConfig::default());
        let doc = chunks
            .iter()
            .find(|c| c.level == "doc")
            .expect("a documented symbol produces a doc chunk");
        assert!(doc.text.contains("another process still owns"));
        assert!(
            doc.text.contains("fn wait_for_exit"),
            "the signature ties the prose to the code"
        );
        assert_eq!(doc.symbol_name, "wait_for_exit");
    }

    /// A doc comment that only restates the name is noise: it adds a chunk that
    /// competes with the code while saying nothing the code does not.
    #[test]
    fn a_doc_that_restates_the_name_is_not_worth_a_chunk() {
        let src = "/// The name.\nfn name() {}\n";
        let parsed = devctx_parse::parse(devctx_parse::Lang::Rust, src).unwrap();
        let chunks = chunk_file("a.rs", src, &parsed, &ChunkConfig::default());
        assert!(!chunks.iter().any(|c| c.level == "doc"));
    }

    /// Prose is cut at an arbitrary place, so a sentence can begin in one block
    /// and end in the next. Overlapping the cut keeps it whole on one side.
    #[test]
    fn raw_text_blocks_overlap_their_neighbour() {
        let cfg = ChunkConfig::default();
        // Long enough to split, with no blank lines to snap to.
        let content: String = (1..=400)
            .map(|i| format!("line {i} of a document that has to be split somewhere\n"))
            .collect();
        let chunks = chunk_raw_text("guide.md", &content, &cfg);
        assert!(chunks.len() > 1, "the fixture must actually split");

        for pair in chunks.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(
                b.start_line <= a.end_line,
                "block {} starts after {} ends",
                b.start_line,
                a.end_line
            );
            assert!(b.start_line > a.start_line, "each block must advance");
        }
        let last = chunks.last().unwrap();
        assert_eq!(
            last.end_line as usize,
            content.lines().count(),
            "the tail is covered"
        );
    }

    #[test]
    fn flushing_nothing_produces_nothing() {
        let (mut chunks, mut tokens) = (Vec::new(), 0);
        flush_small(&mut chunks, &mut Vec::new(), &mut tokens);
        assert!(chunks.is_empty());
    }
}
