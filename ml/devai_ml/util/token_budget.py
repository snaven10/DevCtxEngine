"""Token-budget enforcer for handler outputs.

Wraps tiktoken counting + four strategies (drop / soft_truncate / hard_truncate /
summarize) behind a single `TokenBudget.fit()` call. Handlers that return
context to the LLM call this before serializing the response.

Code outputs (search results, read_symbol) force the DROP strategy regardless
of config — truncating or summarizing code corrupts the identifiers the LLM
needs to reason about it. Prose outputs (memories) honor the configured
strategy.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from ..config import TokenBudgetConfig

if TYPE_CHECKING:
    from ..summarization.base import SummarizerProvider

logger = logging.getLogger(__name__)

# Boundary preference for soft truncation. Tries paragraph, then line, then
# sentence-ish, then word — first one that splits before the budget wins.
_SOFT_TRUNCATE_BOUNDARIES = ("\n\n", "\n", ". ", " ")


@dataclass(frozen=True)
class BudgetResult:
    """Diagnostics returned alongside fitted items."""
    strategy_used: str
    input_count: int
    output_count: int
    input_tokens: int
    output_tokens: int
    max_tokens: int
    items_dropped: int = 0
    items_truncated: int = 0
    items_summarized: int = 0
    notes: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "strategy_used": self.strategy_used,
            "input_count": self.input_count,
            "output_count": self.output_count,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "max_tokens": self.max_tokens,
            "items_dropped": self.items_dropped,
            "items_truncated": self.items_truncated,
            "items_summarized": self.items_summarized,
            "notes": list(self.notes),
        }


class TokenBudget:
    """Apply a token budget to a list of dict-shaped items.

    The summarizer is required only when strategy == "summarize" — for the
    other strategies a None summarizer is fine.
    """

    def __init__(
        self,
        config: TokenBudgetConfig,
        summarizer: "SummarizerProvider | None" = None,
    ) -> None:
        self._config = config
        self._summarizer = summarizer
        self._encoding = None  # lazy
        self._encoding_name = config.encoding

    # ---------------- token counting ----------------

    def _ensure_encoding(self):
        if self._encoding is not None:
            return self._encoding
        try:
            import tiktoken
            self._encoding = tiktoken.get_encoding(self._encoding_name)
        except Exception as exc:
            logger.warning(
                "tiktoken encoding %r unavailable (%s) — falling back to 4-char approximation",
                self._encoding_name, exc,
            )
            self._encoding = "approx"
        return self._encoding

    def count(self, text: str) -> int:
        """Return token count for `text`. Uses tiktoken when available, falls
        back to len(text)//4 otherwise."""
        if not text:
            return 0
        enc = self._ensure_encoding()
        if enc == "approx":
            return max(1, len(text) // 4)
        try:
            return len(enc.encode(text))
        except Exception:
            return max(1, len(text) // 4)

    # ---------------- main entry point ----------------

    def fit(
        self,
        items: list[dict[str, Any]],
        content_key: str = "content",
        max_tokens: int | None = None,
        is_code: bool = False,
        query: str | None = None,
        strategy: str | None = None,
    ) -> tuple[list[dict[str, Any]], BudgetResult]:
        """Apply the configured strategy to fit `items` within budget.

        Args:
            items: list of dicts. Each must have `content_key` mapping to a str.
            content_key: key holding the text (default 'content').
            max_tokens: override the config default for this call.
            is_code: True for code outputs. Forces DROP regardless of config —
                truncating/summarizing code corrupts identifiers.
            query: optional query string for query-focused summarization.
            strategy: optional override of the config strategy.

        Returns:
            (kept_items, BudgetResult). Items are NOT mutated; truncated /
            summarized items are returned as shallow copies with the new text.
        """
        budget = max_tokens if max_tokens is not None else self._config.max_output_tokens
        chosen = strategy if strategy is not None else self._config.strategy
        notes: list[str] = []

        if is_code and chosen != "drop":
            notes.append(f"is_code=True forced strategy 'drop' (was {chosen!r})")
            chosen = "drop"

        if chosen == "summarize" and self._summarizer is None:
            notes.append("strategy='summarize' but no summarizer wired; falling back to 'drop'")
            chosen = "drop"

        input_tokens = sum(self.count(str(item.get(content_key, ""))) for item in items)
        input_count = len(items)

        if chosen == "drop":
            kept, dropped = self._strategy_drop(items, content_key, budget)
            result = BudgetResult(
                strategy_used="drop",
                input_count=input_count,
                output_count=len(kept),
                input_tokens=input_tokens,
                output_tokens=sum(self.count(str(it.get(content_key, ""))) for it in kept),
                max_tokens=budget,
                items_dropped=dropped,
                notes=notes,
            )
            return kept, result

        if chosen == "soft_truncate":
            kept, truncated = self._strategy_truncate(items, content_key, budget, soft=True)
        elif chosen == "hard_truncate":
            kept, truncated = self._strategy_truncate(items, content_key, budget, soft=False)
        elif chosen == "summarize":
            kept, summarized, truncated = self._strategy_summarize(
                items, content_key, budget, query,
            )
            result = BudgetResult(
                strategy_used="summarize",
                input_count=input_count,
                output_count=len(kept),
                input_tokens=input_tokens,
                output_tokens=sum(self.count(str(it.get(content_key, ""))) for it in kept),
                max_tokens=budget,
                items_summarized=summarized,
                items_truncated=truncated,
                notes=notes,
            )
            return kept, result
        else:
            # SummarizerConfig validation should prevent this, but defensive.
            notes.append(f"unknown strategy {chosen!r}, returning items unchanged")
            return list(items), BudgetResult(
                strategy_used=chosen,
                input_count=input_count,
                output_count=input_count,
                input_tokens=input_tokens,
                output_tokens=input_tokens,
                max_tokens=budget,
                notes=notes,
            )

        return kept, BudgetResult(
            strategy_used=chosen,
            input_count=input_count,
            output_count=len(kept),
            input_tokens=input_tokens,
            output_tokens=sum(self.count(str(it.get(content_key, ""))) for it in kept),
            max_tokens=budget,
            items_truncated=truncated,
            notes=notes,
        )

    # ---------------- strategies ----------------

    def _strategy_drop(
        self,
        items: list[dict[str, Any]],
        content_key: str,
        budget: int,
    ) -> tuple[list[dict[str, Any]], int]:
        # Items keep their existing order (assumed ranked by score upstream).
        # We pack from the front until budget is full.
        kept: list[dict[str, Any]] = []
        running = 0
        for item in items:
            cost = self.count(str(item.get(content_key, "")))
            if running + cost > budget and kept:
                break
            kept.append(item)
            running += cost
        return kept, len(items) - len(kept)

    def _strategy_truncate(
        self,
        items: list[dict[str, Any]],
        content_key: str,
        budget: int,
        soft: bool,
    ) -> tuple[list[dict[str, Any]], int]:
        if not items:
            return [], 0
        # Equal share per item. Floor of 8 keeps tiny budgets exercising the
        # truncation path in tests; production budgets will be far higher.
        per_item = max(8, budget // len(items))
        out: list[dict[str, Any]] = []
        truncated = 0
        for item in items:
            text = str(item.get(content_key, ""))
            cost = self.count(text)
            if cost <= per_item:
                out.append(item)
                continue
            new_text = self._truncate_text(text, per_item, soft=soft)
            new_item = dict(item)
            new_item[content_key] = new_text
            new_item["_truncated"] = True
            out.append(new_item)
            truncated += 1
        return out, truncated

    def _strategy_summarize(
        self,
        items: list[dict[str, Any]],
        content_key: str,
        budget: int,
        query: str | None,
    ) -> tuple[list[dict[str, Any]], int, int]:
        if not items or self._summarizer is None:
            return list(items), 0, 0

        per_item = max(128, budget // max(1, len(items)))
        summarizer_target = min(per_item, max(64, self._config.max_output_tokens // 8))

        out: list[dict[str, Any]] = []
        summarized = 0
        truncated = 0
        for item in items:
            text = str(item.get(content_key, ""))
            cost = self.count(text)
            if cost <= per_item:
                out.append(item)
                continue
            try:
                summary = self._summarizer.summarize(
                    text, query=query, target_tokens=summarizer_target,
                )
                # The summarizer may overshoot — if so, hard-truncate after.
                if self.count(summary) > per_item:
                    summary = self._truncate_text(summary, per_item, soft=False)
                    truncated += 1
                new_item = dict(item)
                new_item[content_key] = summary
                new_item["_summarized"] = True
                out.append(new_item)
                summarized += 1
            except Exception as exc:
                logger.warning("Summarize failed for one item (%s) — soft-truncating instead", exc)
                new_text = self._truncate_text(text, per_item, soft=True)
                new_item = dict(item)
                new_item[content_key] = new_text
                new_item["_truncated"] = True
                out.append(new_item)
                truncated += 1
        return out, summarized, truncated

    # ---------------- truncation helpers ----------------

    def _truncate_text(self, text: str, max_tokens: int, soft: bool) -> str:
        if self.count(text) <= max_tokens:
            return text

        # Estimate the char budget: tiktoken counts ~average 4 chars/token for
        # English-ish text. We use a generous multiplier then verify with count().
        char_budget = max_tokens * 5
        candidate = text[:char_budget]

        if soft:
            for boundary in _SOFT_TRUNCATE_BOUNDARIES:
                idx = candidate.rfind(boundary)
                if idx > char_budget // 2:
                    candidate = candidate[:idx]
                    break

        candidate = self._ensure_under(candidate, max_tokens)
        return candidate.rstrip() + "\n... [truncated]"

    def _ensure_under(self, text: str, max_tokens: int) -> str:
        """Shrink text in 10% steps until under budget. Bound at a few iterations."""
        for _ in range(10):
            if self.count(text) <= max_tokens:
                return text
            text = text[: int(len(text) * 0.9)]
        return text
