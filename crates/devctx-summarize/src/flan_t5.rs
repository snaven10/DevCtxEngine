//! Local abstractive summarizer: flan-t5 (seq2seq) via candle. Loads
//! `google/flan-t5-small` from HuggingFace and greedily decodes a summary.
//!
//! Gated behind the `flan-t5` feature (heavy ML stack + model download).

use std::sync::Mutex;

use candle_core::{DType, Device, Tensor, D};
use candle_nn::VarBuilder;
use candle_transformers::models::t5::{Config, T5ForConditionalGeneration};
use tokenizers::Tokenizer;

use crate::error::{Result, SummarizeError};
use crate::provider::Summarizer;

const MODEL_REPO: &str = "google/flan-t5-small";

fn backend(e: impl std::fmt::Display) -> SummarizeError {
    SummarizeError::Backend(e.to_string())
}

/// flan-t5 abstractive summarizer.
pub struct FlanT5Summarizer {
    model: Mutex<T5ForConditionalGeneration>,
    tokenizer: Tokenizer,
    config: Config,
    device: Device,
}

impl FlanT5Summarizer {
    /// Download (if needed) and load flan-t5-small.
    pub fn load() -> Result<Self> {
        use hf_hub::api::sync::Api;
        let api = Api::new().map_err(backend)?;
        let repo = api.model(MODEL_REPO.to_string());

        let config_path = repo.get("config.json").map_err(backend)?;
        let tokenizer_path = repo.get("tokenizer.json").map_err(backend)?;
        let weights_path = repo.get("model.safetensors").map_err(backend)?;

        let config_json = std::fs::read_to_string(config_path).map_err(backend)?;
        let config: Config = serde_json::from_str(&config_json).map_err(backend)?;
        let tokenizer = Tokenizer::from_file(tokenizer_path).map_err(backend)?;

        let device = Device::Cpu;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], DType::F32, &device)
                .map_err(backend)?
        };
        let model = T5ForConditionalGeneration::load(vb, &config).map_err(backend)?;

        Ok(Self {
            model: Mutex::new(model),
            tokenizer,
            config,
            device,
        })
    }

    fn generate(&self, prompt: &str, max_new: usize) -> Result<String> {
        let input_ids = self
            .tokenizer
            .encode(prompt, true)
            .map_err(backend)?
            .get_ids()
            .to_vec();
        let input = Tensor::new(input_ids.as_slice(), &self.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(backend)?;

        let mut model = self.model.lock().expect("summarizer mutex poisoned");
        model.clear_kv_cache();
        let encoder_output = model.encode(&input).map_err(backend)?;

        let start = self
            .config
            .decoder_start_token_id
            .unwrap_or(self.config.pad_token_id) as u32;
        let mut out_ids: Vec<u32> = vec![start];

        for index in 0..max_new {
            let decoder_ids = if index == 0 || !self.config.use_cache {
                Tensor::new(out_ids.as_slice(), &self.device).and_then(|t| t.unsqueeze(0))
            } else {
                Tensor::new(&[*out_ids.last().unwrap()], &self.device).and_then(|t| t.unsqueeze(0))
            }
            .map_err(backend)?;

            let logits = model
                .decode(&decoder_ids, &encoder_output)
                .map_err(backend)?;
            let next = logits
                .squeeze(0)
                .and_then(|l| l.argmax(D::Minus1))
                .and_then(|a| a.to_scalar::<u32>())
                .map_err(backend)?;
            if next as usize == self.config.eos_token_id {
                break;
            }
            out_ids.push(next);
        }

        // Skip the decoder-start token when detokenizing.
        self.tokenizer
            .decode(&out_ids[1..], true)
            .map(|s| s.trim().to_string())
            .map_err(backend)
    }
}

impl Summarizer for FlanT5Summarizer {
    fn summarize(
        &self,
        content: &str,
        query: Option<&str>,
        target_tokens: usize,
    ) -> Result<String> {
        let prompt = match query {
            Some(q) => format!("summarize the following, focusing on {q}: {content}"),
            None => format!("summarize the following: {content}"),
        };
        self.generate(&prompt, target_tokens.max(16))
    }

    fn is_local(&self) -> bool {
        true
    }

    fn supports_query_focus(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "flan-t5"
    }
}
