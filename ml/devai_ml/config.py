"""Centralized configuration for the DevAI ML service.

DevAIConfig is the single source of truth for all tunable parameters. Components
consume sub-configs (ChunkingConfig, EmbeddingConfig, etc.) via constructor
injection rather than reading env vars themselves.

Env vars take precedence over defaults. The from_legacy_dict() bridge preserves
backward compatibility with the dict-based config that serve_stdio used before
v0.8.0-alpha.

Sub-configs introduced in v0.8.0-alpha:
  - ChunkingConfig
  - EmbeddingConfig
  - IdleTimeoutConfig (formalized from the standalone DEVAI_ML_IDLE_TIMEOUT_SEC
    env var added in v0.7.1)
"""

from __future__ import annotations

import logging
import os
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)


def _env_int(key: str, default: int) -> int:
    raw = os.environ.get(key)
    if raw is None or raw == "":
        return default
    try:
        return int(raw)
    except ValueError:
        logger.warning("Invalid int for %s=%r, using default %d", key, raw, default)
        return default


def _env_bool(key: str, default: bool) -> bool:
    raw = os.environ.get(key)
    if raw is None:
        return default
    return raw.strip().lower() in ("1", "true", "yes", "on")


def _env_str(key: str, default: str | None = None) -> str | None:
    raw = os.environ.get(key)
    if raw is None or raw == "":
        return default
    return raw


@dataclass(frozen=True)
class ChunkingConfig:
    """Token-based chunk sizing thresholds for SemanticChunker.

    Env vars:
        DEVAI_MAX_CHUNK_TOKENS              (default 512)
        DEVAI_MIN_CHUNK_TOKENS              (default 64)
        DEVAI_LARGE_FUNCTION_THRESHOLD      (default 1024)
    """
    max_chunk_tokens: int = 512
    min_chunk_tokens: int = 64
    large_function_threshold: int = 1024

    def __post_init__(self) -> None:
        if self.min_chunk_tokens >= self.max_chunk_tokens:
            raise ValueError(
                f"min_chunk_tokens ({self.min_chunk_tokens}) must be < "
                f"max_chunk_tokens ({self.max_chunk_tokens})"
            )
        if self.large_function_threshold < self.max_chunk_tokens:
            raise ValueError(
                f"large_function_threshold ({self.large_function_threshold}) must be "
                f">= max_chunk_tokens ({self.max_chunk_tokens})"
            )

    @classmethod
    def from_env(cls) -> "ChunkingConfig":
        return cls(
            max_chunk_tokens=_env_int("DEVAI_MAX_CHUNK_TOKENS", 512),
            min_chunk_tokens=_env_int("DEVAI_MIN_CHUNK_TOKENS", 64),
            large_function_threshold=_env_int("DEVAI_LARGE_FUNCTION_THRESHOLD", 1024),
        )


@dataclass(frozen=True)
class EmbeddingConfig:
    """Embedding provider configuration.

    Env vars:
        DEVAI_EMBEDDING_PROVIDER    local | openai | voyage | custom (default local)
        DEVAI_EMBEDDING_MODEL       model key/name (default minilm-l6)
        DEVAI_EMBEDDING_DEVICE      cpu | cuda (default cpu)
        DEVAI_EMBEDDING_API_KEY     for openai/voyage/custom
        DEVAI_EMBEDDINGS_OFFLINE    true | false | auto (default auto)
    """
    provider: str = "local"
    model: str = "minilm-l6"
    device: str = "cpu"
    api_key: str | None = None
    offline: str = "auto"

    @classmethod
    def from_env(cls) -> "EmbeddingConfig":
        return cls(
            provider=_env_str("DEVAI_EMBEDDING_PROVIDER", "local") or "local",
            model=_env_str("DEVAI_EMBEDDING_MODEL", "minilm-l6") or "minilm-l6",
            device=_env_str("DEVAI_EMBEDDING_DEVICE", "cpu") or "cpu",
            api_key=_env_str("DEVAI_EMBEDDING_API_KEY"),
            offline=_env_str("DEVAI_EMBEDDINGS_OFFLINE", "auto") or "auto",
        )

    def to_legacy_dict(self) -> dict[str, Any]:
        """Convert to the dict format expected by embeddings.factory.create_provider."""
        d: dict[str, Any] = {
            "provider": self.provider,
            "model": self.model,
            "device": self.device,
            "offline": self.offline,
        }
        if self.api_key:
            d["api_key"] = self.api_key
        return d


@dataclass(frozen=True)
class IdleTimeoutConfig:
    """Idle watchdog (v0.7.1). 0 disables the watchdog entirely.

    Env vars:
        DEVAI_ML_IDLE_TIMEOUT_SEC   seconds of inactivity before exit (default 1800)
    """
    timeout_sec: int = 1800

    @classmethod
    def from_env(cls) -> "IdleTimeoutConfig":
        return cls(timeout_sec=_env_int("DEVAI_ML_IDLE_TIMEOUT_SEC", 1800))


@dataclass(frozen=True)
class DevAIConfig:
    """Top-level configuration. Built once at startup, immutable.

    Sub-configs are injected into components via their constructors. Reading env
    vars after this object exists is a smell — extend the schema instead.
    """
    chunking: ChunkingConfig = field(default_factory=ChunkingConfig)
    embedding: EmbeddingConfig = field(default_factory=EmbeddingConfig)
    idle_timeout: IdleTimeoutConfig = field(default_factory=IdleTimeoutConfig)
    state_dir: Path | None = None

    @classmethod
    def from_env(cls) -> "DevAIConfig":
        """Build config from DEVAI_* env vars. Defaults applied for missing vars."""
        state_dir_raw = _env_str("DEVAI_STATE_DIR")
        return cls(
            chunking=ChunkingConfig.from_env(),
            embedding=EmbeddingConfig.from_env(),
            idle_timeout=IdleTimeoutConfig.from_env(),
            state_dir=Path(state_dir_raw) if state_dir_raw else None,
        )

    @classmethod
    def from_legacy_dict(cls, config: dict[str, Any] | None) -> "DevAIConfig":
        """Bridge for the legacy dict-shaped config that MLService used pre-v0.8.0.

        Env vars take precedence over dict values to match current behavior; the
        dict only fills gaps left by missing env vars.
        """
        if not config:
            return cls.from_env()

        base = cls.from_env()
        emb_dict = config.get("embeddings", {}) or {}
        embedding = EmbeddingConfig(
            provider=emb_dict.get("provider", base.embedding.provider),
            model=emb_dict.get("model", base.embedding.model),
            device=emb_dict.get("device", base.embedding.device),
            api_key=emb_dict.get("api_key", base.embedding.api_key),
            offline=str(emb_dict.get("offline", base.embedding.offline)),
        )
        state_dir = config.get("state_dir")
        return replace(
            base,
            embedding=embedding,
            state_dir=Path(state_dir) if state_dir else base.state_dir,
        )
