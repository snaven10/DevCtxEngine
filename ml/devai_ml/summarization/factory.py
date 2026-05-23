"""Factory for SummarizerProvider implementations. Mirrors embeddings.factory."""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING

from ..config import SummarizerConfig
from .base import SummarizerProvider

if TYPE_CHECKING:
    from ..embeddings.base import EmbeddingProvider

logger = logging.getLogger(__name__)


def create_summarizer(
    config: SummarizerConfig,
    embedding_provider: "EmbeddingProvider | None" = None,
) -> SummarizerProvider:
    """Build a summarizer from config.

    Raises ValueError if config.require_local is True and the selected
    provider sends data externally. This is the privacy guard — to use
    a non-local provider you must consciously set require_local=False.
    """
    provider_type = config.provider

    if provider_type == "noop":
        from .noop import NoOpSummarizer
        provider: SummarizerProvider = NoOpSummarizer()

    elif provider_type == "extractive":
        if embedding_provider is None:
            raise ValueError(
                "extractive summarizer requires an EmbeddingProvider; "
                "pass embedding_provider= to create_summarizer()"
            )
        from .extractive import ExtractiveSummarizer
        provider = ExtractiveSummarizer(embedding=embedding_provider)

    elif provider_type == "flan-t5":
        from .flan_t5 import FlanT5Summarizer
        provider = FlanT5Summarizer(
            model_name=config.model or "google/flan-t5-small",
            device=config.device,
        )

    elif provider_type == "openai":
        from .openai_sum import OpenAISummarizer
        provider = OpenAISummarizer(
            model=config.model or "gpt-4o-mini",
            api_key=config.api_key,
        )

    else:
        # SummarizerConfig.__post_init__ already validates this, but defense
        # in depth in case someone constructs a config bypassing __init__.
        raise ValueError(f"Unknown summarizer provider: {provider_type}")

    if config.require_local and not provider.is_local():
        raise ValueError(
            f"Summarizer provider {provider.model_name()!r} sends data externally, "
            "but DEVAI_SUMMARIZER_REQUIRE_LOCAL=true (default). "
            "Set DEVAI_SUMMARIZER_REQUIRE_LOCAL=false explicitly to allow this."
        )

    logger.info("Summarizer ready: provider=%s model=%s local=%s",
                provider_type, provider.model_name(), provider.is_local())
    return provider
