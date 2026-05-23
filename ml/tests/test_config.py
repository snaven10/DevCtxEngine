"""Tests for the centralized configuration in devai_ml.config.

Covers defaults, env var parsing, dict bridge, validation, and immutability.
"""

from __future__ import annotations

from dataclasses import FrozenInstanceError

import pytest

from devai_ml.config import (
    ChunkingConfig,
    DevAIConfig,
    EmbeddingConfig,
    IdleTimeoutConfig,
    _env_bool,
    _env_int,
    _env_str,
)


# ---------------- helpers ----------------


def _scrub_devai_env(monkeypatch):
    """Remove every DEVAI_* env var so defaults are observable."""
    import os
    for key in list(os.environ.keys()):
        if key.startswith("DEVAI_"):
            monkeypatch.delenv(key, raising=False)


# ---------------- defaults ----------------


def test_defaults_when_no_env(monkeypatch):
    _scrub_devai_env(monkeypatch)
    cfg = DevAIConfig.from_env()
    assert cfg.chunking.max_chunk_tokens == 512
    assert cfg.chunking.min_chunk_tokens == 64
    assert cfg.chunking.large_function_threshold == 1024
    assert cfg.embedding.provider == "local"
    assert cfg.embedding.model == "minilm-l6"
    assert cfg.embedding.device == "cpu"
    assert cfg.embedding.api_key is None
    assert cfg.idle_timeout.timeout_sec == 1800
    assert cfg.state_dir is None


# ---------------- env var overrides ----------------


def test_chunking_env_overrides(monkeypatch):
    _scrub_devai_env(monkeypatch)
    monkeypatch.setenv("DEVAI_MAX_CHUNK_TOKENS", "1024")
    monkeypatch.setenv("DEVAI_MIN_CHUNK_TOKENS", "128")
    monkeypatch.setenv("DEVAI_LARGE_FUNCTION_THRESHOLD", "2048")
    cfg = ChunkingConfig.from_env()
    assert cfg.max_chunk_tokens == 1024
    assert cfg.min_chunk_tokens == 128
    assert cfg.large_function_threshold == 2048


def test_embedding_env_overrides(monkeypatch):
    _scrub_devai_env(monkeypatch)
    monkeypatch.setenv("DEVAI_EMBEDDING_PROVIDER", "openai")
    monkeypatch.setenv("DEVAI_EMBEDDING_MODEL", "text-embedding-3-small")
    monkeypatch.setenv("DEVAI_EMBEDDING_DEVICE", "cuda")
    monkeypatch.setenv("DEVAI_EMBEDDING_API_KEY", "sk-test")
    cfg = EmbeddingConfig.from_env()
    assert cfg.provider == "openai"
    assert cfg.model == "text-embedding-3-small"
    assert cfg.device == "cuda"
    assert cfg.api_key == "sk-test"


def test_idle_timeout_env_override(monkeypatch):
    _scrub_devai_env(monkeypatch)
    monkeypatch.setenv("DEVAI_ML_IDLE_TIMEOUT_SEC", "600")
    cfg = IdleTimeoutConfig.from_env()
    assert cfg.timeout_sec == 600


def test_idle_timeout_zero_disables(monkeypatch):
    _scrub_devai_env(monkeypatch)
    monkeypatch.setenv("DEVAI_ML_IDLE_TIMEOUT_SEC", "0")
    cfg = IdleTimeoutConfig.from_env()
    assert cfg.timeout_sec == 0


def test_state_dir_env(monkeypatch, tmp_path):
    _scrub_devai_env(monkeypatch)
    monkeypatch.setenv("DEVAI_STATE_DIR", str(tmp_path))
    cfg = DevAIConfig.from_env()
    assert cfg.state_dir == tmp_path


# ---------------- invalid env values fall back to defaults ----------------


def test_invalid_int_uses_default(monkeypatch):
    _scrub_devai_env(monkeypatch)
    monkeypatch.setenv("DEVAI_MAX_CHUNK_TOKENS", "not-a-number")
    cfg = ChunkingConfig.from_env()
    assert cfg.max_chunk_tokens == 512


def test_empty_env_uses_default(monkeypatch):
    _scrub_devai_env(monkeypatch)
    monkeypatch.setenv("DEVAI_EMBEDDING_MODEL", "")
    cfg = EmbeddingConfig.from_env()
    assert cfg.model == "minilm-l6"


# ---------------- validation ----------------


