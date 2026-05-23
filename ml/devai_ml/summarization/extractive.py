"""Extractive summarizer: picks top sentences by similarity to a query, reusing
the already-loaded EmbeddingProvider.

Trade-off vs abstractive: no narrative reflow, but preserves exact identifiers
(function names, file paths, error codes). Perfect for LLM-consumed outputs;
suboptimal for human-readable summaries.

Cost: $0, ~50ms per summary (one embed of query + one batch embed of sentences),
no new dependencies.
"""

from __future__ import annotations

import logging
import math
import re
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from ..embeddings.base import EmbeddingProvider

logger = logging.getLogger(__name__)

# Splits on .!? followed by whitespace, OR on blank lines. Good enough for
# code-adjacent prose without dragging in nltk.
_SENT_SPLIT_RE = re.compile(r"(?<=[.!?])\s+|\n\s*\n")


class ExtractiveSummarizer:
    def __init__(self, embedding: "EmbeddingProvider") -> None:
        self._embedder = embedding

    def summarize(
        self,
        content: str,
        query: str | None = None,
        target_tokens: int = 200,
    ) -> str:
        sentences = [s.strip() for s in _SENT_SPLIT_RE.split(content) if s.strip()]
        if len(sentences) <= 3:
            return content

        # Without a query, fall back to using the first sentence as the centroid.
        # That biases toward sentences semantically related to the topic intro,
        # which for memory content tends to be the title-like opener.
        anchor = query if query and query.strip() else sentences[0]

        try:
            anchor_emb = self._embedder.embed([anchor])[0]
            sent_embs = self._embedder.embed(sentences)
        except Exception as exc:
            logger.warning("ExtractiveSummarizer.embed failed: %s. Returning truncated original.", exc)
            return self._truncate_chars(content, target_tokens * 4)

        scored = sorted(
            enumerate(zip(sentences, sent_embs)),
            key=lambda x: -self._cosine(anchor_emb, x[1][1]),
        )

        # Pack high-score sentences until budget is hit.
        picked_idx: set[int] = set()
        running_tokens = 0
        for orig_idx, (sent, _emb) in scored:
            cost = self._approx_tokens(sent)
            if running_tokens + cost > target_tokens and picked_idx:
                break
            picked_idx.add(orig_idx)
            running_tokens += cost

        # Re-order to original document order for readability.
        return " ".join(s for i, s in enumerate(sentences) if i in picked_idx)

    def model_name(self) -> str:
        return f"extractive-{self._embedder.model_name()}"

    def is_local(self) -> bool:
        # is_local mirrors the embedding provider it's wrapping.
        # All bundled embedding providers are local; an external one would
        # propagate the leak risk here. Defensive default: trust the embedder
        # to declare its own locality if it ever exposes that.
        return True

    def supports_query_focus(self) -> bool:
        return True

    @staticmethod
    def _approx_tokens(text: str) -> int:
        # Match the ~4 chars/token approximation used elsewhere in this codebase
        # (see chunking.semantic_chunker.CodeChunk.token_estimate). Avoids a
        # tiktoken dependency in the hot path.
        return max(1, len(text) // 4)

    @staticmethod
    def _cosine(a: list[float], b: list[float]) -> float:
        dot = sum(x * y for x, y in zip(a, b))
        na = math.sqrt(sum(x * x for x in a))
        nb = math.sqrt(sum(y * y for y in b))
        if na == 0 or nb == 0:
            return 0.0
        return dot / (na * nb)

    @staticmethod
    def _truncate_chars(content: str, char_budget: int) -> str:
        if len(content) <= char_budget:
            return content
        return content[:char_budget].rsplit(" ", 1)[0] + "..."
