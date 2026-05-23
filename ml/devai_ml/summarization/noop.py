"""No-op summarizer: returns content unchanged. Used when summarization is
disabled or as a safe fallback."""

from __future__ import annotations


class NoOpSummarizer:
    def summarize(
        self,
        content: str,
        query: str | None = None,  # noqa: ARG002 — protocol signature
        target_tokens: int = 200,  # noqa: ARG002
    ) -> str:
        return content

    def model_name(self) -> str:
        return "noop"

    def is_local(self) -> bool:
        return True

    def supports_query_focus(self) -> bool:
        return False
