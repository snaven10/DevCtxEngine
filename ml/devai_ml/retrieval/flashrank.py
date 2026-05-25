"""FlashRank cross-encoder reranker.

Default model `ms-marco-MiniLM-L-12-v2` (~80MB, cached on disk after first use)
lifts precision@N over LanceDB's raw cosine distance, especially when the
embedding model is small (MiniLM-L6) and a stronger semantic discriminator
helps reorder near-ties.

Cost: ~30-50ms per query for 15 candidates on CPU after the model is loaded.
First call pays the download (~80MB) and the load (~1-2s).

The package is optional. If `flashrank` is missing, the factory falls back to
NoOpReranker rather than failing — the search pipeline keeps working.
"""

from __future__ import annotations

import logging
from dataclasses import replace
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from ..stores.vector_store import SearchResult

logger = logging.getLogger(__name__)


class FlashRankReranker:
    def __init__(
        self,
        model_name: str = "ms-marco-MiniLM-L-12-v2",
        cache_dir: str | None = None,
    ) -> None:
        self._model_name = model_name
        self._cache_dir = cache_dir
        self._ranker = None  # lazy

    def _ensure_loaded(self) -> None:
        if self._ranker is not None:
            return
        try:
            from flashrank import Ranker
        except ImportError as exc:
            raise RuntimeError(
                "FlashRankReranker requires the `flashrank` package. "
                "Install with: pip install flashrank"
            ) from exc
        logger.info("Loading FlashRank model: %s", self._model_name)
        kwargs: dict = {"model_name": self._model_name}
        if self._cache_dir:
            kwargs["cache_dir"] = self._cache_dir
        self._ranker = Ranker(**kwargs)

    def rerank(
        self,
        query: str,
        candidates: "list[SearchResult]",
        top_k: int,
    ) -> "list[SearchResult]":
        if not candidates:
            return []

        try:
            self._ensure_loaded()
        except Exception as exc:
            logger.warning(
                "FlashRank load failed (%s) — returning candidates unchanged",
                exc,
            )
            return candidates[:top_k]

        # Build passages from candidate text. Empty-text candidates get a
        # placeholder so flashrank doesn't choke.
        passages = [
            {"id": str(i), "text": (c.text or c.metadata.get("symbol", "")) or "empty"}
            for i, c in enumerate(candidates)
        ]

        try:
            from flashrank import RerankRequest
            ranked = self._ranker.rerank(RerankRequest(query=query, passages=passages))
        except Exception as exc:
            logger.warning(
                "FlashRank rerank failed (%s) — returning candidates unchanged",
                exc,
            )
            return candidates[:top_k]

        # ranked is a list of dicts (or similar) sorted by score desc with
        # 'id' (the str we passed) and 'score' (float). Some versions expose
        # 'id' as int — handle both.
        id_to_score: dict[str, float] = {}
        for r in ranked:
            rid = str(r.get("id"))
            score = r.get("score")
            if rid and score is not None:
                try:
                    id_to_score[rid] = float(score)
                except (TypeError, ValueError):
                    continue

        if not id_to_score:
            logger.warning("FlashRank returned unparseable output — keeping original order")
            return candidates[:top_k]

        # Reorder candidates by rerank score. Items missing from id_to_score
        # (shouldn't happen) get -inf so they fall to the end.
        order = sorted(
            range(len(candidates)),
            key=lambda i: -id_to_score.get(str(i), float("-inf")),
        )

        reordered: list[SearchResult] = []
        for i in order[:top_k]:
            cand = candidates[i]
            new_score = id_to_score.get(str(i), cand.score)
            # SearchResult is @dataclass (not frozen) but use replace() to keep
            # the original instance immutable and avoid surprising callers.
            reordered.append(replace(cand, score=new_score))
        return reordered

    def model_name(self) -> str:
        return f"flashrank-{self._model_name}"

    def is_active(self) -> bool:
        return True
