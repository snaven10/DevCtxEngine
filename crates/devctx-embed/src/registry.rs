//! Model registry — pure data mapping model keys to dimensions and backends.
//!
//! See `docs/architecture-spec.md` §5. Deliberately free of
//! any `fastembed`/`ort` types so it compiles and tests without the `local`
//! feature. `local.rs` maps [`LocalModelSpec::builtin`] onto fastembed models.

/// Specification of a locally-runnable embedding model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalModelSpec {
    /// Registry key (e.g. `minilm-l6`).
    pub key: &'static str,
    /// Output vector dimension.
    pub dimension: usize,
    /// HuggingFace repo id (reference / user-defined ONNX download).
    pub hf_repo: &'static str,
    /// fastembed built-in model identifier, or `None` for a user-defined ONNX
    /// model (loaded from a local directory, e.g. Granite).
    pub builtin: Option<&'static str>,
    /// Encoder input token cap, if the model has a notably small one.
    pub max_input_tokens: Option<usize>,
}

/// The full local model registry. Dimensions must match the ONNX models exactly.
pub const LOCAL_MODELS: &[LocalModelSpec] = &[
    LocalModelSpec {
        key: "minilm-l6",
        dimension: 384,
        hf_repo: "sentence-transformers/all-MiniLM-L6-v2",
        builtin: Some("AllMiniLML6V2"),
        max_input_tokens: None,
    },
    LocalModelSpec {
        key: "minilm-l12",
        dimension: 384,
        hf_repo: "sentence-transformers/all-MiniLM-L12-v2",
        builtin: Some("AllMiniLML12V2"),
        max_input_tokens: None,
    },
    LocalModelSpec {
        key: "bge-small",
        dimension: 384,
        hf_repo: "BAAI/bge-small-en-v1.5",
        builtin: Some("BGESmallENV15"),
        max_input_tokens: None,
    },
    LocalModelSpec {
        key: "bge-base",
        dimension: 768,
        hf_repo: "BAAI/bge-base-en-v1.5",
        builtin: Some("BGEBaseENV15"),
        max_input_tokens: None,
    },
    LocalModelSpec {
        key: "ml-minilm",
        dimension: 384,
        hf_repo: "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2",
        builtin: Some("ParaphraseMLMiniLML12V2"),
        max_input_tokens: None,
    },
    LocalModelSpec {
        key: "ml-mpnet",
        dimension: 768,
        hf_repo: "sentence-transformers/paraphrase-multilingual-mpnet-base-v2",
        builtin: Some("ParaphraseMLMpnetBaseV2"),
        // 128-token input cap — drives memory chunking.
        max_input_tokens: Some(128),
    },
    // Granite: user-defined ONNX (int8), best CPU quality/speed. Loaded from a
    // local model directory since fastembed has no built-in variant for it.
    LocalModelSpec {
        key: "ml-granite",
        dimension: 384,
        hf_repo: "ibm-granite/granite-embedding-97m-multilingual-r2",
        builtin: None,
        max_input_tokens: None,
    },
    LocalModelSpec {
        key: "ml-granite-lg",
        dimension: 768,
        hf_repo: "ibm-granite/granite-embedding-311m-multilingual-r2",
        builtin: None,
        max_input_tokens: None,
    },
];

/// The default local model key.
pub const DEFAULT_LOCAL_MODEL: &str = "minilm-l6";

/// Look up a local model spec by key.
pub fn find_local(key: &str) -> Option<&'static LocalModelSpec> {
    LOCAL_MODELS.iter().find(|m| m.key == key)
}

/// Dimension of an OpenAI embedding model key (`small`/`large`/`ada`).
pub fn openai_dimension(model_key: &str) -> Option<usize> {
    match model_key {
        "small" => Some(1536),
        "large" => Some(3072),
        "ada" => Some(1536),
        _ => None,
    }
}

/// Map an OpenAI model key to its API model id.
pub fn openai_model_id(model_key: &str) -> &str {
    match model_key {
        "large" => "text-embedding-3-large",
        "ada" => "text-embedding-ada-002",
        _ => "text-embedding-3-small",
    }
}

/// Dimension of a Voyage embedding model key.
pub fn voyage_dimension(model_key: &str) -> Option<usize> {
    match model_key {
        "code-3" => Some(1024),
        "3-lite" => Some(512),
        "3" => Some(1024),
        _ => None,
    }
}

/// Map a Voyage model key to its API model id.
pub fn voyage_model_id(model_key: &str) -> &str {
    match model_key {
        "3-lite" => "voyage-3-lite",
        "3" => "voyage-3",
        _ => "voyage-code-3",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_keys_are_unique() {
        for (i, a) in LOCAL_MODELS.iter().enumerate() {
            for b in &LOCAL_MODELS[i + 1..] {
                assert_ne!(a.key, b.key, "duplicate key {}", a.key);
            }
        }
    }

    #[test]
    fn default_model_exists_and_is_384() {
        let m = find_local(DEFAULT_LOCAL_MODEL).unwrap();
        assert_eq!(m.dimension, 384);
        assert_eq!(m.builtin, Some("AllMiniLML6V2"));
    }

    #[test]
    fn granite_is_user_defined() {
        let g = find_local("ml-granite").unwrap();
        assert_eq!(g.dimension, 384);
        assert_eq!(g.builtin, None);
        assert_eq!(
            g.hf_repo,
            "ibm-granite/granite-embedding-97m-multilingual-r2"
        );
        let lg = find_local("ml-granite-lg").unwrap();
        assert_eq!(lg.dimension, 768);
        assert_eq!(lg.builtin, None);
    }

    #[test]
    fn ml_mpnet_has_token_cap() {
        assert_eq!(find_local("ml-mpnet").unwrap().max_input_tokens, Some(128));
    }

    #[test]
    fn api_dimensions() {
        assert_eq!(openai_dimension("small"), Some(1536));
        assert_eq!(openai_dimension("large"), Some(3072));
        assert_eq!(openai_model_id("large"), "text-embedding-3-large");
        assert_eq!(voyage_dimension("code-3"), Some(1024));
        assert_eq!(voyage_model_id("3-lite"), "voyage-3-lite");
    }
}
