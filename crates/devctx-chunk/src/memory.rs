//! Token-window chunker for long memories.
//!
//! A memory is embedded as an intro vector (`memory`, title + whole content) plus
//! sliding body-window vectors (`memory_chunk`, title prepended). The first window
//! is skipped because the intro already covers the start. See
//! `docs/architecture-spec.md` §3; recall blends intro + best chunk.

/// A single memory chunk to embed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryChunk {
    /// Text to embed.
    pub text: String,
    /// `memory` (intro) or `memory_chunk` (body window).
    pub level: String,
}

/// Windowing configuration (word-approximated tokens).
#[derive(Debug, Clone, Copy)]
pub struct MemoryChunkConfig {
    /// Window size in words.
    pub window_tokens: usize,
    /// Overlap between consecutive windows, in words.
    pub overlap: usize,
    /// Maximum number of body-window chunks.
    pub max_chunks: usize,
}

impl Default for MemoryChunkConfig {
    fn default() -> Self {
        Self {
            window_tokens: 100,
            overlap: 30,
            max_chunks: 40,
        }
    }
}

/// Chunk a memory into an intro chunk plus body-window chunks.
pub fn memory_chunks(title: &str, content: &str, cfg: &MemoryChunkConfig) -> Vec<MemoryChunk> {
    let mut out = Vec::new();

    // Intro: title + whole content (the embedder caps overly long input).
    out.push(MemoryChunk {
        text: with_title(title, content),
        level: "memory".to_string(),
    });

    let words: Vec<&str> = content.split_whitespace().collect();
    let window = cfg.window_tokens.max(1);
    let step = window.saturating_sub(cfg.overlap).max(1);

    // Skip the first window (i == 0): the intro already covers it.
    let mut i = step;
    while i < words.len() && out.len() <= cfg.max_chunks {
        let end = (i + window).min(words.len());
        let window_text = words[i..end].join(" ");
        out.push(MemoryChunk {
            text: with_title(title, &window_text),
            level: "memory_chunk".to_string(),
        });
        i += step;
    }

    out
}

fn with_title(title: &str, body: &str) -> String {
    if title.is_empty() {
        body.to_string()
    } else {
        format!("{title}\n{body}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(window: usize, overlap: usize, max: usize) -> MemoryChunkConfig {
        MemoryChunkConfig {
            window_tokens: window,
            overlap,
            max_chunks: max,
        }
    }

    #[test]
    fn short_content_yields_only_intro() {
        let chunks = memory_chunks("Title", "a b c", &cfg(5, 2, 40));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].level, "memory");
        assert_eq!(chunks[0].text, "Title\na b c");
    }

    #[test]
    fn windows_skip_first_and_prepend_title() {
        // 12 words, window 5, overlap 2 => step 3. Windows at i=3,6,9 (i=0 skipped).
        let content = "w0 w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11";
        let chunks = memory_chunks("T", content, &cfg(5, 2, 40));
        // intro + 3 body windows
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].level, "memory");
        assert!(chunks[1..].iter().all(|c| c.level == "memory_chunk"));
        assert!(chunks[1..].iter().all(|c| c.text.starts_with("T\n")));
        assert_eq!(chunks[1].text, "T\nw3 w4 w5 w6 w7");
        assert_eq!(chunks[3].text, "T\nw9 w10 w11");
    }

    #[test]
    fn respects_max_chunks() {
        let content = (0..200)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = memory_chunks("T", &content, &cfg(5, 2, 3));
        // intro + at most max_chunks body windows
        assert!(chunks.len() <= 4, "got {}", chunks.len());
        assert_eq!(chunks[0].level, "memory");
    }

    #[test]
    fn empty_title_omits_prefix() {
        let chunks = memory_chunks("", "a b c", &cfg(5, 2, 40));
        assert_eq!(chunks[0].text, "a b c");
    }
}
