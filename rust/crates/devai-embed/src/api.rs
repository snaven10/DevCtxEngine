//! API-backed embedding providers: OpenAI, Voyage, and a generic custom HTTP
//! endpoint. Request-building and response-parsing are pure functions so they
//! can be tested offline; only [`post_json`] touches the network.

use serde_json::{json, Value};

use crate::error::{EmbedError, Result};
use crate::provider::EmbeddingProvider;

const OPENAI_URL: &str = "https://api.openai.com/v1/embeddings";
const VOYAGE_URL: &str = "https://api.voyageai.com/v1/embeddings";

/// POST a JSON body with a Bearer token and decode the JSON response.
fn post_json(url: &str, api_key: &str, body: Value) -> Result<Value> {
    let resp = ureq::post(url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|e| EmbedError::Http(e.to_string()))?;
    resp.into_json::<Value>()
        .map_err(|e| EmbedError::BadResponse(e.to_string()))
}

/// Build the request body for OpenAI-style embeddings.
fn openai_style_body(model_id: &str, texts: &[String], input_type: Option<&str>) -> Value {
    let mut body = json!({ "model": model_id, "input": texts });
    if let Some(it) = input_type {
        body["input_type"] = json!(it);
    }
    body
}

/// Parse an OpenAI/Voyage-style `{ "data": [{ "embedding": [...], "index": n }] }`
/// response into embeddings ordered by `index`.
fn parse_data_embeddings(v: &Value) -> Result<Vec<Vec<f32>>> {
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| EmbedError::BadResponse("missing `data` array".into()))?;
    let mut indexed: Vec<(u64, Vec<f32>)> = Vec::with_capacity(data.len());
    for (i, item) in data.iter().enumerate() {
        let idx = item
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(i as u64);
        let emb = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| EmbedError::BadResponse("missing `embedding`".into()))?;
        indexed.push((idx, json_floats(emb)?));
    }
    indexed.sort_by_key(|(i, _)| *i);
    Ok(indexed.into_iter().map(|(_, e)| e).collect())
}

/// Parse a `{ "vectors": [[...], ...] }` response (custom endpoint).
fn parse_vectors(v: &Value) -> Result<Vec<Vec<f32>>> {
    let arr = v
        .get("vectors")
        .and_then(Value::as_array)
        .ok_or_else(|| EmbedError::BadResponse("missing `vectors` array".into()))?;
    arr.iter()
        .map(|row| {
            row.as_array()
                .ok_or_else(|| EmbedError::BadResponse("`vectors` element is not an array".into()))
                .and_then(|r| json_floats(r))
        })
        .collect()
}

fn json_floats(arr: &[Value]) -> Result<Vec<f32>> {
    arr.iter()
        .map(|x| {
            x.as_f64()
                .map(|f| f as f32)
                .ok_or_else(|| EmbedError::BadResponse("non-numeric embedding element".into()))
        })
        .collect()
}

/// OpenAI embeddings provider.
pub struct OpenAiProvider {
    model_id: String,
    dimension: usize,
    api_key: String,
}

impl OpenAiProvider {
    /// Construct from a model id, dimension and API key.
    pub fn new(model_id: impl Into<String>, dimension: usize, api_key: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            dimension,
            api_key: api_key.into(),
        }
    }
}

impl EmbeddingProvider for OpenAiProvider {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let body = openai_style_body(&self.model_id, texts, None);
        parse_data_embeddings(&post_json(OPENAI_URL, &self.api_key, body)?)
    }
    fn dimension(&self) -> usize {
        self.dimension
    }
    fn model_name(&self) -> &str {
        &self.model_id
    }
}

/// Voyage (code-optimized) embeddings provider.
pub struct VoyageProvider {
    model_id: String,
    dimension: usize,
    api_key: String,
}

impl VoyageProvider {
    /// Construct from a model id, dimension and API key.
    pub fn new(model_id: impl Into<String>, dimension: usize, api_key: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            dimension,
            api_key: api_key.into(),
        }
    }
}

impl EmbeddingProvider for VoyageProvider {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let body = openai_style_body(&self.model_id, texts, Some("document"));
        parse_data_embeddings(&post_json(VOYAGE_URL, &self.api_key, body)?)
    }
    fn dimension(&self) -> usize {
        self.dimension
    }
    fn model_name(&self) -> &str {
        &self.model_id
    }
}

/// Generic custom HTTP embedding provider (`POST {endpoint}/embed`).
pub struct CustomProvider {
    endpoint: String,
    model_id: String,
    dimension: usize,
    api_key: String,
}

impl CustomProvider {
    /// Construct from an endpoint base URL, model id, dimension and optional key.
    pub fn new(
        endpoint: impl Into<String>,
        model_id: impl Into<String>,
        dimension: usize,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            model_id: model_id.into(),
            dimension,
            api_key: api_key.into(),
        }
    }
}

impl EmbeddingProvider for CustomProvider {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("{}/embed", self.endpoint.trim_end_matches('/'));
        let body = json!({ "texts": texts, "model": self.model_id });
        parse_vectors(&post_json(&url, &self.api_key, body)?)
    }
    fn dimension(&self) -> usize {
        self.dimension
    }
    fn model_name(&self) -> &str {
        &self.model_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_body_has_model_and_input() {
        let body = openai_style_body("text-embedding-3-small", &["a".into(), "b".into()], None);
        assert_eq!(body["model"], "text-embedding-3-small");
        assert_eq!(body["input"][0], "a");
        assert!(body.get("input_type").is_none());
    }

    #[test]
    fn voyage_body_sets_input_type() {
        let body = openai_style_body("voyage-code-3", &["x".into()], Some("document"));
        assert_eq!(body["input_type"], "document");
    }

    #[test]
    fn parses_data_and_orders_by_index() {
        // Deliberately out of order.
        let v = json!({
            "data": [
                { "embedding": [0.0, 1.0], "index": 1 },
                { "embedding": [1.0, 0.0], "index": 0 }
            ]
        });
        let out = parse_data_embeddings(&v).unwrap();
        assert_eq!(out, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    }

    #[test]
    fn parses_custom_vectors() {
        let v = json!({ "vectors": [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]], "dimension": 3 });
        let out = parse_vectors(&v).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[1], vec![4.0, 5.0, 6.0]);
    }

    #[test]
    fn rejects_malformed_response() {
        assert!(parse_data_embeddings(&json!({ "nope": true })).is_err());
        assert!(parse_vectors(&json!({ "nope": true })).is_err());
    }
}
