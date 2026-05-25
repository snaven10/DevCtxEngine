"""Factory for RerankerProvider implementations.

Behavior:
    enabled=False                       → NoOpReranker
    provider=noop                       → NoOpReranker
    provider=flashrank + pkg present    → FlashRankReranker
    provider=flashrank + pkg missing    → NoOpReranker + one-time WARN log
"""

from __future__ import annotations

import logging

from ..config import RerankConfig
from .base import RerankerProvider

logger = logging.getLogger(__name__)


def create_reranker(config: RerankConfig) -> RerankerProvider:
    if not config.enabled:
        from .noop import NoOpReranker
        logger.info("Reranker disabled (DEVAI_RERANK_ENABLED=false)")
        return NoOpReranker()

    if config.provider == "noop":
        from .noop import NoOpReranker
        return NoOpReranker()

    if config.provider == "flashrank":
        try:
            import flashrank  # noqa: F401 — probe only
        except ImportError:
            logger.warning(
                "DEVAI_RERANK_PROVIDER=flashrank but the `flashrank` package is "
                "not installed. Falling back to no-op reranking. "
                "Install with: pip install flashrank"
            )
            from .noop import NoOpReranker
            return NoOpReranker()

        from .flashrank import FlashRankReranker
        reranker = FlashRankReranker(
            model_name=config.model,
            cache_dir=config.cache_dir,
        )
        logger.info("Reranker ready: provider=flashrank model=%s", reranker.model_name())
        return reranker

    # RerankConfig.__post_init__ validates this, but defensive.
    raise ValueError(f"Unknown reranker provider: {config.provider}")
