"""OpenAI-backed summarizer. Disabled by default via SummarizerConfig.require_local.

Privacy warning: content is sent to OpenAI servers. Do NOT enable for proprietary
or sensitive code. To use this provider you must explicitly set
DEVAI_SUMMARIZER_REQUIRE_LOCAL=false.
"""

from __future__ import annotations

import logging

logger = logging.getLogger(__name__)


class OpenAISummarizer:
    def __init__(self, model: str = "gpt-4o-mini", api_key: str | None = None) -> None:
        # Defer importing openai until summarize() is actually called. The
        # privacy guard in factory.create_summarizer() blocks this provider
        # before any call happens (default require_local=True), so we must
        # NOT raise on construction — that would leak through the guard.
        self._model = model
        self._api_key = api_key
        self._client = None  # lazy

    def _ensure_client(self):
        if self._client is None:
            try:
                from openai import OpenAI
            except ImportError as exc:
                raise RuntimeError(
                    "OpenAISummarizer requires the `openai` package. "
                    "Install with: pip install 'devai-ml[api]'"
                ) from exc
            self._client = OpenAI(api_key=self._api_key)
        return self._client

    def summarize(
        self,
        content: str,
        query: str | None = None,
        target_tokens: int = 200,
    ) -> str:
        try:
            client = self._ensure_client()
            system = (
                "You produce concise summaries. Preserve all identifiers, file names, "
                "function names, and error codes verbatim. Do not invent details."
            )
            if query and query.strip():
                user_prompt = (
                    f"Summarize the following text in the context of this question: '{query}'.\n"
                    f"Target length: about {target_tokens} tokens.\n\n"
                    f"{content}"
                )
            else:
                user_prompt = (
                    f"Summarize the following text. Target length: about {target_tokens} tokens.\n\n"
                    f"{content}"
                )
            resp = client.chat.completions.create(
                model=self._model,
                messages=[
                    {"role": "system", "content": system},
                    {"role": "user", "content": user_prompt},
                ],
                max_tokens=target_tokens + 50,
                temperature=0.2,
            )
            return resp.choices[0].message.content.strip()
        except Exception as exc:
            logger.warning("OpenAI summarize failed: %s. Returning truncated original.", exc)
            return self._truncate_chars(content, target_tokens * 4)

    def model_name(self) -> str:
        return f"openai-{self._model}"

    def is_local(self) -> bool:
        return False

    def supports_query_focus(self) -> bool:
        return True

    @staticmethod
    def _truncate_chars(content: str, char_budget: int) -> str:
        if len(content) <= char_budget:
            return content
        return content[:char_budget].rsplit(" ", 1)[0] + "..."
