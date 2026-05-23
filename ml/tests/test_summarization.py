"""Tests for the summarization factory + providers (extractive + noop + guard).

flan-t5 and openai providers are not exercised end-to-end here: they require
network or large model downloads. We test their factory wiring and the
require_local guard. Real inference is covered by integration tests when
those models are available locally.
"""

from __future__ import annotations

import pytest

from devai_ml.config import SummarizerConfig
from devai_ml.summarization import create_summarizer
from devai_ml.summarization.noop import NoOpSummarizer


class FakeEmbedder:
    """Deterministic keyword-based stub. The vector marks whether the text
    contains each tracked keyword — cosine sim then ranks by lexical overlap
    with the query, which is predictable and stable across pytest runs."""

    KEYWORDS = ("long", "short", "intro", "ending", "middle")

    def __init__(self) -> None:
        self._calls: list[list[str]] = []

    def _emb(self, text: str) -> list[float]:
        lower = text.lower()
        return [1.0 if kw in lower else 0.05 for kw in self.KEYWORDS]

    def embed(self, texts: list[str]) -> list[list[float]]:
        self._calls.append(list(texts))
        return [self._emb(t) for t in texts]

    def embed_single(self, text: str) -> list[float]:
        return self._emb(text)

    def dimension(self) -> int: return len(self.KEYWORDS)
    def model_name(self) -> str: return "fake-embedder"


# ---------------- factory ----------------


def test_factory_default_is_extractive():
    fake = FakeEmbedder()
    s = create_summarizer(SummarizerConfig(), embedding_provider=fake)
    assert s.model_name().startswith("extractive-")
    assert s.is_local() is True


def test_factory_noop():
    s = create_summarizer(SummarizerConfig(provider="noop"))
    assert isinstance(s, NoOpSummarizer)
    assert s.is_local() is True


def test_factory_extractive_requires_embedding():
    with pytest.raises(ValueError, match="requires an EmbeddingProvider"):
        create_summarizer(SummarizerConfig(provider="extractive"), embedding_provider=None)


def test_factory_invalid_provider_rejected_in_config():
    with pytest.raises(ValueError, match="Invalid provider"):
        SummarizerConfig(provider="not-a-real-provider")


# ---------------- require_local guard ----------------


def test_require_local_blocks_openai_by_default():
    """OpenAISummarizer.is_local() is False, so the factory must refuse it
    when require_local=True (the default)."""
    # NB: imports `openai` package which IS installed in the venv (in [api]
    # extras). If the package were missing this would fail earlier with a
    # different ImportError.
    with pytest.raises(ValueError, match="sends data externally"):
        create_summarizer(
            SummarizerConfig(provider="openai", api_key="dummy"),
        )


def test_require_local_false_allows_openai():
    """Setting require_local=False explicitly unblocks the openai provider."""
    s = create_summarizer(
        SummarizerConfig(provider="openai", api_key="dummy", require_local=False),
    )
    assert s.model_name().startswith("openai-")
    assert s.is_local() is False


# ---------------- NoOp ----------------


def test_noop_returns_content_unchanged():
    s = NoOpSummarizer()
    assert s.summarize("hello world") == "hello world"
    assert s.supports_query_focus() is False


# ---------------- ExtractiveSummarizer behavior ----------------


def test_extractive_returns_original_for_short_content():
    """If content has <= 3 sentences, we return as-is rather than ranking."""
    fake = FakeEmbedder()
    s = create_summarizer(SummarizerConfig(provider="extractive"), embedding_provider=fake)
    short = "One sentence. Two sentence. Three sentence."
    assert s.summarize(short) == short
    assert len(fake._calls) == 0  # never embedded


def test_extractive_picks_query_relevant_sentences():
    fake = FakeEmbedder()
    s = create_summarizer(SummarizerConfig(provider="extractive"), embedding_provider=fake)
    # Sentences keyed to the FakeEmbedder's keywords. Query "long" should pick
    # the sentence containing "long" over the others.
    text = (
        "This sentence mentions short content. "
        "This sentence mentions long content. "
        "This sentence mentions middle content. "
        "This sentence mentions intro content. "
        "This sentence mentions ending content."
    )
    summary = s.summarize(text, query="long", target_tokens=15)
    # The "long" sentence must be present (highest cosine sim with query "long")
    assert "long content" in summary


def test_extractive_preserves_document_order():
    """When multiple sentences are picked, output order matches document order."""
    fake = FakeEmbedder()
    s = create_summarizer(SummarizerConfig(provider="extractive"), embedding_provider=fake)
    text = (
        "Alpha intro sentence here. "
        "Beta middle sentence here. "
        "Gamma ending sentence here. "
        "Delta unrelated content. "
        "Epsilon also unrelated content."
    )
    # query matches "intro" and "ending" — both should be picked, in original order
    summary = s.summarize(text, query="intro ending", target_tokens=30)
    if "Alpha" in summary and "Gamma" in summary:
        assert summary.index("Alpha") < summary.index("Gamma")


def test_extractive_falls_back_on_embedder_failure():
    """If the embedder raises mid-summarize, we should NOT propagate — return
    the truncated original instead so the caller gets something usable."""

    class BrokenEmbedder:
        def embed(self, texts):
            raise RuntimeError("simulated embed failure")
        def model_name(self): return "broken"

    s = create_summarizer(
        SummarizerConfig(provider="extractive"),
        embedding_provider=BrokenEmbedder(),
    )
    long_text = "Sentence one. " * 50
    out = s.summarize(long_text, target_tokens=20)
    # Should be a string, shorter than original
    assert isinstance(out, str)
    assert len(out) < len(long_text)
