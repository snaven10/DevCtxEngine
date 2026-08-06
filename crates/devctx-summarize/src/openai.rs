//! OpenAI (abstractive) summarizer via the chat-completions API. Request-building
//! and response-parsing are pure functions so they can be tested offline.

use serde_json::{json, Value};

use crate::error::{Result, SummarizeError};
use crate::provider::Summarizer;

const CHAT_URL: &str = "https://api.openai.com/v1/chat/completions";

/// OpenAI abstractive summarizer.
pub struct OpenAiSummarizer {
    model: String,
    api_key: String,
}

impl OpenAiSummarizer {
    /// Construct from a model id and API key.
    pub fn new(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: api_key.into(),
        }
    }
}

impl Summarizer for OpenAiSummarizer {
    fn summarize(
        &self,
        content: &str,
        query: Option<&str>,
        target_tokens: usize,
    ) -> Result<String> {
        let body = chat_body(&self.model, content, query, target_tokens);
        let resp = ureq::post(CHAT_URL)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| SummarizeError::Http(e.to_string()))?
            .into_json::<Value>()
            .map_err(|e| SummarizeError::BadResponse(e.to_string()))?;
        parse_chat(&resp)
    }

    fn is_local(&self) -> bool {
        false
    }

    fn supports_query_focus(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "openai"
    }
}

/// Build the chat-completions request body.
fn chat_body(model: &str, content: &str, query: Option<&str>, target_tokens: usize) -> Value {
    let system = match query {
        Some(q) => format!(
            "Summarize the text focusing on: {q}. Be concise, preserve identifiers, \
             aim for about {target_tokens} tokens."
        ),
        None => format!(
            "Summarize the text concisely, preserving identifiers, aiming for about \
             {target_tokens} tokens."
        ),
    };
    json!({
        "model": model,
        "max_tokens": target_tokens,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": content },
        ],
    })
}

/// Extract the assistant message content from a chat-completions response.
fn parse_chat(v: &Value) -> Result<String> {
    v.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .ok_or_else(|| SummarizeError::BadResponse("missing choices[0].message.content".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_has_model_messages_and_query_focus() {
        let body = chat_body("gpt-4o-mini", "some text", Some("auth"), 150);
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["max_tokens"], 150);
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("auth"));
        assert_eq!(body["messages"][1]["content"], "some text");
    }

    #[test]
    fn parses_content() {
        let v = json!({ "choices": [{ "message": { "content": " a summary " } }] });
        assert_eq!(parse_chat(&v).unwrap(), "a summary");
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse_chat(&json!({ "nope": true })).is_err());
    }
}
