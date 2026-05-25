"""Tests for util.token_budget.TokenBudget — the 4 strategies + diagnostics."""

from __future__ import annotations

from typing import Any

from devai_ml.config import TokenBudgetConfig
from devai_ml.util.token_budget import BudgetResult, TokenBudget


class FakeSummarizer:
    """Deterministic summarizer for tests. Just returns 'SUMMARY: <first 30 chars>'."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str | None, int]] = []

    def summarize(self, content: str, query: str | None = None, target_tokens: int = 200) -> str:
        self.calls.append((content, query, target_tokens))
        return f"SUMMARY: {content[:30]}"

    def model_name(self) -> str: return "fake"
    def is_local(self) -> bool: return True
    def supports_query_focus(self) -> bool: return True


class FailingSummarizer:
    """Raises on every call. Tests the fallback path."""
    def summarize(self, content: str, query: str | None = None, target_tokens: int = 200) -> str:
        raise RuntimeError("simulated summarizer failure")
    def model_name(self) -> str: return "failing"
    def is_local(self) -> bool: return True
    def supports_query_focus(self) -> bool: return False


def _items(*texts: str, key: str = "content") -> list[dict[str, Any]]:
    return [{key: t} for t in texts]


# ---------------- counting ----------------


def test_count_empty_returns_zero():
    tb = TokenBudget(TokenBudgetConfig())
    assert tb.count("") == 0


def test_count_short_text():
    tb = TokenBudget(TokenBudgetConfig())
    # "hello world" → tiktoken cl100k = 2 tokens
    assert tb.count("hello world") in (2, 3)  # ~2 with tiktoken, ~3 with approx


# ---------------- DROP ----------------


def test_drop_keeps_first_items_within_budget():
    tb = TokenBudget(TokenBudgetConfig(max_output_tokens=20))
    items = _items("aaaa " * 5, "bbbb " * 5, "cccc " * 5)
    kept, result = tb.fit(items, max_tokens=10)
    assert result.strategy_used == "drop"
    assert len(kept) <= 2
    assert result.items_dropped == len(items) - len(kept)


def test_drop_always_keeps_at_least_one():
    """Even a single oversized item should be kept rather than returning empty —
    callers prefer one partial answer over none."""
    tb = TokenBudget(TokenBudgetConfig(max_output_tokens=10))
    items = _items("x" * 5000)
    kept, _ = tb.fit(items, max_tokens=5)
    assert len(kept) == 1


def test_drop_diagnostics_token_counts():
    tb = TokenBudget(TokenBudgetConfig())
    items = _items("hello world", "foo bar baz")
    kept, result = tb.fit(items, max_tokens=1000)  # budget plenty
    assert result.input_count == 2
    assert result.output_count == 2
    assert result.input_tokens > 0
    assert result.output_tokens == result.input_tokens


# ---------------- SOFT_TRUNCATE ----------------


def test_soft_truncate_cuts_at_boundary():
    tb = TokenBudget(TokenBudgetConfig(max_output_tokens=20, strategy="soft_truncate"))
    # Build a long text with clear paragraph breaks
    text = "First sentence.\n\nSecond sentence with more words.\n\nThird block."
    kept, result = tb.fit([{"content": text}], max_tokens=8)
    assert result.strategy_used == "soft_truncate"
    new = kept[0]["content"]
    assert new != text
    assert "[truncated]" in new


def test_truncate_marks_item():
    tb = TokenBudget(TokenBudgetConfig(strategy="soft_truncate"))
    items = _items("word " * 500)
    kept, _ = tb.fit(items, max_tokens=20)
    assert kept[0].get("_truncated") is True


def test_truncate_leaves_small_items_untouched():
    tb = TokenBudget(TokenBudgetConfig(strategy="soft_truncate", max_output_tokens=200))
    items = _items("tiny", "also small")
    kept, result = tb.fit(items)
    assert result.items_truncated == 0
    assert kept[0]["content"] == "tiny"


# ---------------- HARD_TRUNCATE ----------------


def test_hard_truncate_does_not_split_at_boundary():
    tb = TokenBudget(TokenBudgetConfig(strategy="hard_truncate"))
    text = "x" * 10000  # no boundaries at all
    kept, result = tb.fit([{"content": text}], max_tokens=20)
    assert result.strategy_used == "hard_truncate"
    assert len(kept[0]["content"]) < len(text)
    assert "[truncated]" in kept[0]["content"]


# ---------------- SUMMARIZE ----------------


def test_summarize_calls_summarizer_for_oversize_items():
    fake = FakeSummarizer()
    tb = TokenBudget(TokenBudgetConfig(strategy="summarize"), summarizer=fake)
    items = _items("x" * 5000)
    kept, result = tb.fit(items, max_tokens=50)
    assert result.strategy_used == "summarize"
    assert len(fake.calls) == 1
    assert kept[0]["_summarized"] is True
    assert kept[0]["content"].startswith("SUMMARY:")


def test_summarize_passes_query_through():
    fake = FakeSummarizer()
    tb = TokenBudget(TokenBudgetConfig(strategy="summarize"), summarizer=fake)
    items = _items("x" * 5000)
    tb.fit(items, max_tokens=50, query="my question")
    assert fake.calls[0][1] == "my question"


def test_summarize_falls_back_to_truncate_on_summarizer_error():
    failing = FailingSummarizer()
    tb = TokenBudget(TokenBudgetConfig(strategy="summarize"), summarizer=failing)
    items = _items("x" * 5000)
    kept, result = tb.fit(items, max_tokens=50)
    # Item should still be returned, just truncated instead of summarized
    assert len(kept) == 1
    assert kept[0].get("_truncated") is True or kept[0].get("_summarized") is False
    assert result.items_truncated >= 1


def test_summarize_without_summarizer_falls_back_to_drop():
    tb = TokenBudget(TokenBudgetConfig(strategy="summarize"), summarizer=None)
    items = _items("x" * 5000, "y" * 5000)
    kept, result = tb.fit(items, max_tokens=50)
    assert result.strategy_used == "drop"
    assert "falling back" in " ".join(result.notes)


# ---------------- is_code forces DROP ----------------


def test_is_code_forces_drop_regardless_of_config():
    fake = FakeSummarizer()
    tb = TokenBudget(TokenBudgetConfig(strategy="summarize"), summarizer=fake)
    items = _items("def foo(): pass\n" * 1000)
    _kept, result = tb.fit(items, max_tokens=20, is_code=True)
    assert result.strategy_used == "drop"
    assert len(fake.calls) == 0
    assert any("is_code=True forced" in n for n in result.notes)


# ---------------- override per call ----------------


def test_per_call_strategy_override():
    tb = TokenBudget(TokenBudgetConfig(strategy="drop"))
    items = _items("x" * 5000)
    _kept, result = tb.fit(items, max_tokens=50, strategy="hard_truncate")
    assert result.strategy_used == "hard_truncate"


def test_max_tokens_override():
    tb = TokenBudget(TokenBudgetConfig(max_output_tokens=4000))
    items = _items("a", "b", "c")
    _kept, result = tb.fit(items, max_tokens=100)
    assert result.max_tokens == 100


# ---------------- BudgetResult serialization ----------------


def test_budget_result_to_dict_round_trip():
    r = BudgetResult(
        strategy_used="drop",
        input_count=10, output_count=5,
        input_tokens=1000, output_tokens=500,
        max_tokens=500,
        items_dropped=5,
    )
    d = r.to_dict()
    assert d["strategy_used"] == "drop"
    assert d["items_dropped"] == 5
    assert d["max_tokens"] == 500


# ---------------- non-string content keys / edge cases ----------------


def test_handles_missing_content_key():
    tb = TokenBudget(TokenBudgetConfig())
    items: list[dict[str, Any]] = [{"id": 1}, {"id": 2, "content": "real"}]
    kept, result = tb.fit(items, max_tokens=1000)
    assert len(kept) == 2  # missing content = 0 tokens, both fit
    assert result.input_tokens > 0


def test_handles_empty_list():
    tb = TokenBudget(TokenBudgetConfig())
    kept, result = tb.fit([])
    assert kept == []
    assert result.input_count == 0
    assert result.output_count == 0
