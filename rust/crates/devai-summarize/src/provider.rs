//! The summarizer abstraction and the no-op (truncate) fallback.

use crate::error::Result;

/// A content summarizer.
pub trait Summarizer: Send + Sync {
    /// Summarize `content` to roughly `target_tokens`, optionally focused on
    /// `query`.
    fn summarize(&self, content: &str, query: Option<&str>, target_tokens: usize)
        -> Result<String>;

    /// Whether the summarizer runs entirely locally.
    fn is_local(&self) -> bool;

    /// Whether the summarizer can focus on a query.
    fn supports_query_focus(&self) -> bool;

    /// Provider name.
    fn name(&self) -> &str {
        "summarizer"
    }
}

/// A summarizer that truncates to the token budget (no model).
pub struct NoopSummarizer;

impl Summarizer for NoopSummarizer {
    fn summarize(
        &self,
        content: &str,
        _query: Option<&str>,
        target_tokens: usize,
    ) -> Result<String> {
        let max_chars = target_tokens.saturating_mul(4);
        let trimmed = content.trim();
        if trimmed.chars().count() <= max_chars {
            return Ok(trimmed.to_string());
        }
        Ok(trimmed.chars().take(max_chars).collect::<String>() + "…")
    }

    fn is_local(&self) -> bool {
        true
    }

    fn supports_query_focus(&self) -> bool {
        false
    }

    fn name(&self) -> &str {
        "noop"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_truncates_to_budget() {
        let long = "x".repeat(1000);
        let out = NoopSummarizer.summarize(&long, None, 10).unwrap(); // 40 chars
        assert_eq!(out.chars().count(), 41); // 40 + ellipsis
    }

    #[test]
    fn noop_passes_short_content() {
        assert_eq!(
            NoopSummarizer.summarize("short", None, 100).unwrap(),
            "short"
        );
    }
}
