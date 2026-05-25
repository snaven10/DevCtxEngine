"""RerankerProvider Protocol. Mirrors the EmbeddingProvider / SummarizerProvider
pattern so callers can swap implementations via the factory."""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    from ..stores.vector_store import SearchResult


@runtime_checkable
class RerankerProvider(Protocol):
    """Reorder vector-search candidates by relevance to the query.

    Implementations must never raise on normal failure — degrade gracefully
    (return candidates[:top_k] unchanged) so the search pipeline remains usable.
    """

    def rerank(
        self,
        query: str,
        candidates: "list[SearchResult]",
        top_k: int,
    ) -> "list[SearchResult]":
        """Return up to `top_k` candidates reordered by rerank score."""
        ...

    def model_name(self) -> str:
        """Identifier of the active model (e.g. 'flashrank-ms-marco-MiniLM-L-12-v2')."""
        ...

    def is_active(self) -> bool:
        """True if the reranker is actually doing work. NoOp returns False
        so callers can decide whether to emit a 'rerank_score' field."""
        ...
