"""Summarizer Protocol mirrors the EmbeddingProvider pattern."""

from __future__ import annotations

from typing import Protocol, runtime_checkable


@runtime_checkable
class SummarizerProvider(Protocol):
    """Protocol for summarizer providers. All providers must implement this."""

    def summarize(
        self,
        content: str,
        query: str | None = None,
        target_tokens: int = 200,
    ) -> str:
        """Produce a summary of `content`.

        On any internal failure, returns the original content truncated to fit
        target_tokens rather than raising — summarization is best-effort.
        """
        ...

    def model_name(self) -> str:
        """Return the provider/model identifier (e.g. 'flan-t5-small')."""
        ...

    def is_local(self) -> bool:
        """True if no data leaves the machine. Used by the privacy guard."""
        ...

    def supports_query_focus(self) -> bool:
        """True if the `query` parameter actually influences output."""
        ...