def test_chunking_rejects_min_gte_max():
    with pytest.raises(ValueError, match="must be <"):
        ChunkingConfig(max_chunk_tokens=64, min_chunk_tokens=128)


def test_chunking_rejects_min_equal_max():
    with pytest.raises(ValueError, match="must be <"):
        ChunkingConfig(max_chunk_tokens=128, min_chunk_tokens=128)


def test_chunking_rejects_large_threshold_below_max():
    with pytest.raises(ValueError, match="large_function_threshold"):
        ChunkingConfig(max_chunk_tokens=512, large_function_threshold=256)


# ---------------- legacy dict bridge ----------------


def test_from_legacy_dict_none_falls_back_to_env(monkeypatch):
    _scrub_devai_env(monkeypatch)
    cfg = DevAIConfig.from_legacy_dict(None)
    assert cfg.embedding.provider == "local"


def test_from_legacy_dict_overrides_embedding(monkeypatch):
    _scrub_devai_env(monkeypatch)
    legacy = {
        "embeddings": {
            "provider": "voyage",
            "model": "code-3",
            "device": "cpu",
            "api_key": "voy-test",
        },
    }
    cfg = DevAIConfig.from_legacy_dict(legacy)
    assert cfg.embedding.provider == "voyage"
    assert cfg.embedding.model == "code-3"
    assert cfg.embedding.api_key == "voy-test"
    # other defaults preserved
    assert cfg.chunking.max_chunk_tokens == 512


def test_from_legacy_dict_env_still_applies_for_unspecified_fields(monkeypatch):
    _scrub_devai_env(monkeypatch)
    monkeypatch.setenv("DEVAI_MAX_CHUNK_TOKENS", "1024")
    cfg = DevAIConfig.from_legacy_dict({"embeddings": {"provider": "openai"}})
    assert cfg.chunking.max_chunk_tokens == 1024
    assert cfg.embedding.provider == "openai"


def test_embedding_to_legacy_dict_omits_empty_api_key():
    cfg = EmbeddingConfig(provider="local", model="minilm-l6", device="cpu")
    d = cfg.to_legacy_dict()
    assert "api_key" not in d
    assert d["provider"] == "local"


def test_embedding_to_legacy_dict_includes_api_key_when_set():
    cfg = EmbeddingConfig(provider="openai", api_key="sk-abc")
    d = cfg.to_legacy_dict()
    assert d["api_key"] == "sk-abc"


# ---------------- immutability ----------------


def test_chunking_config_is_frozen():
    cfg = ChunkingConfig()
    with pytest.raises(FrozenInstanceError):
        cfg.max_chunk_tokens = 1024  # type: ignore[misc]


def test_devai_config_is_frozen():
    cfg = DevAIConfig()
    with pytest.raises(FrozenInstanceError):
        cfg.chunking = ChunkingConfig()  # type: ignore[misc]


# ---------------- _env helpers ----------------


def test_env_int_invalid_returns_default(monkeypatch):
    monkeypatch.setenv("TEST_INT_BAD", "abc")
    assert _env_int("TEST_INT_BAD", 42) == 42


def test_env_bool_truthy(monkeypatch):
    monkeypatch.setenv("TEST_B_1", "true")
    monkeypatch.setenv("TEST_B_2", "1")
    monkeypatch.setenv("TEST_B_3", "yes")
    monkeypatch.setenv("TEST_B_4", "on")
    assert _env_bool("TEST_B_1", False) is True
    assert _env_bool("TEST_B_2", False) is True
    assert _env_bool("TEST_B_3", False) is True
    assert _env_bool("TEST_B_4", False) is True


def test_env_bool_falsy(monkeypatch):
    monkeypatch.setenv("TEST_B_5", "false")
    monkeypatch.setenv("TEST_B_6", "0")
    monkeypatch.setenv("TEST_B_7", "no")
    assert _env_bool("TEST_B_5", True) is False
    assert _env_bool("TEST_B_6", True) is False
    assert _env_bool("TEST_B_7", True) is False


def test_env_bool_unset_uses_default():
    assert _env_bool("DEFINITELY_NOT_SET", True) is True
    assert _env_bool("DEFINITELY_NOT_SET", False) is False


def test_env_str_empty_uses_default(monkeypatch):
    monkeypatch.setenv("TEST_STR_EMPTY", "")
    assert _env_str("TEST_STR_EMPTY", "fallback") == "fallback"
