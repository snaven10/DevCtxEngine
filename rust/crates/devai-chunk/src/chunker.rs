//! The semantic multi-level chunker: file → class → function → block.
//!
//! Never splits mid-symbol. Small symbols are grouped; large functions are split
//! into block chunks that re-include the signature line. Mirrors the legacy
//! `semantic_chunker.py` (rewrite plan §3).

use std::path::Path;

use devai_parse::{ParsedFile, Symbol};

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
        i = end;
    }
    out
}

struct PendingSmall {
    text: String,
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
    chunks.push(Chunk::new(
        text,
        "function",
        "",
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
