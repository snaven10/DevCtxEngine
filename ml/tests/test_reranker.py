"""Tests for the reranking factory + providers.

FlashRank end-to-end is not exercised here (would need to download an ~80MB
model and is excluded from CI by design). The factory's graceful-fallback
behavior IS tested with the import-missing path.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass
from typing import Any

import pytest

from devai_ml.config import RerankConfig
from devai_ml.retrieval import create_reranker
from devai_ml.retrieval.noop import NoOpReranker


# Use a local SearchResult-shaped dataclass to avoid pulling in the vector_store
# module (which would trigger lancedb imports). The Protocol only requires
# `text`, `score`, `metadata`, `id`.
@dataclass
class FakeSearchResult:
    id: str
    score: float
    metadata: dict[str, Any]
    text: str


def _candidates(*items: tuple[str, str]) -> list[FakeSearchResult]:
    """Build a candidate list from (symbol, text) pairs."""
    return [
        FakeSearchResult(
            id=str(i),
            score=1.0 - i * 0.1,  # decreasing pseudo-distance
            metadata={"symbol": sym, "file": f"f{i}.py"},
            text=txt,
        )
        for i, (sym, txt) in enumerate(items)
    ]


# ---------------- factory: enabled flag ----------------


def test_factory_disabled_returns_noop():
    r = create_reranker(RerankConfig(enabled=False))
    assert isinstance(r, NoOpReranker)
    assert r.is_active() is False


def test_factory_noop_provider():
    r = create_reranker(RerankConfig(provider="noop"))
    assert isinstance(r, NoOpReranker)


def test_factory_invalid_provider_rejected_at_config():
    with pytest.raises(ValueError, match="Invalid provider"):
        RerankConfig(provider="cohere")  # not yet supported


def test_factory_invalid_top_k_rejected():
    with pytest.raises(ValueError, match="top_k_fetch"):
        RerankConfig(top_k_fetch=0)


# ---------------- factory: flashrank fallback ----------------


def test_factory_falls_back_to_noop_when_flashrank_missing(monkeypatch):
    """When flashrank isn't installed, factory should return NoOp + log warning."""
    # Force the import to fail even if flashrank IS installed in this venv.
    monkeypatch.setitem(sys.modules, "flashrank", None)
    r = create_reranker(RerankConfig(enabled=True, provider="flashrank"))
    assert isinstance(r, NoOpReranker)


# ---------------- NoOpReranker behavior ----------------


def test_noop_returns_top_k_in_order():
    r = NoOpReranker()
    cands = _candidates(("foo", "alpha"), ("bar", "beta"), ("baz", "gamma"))
    out = r.rerank("anything", cands, top_k=2)
    assert len(out) == 2
    assert [c.id for c in out] == ["0", "1"]


def test_noop_respects_top_k_larger_than_candidates():
    r = NoOpReranker()
    cands = _candidates(("foo", "alpha"))
    assert len(r.rerank("q", cands, top_k=10)) == 1


def test_noop_handles_empty_candidates():
    r = NoOpReranker()
    assert r.rerank("q", [], top_k=5) == []


# ---------------- FlashRank fake-driven behavior ----------------


class FakeFlashRanker:
    """Simulates flashrank.Ranker — orders by query token count in passage text.
    Closer match = higher score. Predictable for tests."""

    def __init__(self, model_name: str, cache_dir: str | None = None) -> None:
        self.model_name = model_name

    def rerank(self, request):
        query_terms = set(request.query.lower().split())
        out = []
        for p in request.passages:
            text = str(p.get("text", "")).lower()
            score = sum(1 for tok in query_terms if tok in text)
            out.append({"id": p["id"], "score": float(score)})
        return sorted(out, key=lambda r: -r["score"])


def test_flashrank_reorders_by_query_overlap(monkeypatch):
    """Inject a FakeFlashRanker module and verify FlashRankReranker uses it
    to reorder candidates by query relevance."""
    fake_module = type(sys)("flashrank")
    fake_module.Ranker = FakeFlashRanker  # type: ignore[attr-defined]

    class FakeRerankRequest:
        def __init__(self, query: str, passages: list[dict]) -> None:
            self.query = query
            self.passages = passages

    fake_module.RerankRequest = FakeRerankRequest  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "flashrank", fake_module)

    from devai_ml.retrieval.flashrank import FlashRankReranker
    r = FlashRankReranker(model_name="fake")

    cands = _candidates(
        ("foo", "this is about apples and oranges"),
        ("bar", "delete user account from database"),
        ("baz", "create a new user with default profile"),
    )
    out = r.rerank("create user", cands, top_k=2)

    assert len(out) == 2
    # The "create a new user" passage should rank first (matches both query tokens).
    assert out[0].id == "2"
    # The "delete user" passage should rank second (matches 1 token).
    assert out[1].id == "1"
    assert r.is_active() is True


def test_flashrank_propagates_rerank_score(monkeypatch):
    """Reranked SearchResults should have their `score` field updated to the
    rerank score, not the original LanceDB distance."""
    fake_module = type(sys)("flashrank")
    fake_module.Ranker = FakeFlashRanker  # type: ignore[attr-defined]

    class FakeRerankRequest:
        def __init__(self, query: str, passages: list[dict]) -> None:
            self.query = query
            self.passages = passages

    fake_module.RerankRequest = FakeRerankRequest  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "flashrank", fake_module)

    from devai_ml.retrieval.flashrank import FlashRankReranker
    r = FlashRankReranker(model_name="fake")

    # Explicit original score that can't collide with the fake reranker's score.
    cands = [FakeSearchResult(id="0", score=0.42, metadata={}, text="user account")]
    out = r.rerank("user", cands, top_k=1)
    assert out[0].score != 0.42, "rerank score should replace original"
    assert out[0].score == 1.0, "FakeFlashRanker returns 1.0 for one query-term match"


def test_flashrank_empty_candidates_returns_empty(monkeypatch):
    fake_module = type(sys)("flashrank")
    fake_module.Ranker = FakeFlashRanker  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "flashrank", fake_module)
    from devai_ml.retrieval.flashrank import FlashRankReranker
    r = FlashRankReranker(model_name="fake")
    assert r.rerank("q", [], top_k=5) == []


def test_flashrank_falls_back_on_ranker_exception(monkeypatch):
    """If the underlying ranker raises mid-call, we should NOT propagate —
    return the original candidates so the search pipeline keeps working."""

    class ExplodingRanker:
        def __init__(self, *a, **kw) -> None: pass
        def rerank(self, _req):
            raise RuntimeError("boom")

    fake_module = type(sys)("flashrank")
    fake_module.Ranker = ExplodingRanker  # type: ignore[attr-defined]
    fake_module.RerankRequest = lambda **kw: type("Req", (), kw)()  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "flashrank", fake_module)

    from devai_ml.retrieval.flashrank import FlashRankReranker
    r = FlashRankReranker(model_name="fake")
    cands = _candidates(("a", "x"), ("b", "y"))
    out = r.rerank("q", cands, top_k=2)
    assert len(out) == 2
    assert [c.id for c in out] == ["0", "1"]  # original order
