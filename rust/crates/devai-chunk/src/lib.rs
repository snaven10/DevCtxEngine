//! `devai-chunk` — semantic multi-level code chunker for DevAI.
//!
//! Turns a parsed file (`devai-parse`) plus its source into embeddable chunks at
//! file / class / function / block level, never splitting mid-symbol. See
//! `docs/rust-rewrite-plan.md` §3.

mod chunk;
mod chunker;
mod memory;

pub use chunk::{content_hash, estimate_tokens, Chunk, ChunkConfig};
pub use chunker::{chunk_file, chunk_raw_text};
pub use memory::{memory_chunks, MemoryChunk, MemoryChunkConfig};

#[cfg(test)]
mod tests {
    use super::*;
    use devai_parse::{parse, Lang};

    fn chunks_for(lang: Lang, path: &str, src: &str) -> Vec<Chunk> {
        let parsed = parse(lang, src).unwrap();
        chunk_file(path, src, &parsed, &ChunkConfig::default())
    }

    fn levels(chunks: &[Chunk]) -> Vec<&str> {
        chunks.iter().map(|c| c.level.as_str()).collect()
    }

    #[test]
    fn emits_file_chunk_with_imports_and_symbols() {
        let src = "\
import os

def alpha():
    return 1
";
        let chunks = chunks_for(Lang::Python, "pkg/mod.py", src);
        let file = &chunks[0];
        assert_eq!(file.level, "file");
        assert_eq!(file.symbol_name, "mod.py");
        assert!(file.text.contains("# Imports:"));
        assert!(file.text.contains("import os"));
        assert!(file.text.contains("alpha"));
        assert_eq!(file.content_hash.len(), 16);
    }

    #[test]
    fn class_chunk_lists_methods() {
        let src = "\
class Greeter:
    def hello(self):
        return 1
    def bye(self):
        return 2
";
        let chunks = chunks_for(Lang::Python, "g.py", src);
        let class = chunks.iter().find(|c| c.level == "class").unwrap();
        assert_eq!(class.symbol_name, "Greeter");
        assert!(class.text.contains("# methods:"));
        assert!(class.text.contains("hello"));
        assert!(class.text.contains("bye"));
    }

    #[test]
    fn normal_function_chunk_has_context_header() {
        // A mid-sized function (> min, < large): ~100 tokens.
        let body: String = (0..30)
            .map(|i| format!("    let v{i} = {i} + 1;\n"))
            .collect();
        let src = format!("fn medium() {{\n{body}}}\n");
        let chunks = chunks_for(Lang::Rust, "src/lib.rs", &src);
        let f = chunks.iter().find(|c| c.level == "function").unwrap();
        assert_eq!(f.symbol_name, "medium");
        assert!(f.context_header.contains("lib.rs"));
        assert!(f.context_header.contains("medium"));
        assert!(f.text.starts_with("# lib.rs > medium"));
    }

    #[test]
    fn tiny_functions_are_grouped() {
        let src = "\
fn a() {}
fn b() {}
fn c() {}
";
        let chunks = chunks_for(Lang::Rust, "t.rs", src);
        let grouped: Vec<_> = chunks
            .iter()
            .filter(|c| c.symbol_type == "grouped")
            .collect();
        assert_eq!(grouped.len(), 1, "levels: {:?}", levels(&chunks));
        assert_eq!(grouped[0].level, "function");
        // No individual function chunks for the tiny fns.
        assert!(chunks.iter().all(|c| c.symbol_name != "a"));
    }

    #[test]
    fn raw_text_small_is_one_file_chunk() {
        let chunks = chunk_raw_text(
            "docs/readme.md",
            "# Title\n\nSome notes here.\n",
            &ChunkConfig::default(),
        );
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].level, "file");
        assert_eq!(chunks[0].symbol_name, "readme.md");
        assert!(chunks[0].text.contains("Some notes"));
    }

    #[test]
    fn raw_text_large_splits_into_blocks() {
        let content: String = (0..600).map(|i| format!("line number {i}\n")).collect();
        let chunks = chunk_raw_text("data.txt", &content, &ChunkConfig::default());
        assert!(chunks.len() >= 2, "got {}", chunks.len());
        assert!(chunks.iter().all(|c| c.level == "block"));
        // Increasing, non-overlapping ranges.
        for w in chunks.windows(2) {
            assert!(w[1].start_line > w[0].start_line);
        }
    }

    #[test]
    fn raw_text_empty_yields_nothing() {
        assert!(chunk_raw_text("x.md", "   \n\n", &ChunkConfig::default()).is_empty());
    }

    #[test]
    fn large_function_splits_into_blocks_with_signature() {
        // ~2000 tokens => multiple block chunks.
        let body: String = (0..400)
            .map(|i| format!("    let v{i} = {i} + 1;\n"))
            .collect();
        let src = format!("fn big() {{\n{body}}}\n");
        let chunks = chunks_for(Lang::Rust, "big.rs", &src);
        let blocks: Vec<_> = chunks.iter().filter(|c| c.level == "block").collect();
        assert!(
            blocks.len() >= 2,
            "expected >=2 blocks, got {}",
            blocks.len()
        );
        // Continuation blocks re-include the signature line.
        assert!(blocks[1].text.contains("fn big"));
        // No mid-symbol overlap: block start lines are strictly increasing.
        for w in blocks.windows(2) {
            assert!(w[1].start_line > w[0].start_line);
        }
    }
}
