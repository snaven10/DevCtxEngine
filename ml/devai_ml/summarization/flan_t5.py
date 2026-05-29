"""Local abstractive summarizer via HuggingFace transformers + flan-t5.

Models recommended:
    google/flan-t5-small  (~80 MB, ~1s/summary on CPU)   default
    google/flan-t5-base   (~250 MB, ~2-3s on CPU)
    google/flan-t5-large  (~1 GB, slow on CPU)

The tokenizer + seq2seq model are lazy-loaded on first call so startup stays
fast. On any error (missing model, OOM, bad output) the summarizer falls back
to the truncated original to keep the budget contract.

NOTE: uses AutoModelForSeq2SeqLM + AutoTokenizer directly rather than the
`pipeline("text2text-generation", ...)` helper — transformers 5.x removed that
pipeline task. T5 has a 512-token input limit; the tokenizer truncates oversize
inputs (truncation=True) so big memories are summarized from their first ~512
tokens rather than erroring.
"""

from __future__ import annotations

import logging

logger = logging.getLogger(__name__)

# T5 encoder hard limit. Inputs longer than this are truncated by the tokenizer.
_MAX_INPUT_TOKENS = 512


class FlanT5Summarizer:
    def __init__(
        self,
        model_name: str = "google/flan-t5-small",
        device: str = "cpu",
    ) -> None:
        self._model_name = model_name
        self._device = device
        self._tokenizer = None  # lazy
        self._model = None  # lazy

    def _ensure_loaded(self) -> None:
        if self._model is not None:
            return
        try:
            from transformers import AutoModelForSeq2SeqLM, AutoTokenizer
        except ImportError as exc:
            raise RuntimeError(
                "FlanT5Summarizer requires the `transformers` package. "
                "Install with: pip install transformers"
            ) from exc

        logger.info("Loading flan-t5 summarizer: %s (device=%s)", self._model_name, self._device)
        tokenizer = AutoTokenizer.from_pretrained(self._model_name)
        model = AutoModelForSeq2SeqLM.from_pretrained(self._model_name)
        if self._device != "cpu":
            model = model.to(self._device)
        model.eval()
        self._tokenizer = tokenizer
        self._model = model

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
            import torch

            input_ids = self._tokenizer(
                prompt,
                return_tensors="pt",
                truncation=True,
                max_length=_MAX_INPUT_TOKENS,
            ).input_ids
            if self._device != "cpu":
                input_ids = input_ids.to(self._device)

            # max_new_tokens caps the summary length (in model tokens, close
            # enough to tiktoken for sizing). Buffer of 50 absorbs overshoot.
            # no_repeat_ngram_size + repetition_penalty kill flan-t5's degenerate
            # "X: X: X:" repetition loops; beam search + early_stopping favor a
            # complete, coherent summary. min_new_tokens is intentionally dropped
            # — forcing length made the model emit filler/repetition.
            with torch.no_grad():
                output_ids = self._model.generate(
                    input_ids,
                    max_new_tokens=target_tokens + 50,
                    do_sample=False,
                    num_beams=4,
                    no_repeat_ngram_size=3,
                    repetition_penalty=1.3,
                    early_stopping=True,
                )
            return self._tokenizer.decode(output_ids[0], skip_special_tokens=True).strip()
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
