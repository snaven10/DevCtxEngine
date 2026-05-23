"""Summarizer providers used by the token-budget SUMMARIZE strategy.

Public API:
    SummarizerProvider (Protocol)
    create_summarizer(config, embedding_provider) -> SummarizerProvider
"""

from .base import SummarizerProvider
from .factory import create_summarizer

__all__ = ["SummarizerProvider", "create_summarizer"]
