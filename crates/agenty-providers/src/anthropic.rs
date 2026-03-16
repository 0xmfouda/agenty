use agenty_types::{AgentError, Config, Message, Role};
use reqwest::Client;
use serde::{Deserialize, Serialize};

const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// HTTP client for the Anthropic Messages API.
pub struct AnthropicClient {
    api_key: String,
    http: Client,
}

impl AnthropicClient {
    /// Build a client from an explicit key, falling back to `ANTHROPIC_API_KEY`
    /// when `api_key` is `None`.
    pub fn new(api_key: Option<String>) -> Result<Self, AgentError> {
        let api_key = match api_key {
            Some(k) if !k.is_empty() => k,
            _ => std::env::var(ANTHROPIC_API_KEY_ENV).map_err(|_| {
                AgentError::Config(format!(
                    "{ANTHROPIC_API_KEY_ENV} is not set and no api_key was provided"
                ))
            })?,
        };

        Ok(Self { api_key, http: Client::new() })
    }

    /// Send a non-streaming `messages` request and return the assistant reply.
    pub async fn send_message(
        &self,
        config: &Config,
        messages: &[Message],
    ) -> Result<Message, AgentError> {
        let body = MessagesRequest {
            model: &config.model,
            max_tokens: config.max_tokens,
            system: (!config.system_prompt.is_empty()).then_some(&config.system_prompt),
            messages,
            stream: false,
        };

        let resp = self
            .http
            .post(ANTHROPIC_MESSAGES_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                AgentError::Provider(format!("HTTP request to Anthropic failed: {e}"))
            })?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| {
            AgentError::Provider(format!("failed to read response body (HTTP {status}): {e}"))
        })?;

        if !status.is_success() {
            let detail = serde_json::from_str::<ApiErrorEnvelope>(&body_text)
                .map(|env| format!("{}: {}", env.error.kind, env.error.message))
                .unwrap_or_else(|_| {
                    if body_text.is_empty() {
                        "<empty body>".to_string()
                    } else {
                        body_text.clone()
                    }
                });
            return Err(AgentError::Provider(format!(
                "Anthropic API error (HTTP {status}): {detail}"
            )));
        }

        let parsed: MessagesResponse = serde_json::from_str(&body_text).map_err(|e| {
            AgentError::Provider(format!(
                "failed to decode Anthropic response: {e}; body: {body_text}"
            ))
        })?;

        let content = parsed
            .content
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text),
                ContentBlock::Unknown => None,
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(Message::new(Role::Assistant, content))
    }
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a String>,
    messages: &'a [Message],
    stream: bool,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}
