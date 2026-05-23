"""Local abstractive summarizer via HuggingFace transformers + flan-t5.

Models recommended:
    google/flan-t5-small  (~80 MB, ~1s/summary on CPU)   default
    google/flan-t5-base   (~250 MB, ~2-3s on CPU)
    google/flan-t5-large  (~1 GB, slow on CPU)

The pipeline is lazy-loaded on first call so startup stays fast. On any error
(missing model, OOM, bad output) the summarizer falls back to the truncated
original to keep the budget contract.
"""

from __future__ import annotations

import logging

logger = logging.getLogger(__name__)


class FlanT5Summarizer:
    def __init__(
        self,
        model_name: str = "google/flan-t5-small",
        device: str = "cpu",
    ) -> None:
        self._model_name = model_name
        self._device = device
        self._pipeline = None  # lazy

    def _ensure_loaded(self) -> None:
        if self._pipeline is not None:
            return
        try:
            from transformers import pipeline
        except ImportError as exc:
            raise RuntimeError(
                "FlanT5Summarizer requires the `transformers` package. "
                "Install with: pip install transformers"
            ) from exc

        logger.info("Loading flan-t5 summarizer: %s (device=%s)", self._model_name, self._device)
        self._pipeline = pipeline(
            "text2text-generation",
            model=self._model_name,
            device=-1 if self._device == "cpu" else 0,
        )

    def summarize(
        self,
        content: str,
        query: str | None = None,
        target_tokens: int = 200,
    ) -> str:
        try:
            self._ensure_loaded()
        except Exception as exc:
            logger.warning("FlanT5 load failed: %s. Returning truncated original.", exc)
            return self._truncate_chars(content, target_tokens * 4)

        if query and query.strip():
            prompt = (
                f"Summarize the following text in the context of this question: '{query}'. "
                "Keep all relevant identifiers, file names, and technical terms verbatim.\n\n"
                f"Text: {content}\n\nSummary:"
            )
        else:
            prompt = (
                "Summarize the following text concisely, preserving technical terms.\n\n"
                f"{content}\n\nSummary:"
            )

        try:
            # max_length here is in model tokens, close enough to tiktoken count
            # for sizing purposes. Buffer of 50 absorbs slight overshoot.
            result = self._pipeline(
                prompt,
                max_length=target_tokens + 50,
                min_length=max(target_tokens // 3, 30),
                do_sample=False,
            )
            return result[0]["generated_text"].strip()
        except Exception as exc:
            logger.warning("FlanT5 inference failed: %s. Returning truncated original.", exc)
            return self._truncate_chars(content, target_tokens * 4)

    def model_name(self) -> str:
        return self._model_name

    def is_local(self) -> bool:
        return True

    def supports_query_focus(self) -> bool:
        return True

    @staticmethod
    def _truncate_chars(content: str, char_budget: int) -> str:
        if len(content) <= char_budget:
            return content
        return content[:char_budget].rsplit(" ", 1)[0] + "..."
