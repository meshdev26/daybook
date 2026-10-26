use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::error::AiError;
use super::provider::{GenerateRequest, GenerateResponse, TokenUsage};

#[derive(Clone)]
pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(base_url: String, api_key: String) -> Result<Self, AiError> {
        let base_url = if base_url.trim().is_empty() {
            "https://api.anthropic.com/v1".to_string()
        } else {
            base_url
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| AiError::Provider(format!("failed to build HTTP client: {}", e)))?;
        Ok(Self {
            base_url,
            api_key,
            client,
        })
    }
}

#[derive(Serialize)]
struct AReq {
    model: String,
    max_tokens: i64,
    messages: Vec<AMsg>,
    system: Option<String>,
}
#[derive(Serialize)]
struct AMsg {
    role: String,
    content: String,
}
#[derive(Deserialize)]
struct AResp {
    content: Vec<AContent>,
    usage: Option<AUsage>,
}
#[derive(Deserialize)]
struct AContent {
    text: Option<String>,
}
#[derive(Deserialize)]
struct AUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

#[async_trait]
impl super::provider::AiProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, AiError> {
        // split system message
        let mut system = None;
        let mut msgs = vec![];
        for m in req.messages {
            if m.role == "system" {
                system = Some(m.content);
            } else {
                msgs.push(AMsg {
                    role: m.role,
                    content: m.content,
                });
            }
        }
        let body = AReq {
            model: req.model,
            max_tokens: req.max_tokens,
            messages: msgs,
            system,
        };
        let url = format!("{}/messages", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| AiError::Provider(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AiError::Provider(e.to_string()))?;
        if !status.is_success() {
            return Err(AiError::Provider(format!("HTTP {} - {}", status, text)));
        }
        let parsed: AResp =
            serde_json::from_str(&text).map_err(|e| AiError::Parse(e.to_string()))?;
        let content = parsed
            .content
            .into_iter()
            .filter_map(|c| c.text)
            .collect::<Vec<_>>()
            .join("");
        let usage = parsed
            .usage
            .map(|u| TokenUsage {
                prompt_tokens: u.input_tokens.unwrap_or(0),
                completion_tokens: u.output_tokens.unwrap_or(0),
                total_tokens: u.input_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0),
            })
            .unwrap_or(TokenUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            });
        Ok(GenerateResponse { content, usage })
    }
}
