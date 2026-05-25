"""No-op reranker: returns candidates[:top_k] unchanged.

Used as the default when DEVAI_RERANK_ENABLED=false, and as the silent fallback
when DEVAI_RERANK_PROVIDER=flashrank but the flashrank package is missing.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from ..stores.vector_store import SearchResult


class NoOpReranker:
    def rerank(
        self,
        query: str,  # noqa: ARG002 — protocol signature
        candidates: "list[SearchResult]",
        top_k: int,
    ) -> "list[SearchResult]":
        return candidates[:top_k]

    def model_name(self) -> str:
        return "noop"

    def is_active(self) -> bool:
        return False
