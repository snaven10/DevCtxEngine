//! Extractive summarizer: rank sentences by similarity to a query (or the first
//! sentence) using the embedding model, pack to a token budget, restore order.
//! Preserves identifiers exactly (whole sentences are selected) — which is why
//! it is the default for code. See `docs/architecture-spec.md` §7.

use std::sync::Arc;

use devctx_embed::EmbeddingProvider;

use crate::error::Result;
use crate::provider::Summarizer;

/// Rough token estimate: ~4 characters per token.
pub(crate) fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / 4
}

/// Split text into sentences (on `.`/`!`/`?` boundaries and newlines).
pub(crate) fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        cur.push(c);
        let boundary = matches!(c, '.' | '!' | '?')
            && chars.peek().map(|n| n.is_whitespace()).unwrap_or(true)
            || c == '\n';
        if boundary {
            let s = cur.trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
            cur.clear();
        }
    }
    let s = cur.trim();
    if !s.is_empty() {
        out.push(s.to_string());
    }
    out
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// The extractive summarizer.
pub struct ExtractiveSummarizer {
    embedder: Arc<dyn EmbeddingProvider>,
}

impl ExtractiveSummarizer {
    /// Build over an embedding provider.
    pub fn new(embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { embedder }
    }
}

impl Summarizer for ExtractiveSummarizer {
    fn summarize(
        &self,
        content: &str,
        query: Option<&str>,
        target_tokens: usize,
    ) -> Result<String> {
        let sentences = split_sentences(content);
        if sentences.len() <= 1 || estimate_tokens(content) <= target_tokens {
            return Ok(content.trim().to_string());
        }

        // Embed the anchor (query, else the first sentence) + all sentences.
        let anchor = query
            .map(str::to_string)
            .unwrap_or_else(|| sentences[0].clone());
        let mut inputs = Vec::with_capacity(sentences.len() + 1);
        inputs.push(anchor);
        inputs.extend(sentences.iter().cloned());
        let vectors = self.embedder.embed(&inputs)?;
        let (anchor_vec, sent_vecs) = vectors.split_first().expect("non-empty");

        // Rank sentences by similarity to the anchor.
        let mut scored: Vec<(usize, f32)> = sent_vecs
            .iter()
            .enumerate()
            .map(|(i, v)| (i, cosine(anchor_vec, v)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Greedily pack to the token budget.
        let mut selected: Vec<usize> = Vec::new();
        let mut tokens = 0;
        for (i, _) in scored {
            let t = estimate_tokens(&sentences[i]);
            if !selected.is_empty() && tokens + t > target_tokens {
                continue;
            }
            selected.push(i);
            tokens += t;
            if tokens >= target_tokens {
                break;
            }
        }

        // Restore document order.
        selected.sort_unstable();
        Ok(selected
            .iter()
            .map(|&i| sentences[i].as_str())
            .collect::<Vec<_>>()
            .join(" "))
    }

    fn is_local(&self) -> bool {
        true
    }

    fn supports_query_focus(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "extractive"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_embed::Result as EmbedResult;

    #[test]
    fn splits_sentences() {
        let s = split_sentences("Hello world. How are you?\nFine!");
        assert_eq!(s, vec!["Hello world.", "How are you?", "Fine!"]);
    }

    /// Keyword-presence embedder so ranking is deterministic and offline.
    struct KwEmbedder;
    impl EmbeddingProvider for KwEmbedder {
        fn embed(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
            let kws = ["database", "auth", "cache"];
            Ok(texts
                .iter()
                .map(|t| {
                    let lc = t.to_lowercase();
                    kws.iter()
                        .map(|k| if lc.contains(k) { 1.0 } else { 0.0 })
                        .collect()
                })
                .collect())
        }
        fn dimension(&self) -> usize {
            3
        }
        fn model_name(&self) -> &str {
            "kw"
        }
    }

    #[test]
    fn extractive_selects_query_relevant_sentences() {
        let content = "The database uses connection pooling. \
             Cats are cute animals. \
             Auth relies on rotating tokens. \
             The weather is nice today.";
        let s = ExtractiveSummarizer::new(Arc::new(KwEmbedder));
        // Small budget forces selection; query focuses on "database".
        let summary = s.summarize(content, Some("database"), 10).unwrap();
        assert!(summary.contains("database"));
        assert!(!summary.contains("weather"));
    }

    #[test]
    fn short_content_is_returned_verbatim() {
        let s = ExtractiveSummarizer::new(Arc::new(KwEmbedder));
        assert_eq!(
            s.summarize("Just one line.", None, 200).unwrap(),
            "Just one line."
        );
    }
}
