use std::pin::Pin;
use std::time::Duration;

use agenty_types::{
    AgentError, ChatMessage, Config, ContentBlock, Message, Role, StopReason, ToolSpec,
};
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};

use crate::{Token, TokenStream};

const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Retry policy for transient Anthropic API failures.
#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
        }
    }
}

/// HTTP client for the Anthropic Messages API.
pub struct AnthropicClient {
    api_key: String,
    http: Client,
    retry: RetryConfig,
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

        Ok(Self { api_key, http: Client::new(), retry: RetryConfig::default() })
    }

    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    /// Send a non-streaming `messages` request, retrying transient failures.
    pub async fn send_message(
        &self,
        config: &Config,
        messages: &[Message],
    ) -> Result<Message, AgentError> {
        let body = build_request(config, messages, false);
        let resp = self.send_with_retry(&body).await?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| {
            AgentError::Provider(format!("failed to read response body (HTTP {status}): {e}"))
        })?;

        let parsed: MessagesResponse = serde_json::from_str(&body_text).map_err(|e| {
            AgentError::Provider(format!(
                "failed to decode Anthropic response: {e}; body: {body_text}"
            ))
        })?;

        let content = parsed
            .content
            .into_iter()
            .filter_map(|block| match block {
                TextBlock::Text { text } => Some(text),
                TextBlock::Unknown => None,
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(Message::new(Role::Assistant, content))
    }

    /// Stream tokens from a `messages` request using SSE.
    ///
    /// The initial request is retried on transient failures; once the stream
    /// starts, errors propagate as `Err` items and the stream terminates.
    pub async fn stream_message(
        &self,
        config: &Config,
        messages: &[Message],
    ) -> Result<TokenStream<'static>, AgentError> {
        let body = build_request(config, messages, true);
        let resp = self.send_with_retry(&body).await?;

        let events = resp.bytes_stream().eventsource();
        let tokens = events.filter_map(|event| async move {
            match event {
                Err(e) => Some(Err(AgentError::Provider(format!("SSE transport error: {e}")))),
                Ok(event) => parse_sse_event(&event.data).transpose(),
            }
        });

        Ok(Box::pin(tokens) as Pin<Box<dyn Stream<Item = Result<Token, AgentError>> + Send>>)
    }

    /// Send a `messages` request with `tools`, non-streaming, and parse the
    /// full response including `stop_reason` and all content blocks.
    pub async fn send_with_tools(
        &self,
        config: &Config,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantResponse, AgentError> {
        let body = ToolsRequest {
            model: &config.model,
            max_tokens: config.max_tokens,
            system: (!config.system_prompt.is_empty()).then_some(&config.system_prompt),
            tools,
            messages,
        };
        let resp = self.send_with_retry(&body).await?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| {
            AgentError::Provider(format!("failed to read response body (HTTP {status}): {e}"))
        })?;

        let parsed: ToolsResponse = serde_json::from_str(&body_text).map_err(|e| {
            AgentError::Provider(format!(
                "failed to decode Anthropic response: {e}; body: {body_text}"
            ))
        })?;

        Ok(AssistantResponse {
            content: parsed.content,
            stop_reason: parsed.stop_reason,
        })
    }

    /// Send the request with retries, returning the response body-unread on 2xx.
    async fn send_with_retry<B: Serialize + ?Sized>(
        &self,
        body: &B,
    ) -> Result<Response, AgentError> {
        let mut attempt: u32 = 0;
        loop {
            match self.send_once(body).await {
                Ok(resp) => return Ok(resp),
                Err(AttemptError::Fatal(err)) => return Err(err),
                Err(AttemptError::Transient { err, retry_after }) => {
                    if attempt >= self.retry.max_retries {
                        return Err(err);
                    }
                    let delay = retry_after.unwrap_or_else(|| backoff_delay(attempt, self.retry));
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            }
        }
    }

    async fn send_once<B: Serialize + ?Sized>(
        &self,
        body: &B,
    ) -> Result<Response, AttemptError> {
        let resp = self
            .http
            .post(ANTHROPIC_MESSAGES_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                let err = AgentError::Provider(format!("HTTP request to Anthropic failed: {e}"));
                if is_retryable_transport_error(&e) {
                    AttemptError::Transient { err, retry_after: None }
                } else {
                    AttemptError::Fatal(err)
                }
            })?;

        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }

        let retry_after = parse_retry_after(&resp);
        let body_text = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<ApiErrorEnvelope>(&body_text)
            .map(|env| format!("{}: {}", env.error.kind, env.error.message))
            .unwrap_or_else(|_| {
                if body_text.is_empty() { "<empty body>".to_string() } else { body_text }
            });
        let err = AgentError::Provider(format!(
            "Anthropic API error (HTTP {status}): {detail}"
        ));
        Err(if is_retryable_status(status) {
            AttemptError::Transient { err, retry_after }
        } else {
            AttemptError::Fatal(err)
        })
    }
}

enum AttemptError {
    Transient { err: AgentError, retry_after: Option<Duration> },
    Fatal(AgentError),
}

fn build_request<'a>(
    config: &'a Config,
    messages: &'a [Message],
    stream: bool,
) -> MessagesRequest<'a> {
    MessagesRequest {
        model: &config.model,
        max_tokens: config.max_tokens,
        system: (!config.system_prompt.is_empty()).then_some(&config.system_prompt),
        messages,
        stream,
    }
}

fn parse_sse_event(data: &str) -> Result<Option<Token>, AgentError> {
    match serde_json::from_str::<StreamEvent>(data) {
        Ok(StreamEvent::ContentBlockDelta {
            delta: ContentDelta::TextDelta { text },
            ..
        }) => Ok(Some(text)),
        Ok(StreamEvent::Error { error }) => Err(AgentError::Provider(format!(
            "Anthropic stream error: {}: {}",
            error.kind, error.message
        ))),
        Ok(_) => Ok(None),
        Err(e) => Err(AgentError::Provider(format!(
            "failed to decode SSE event: {e}; data: {data}"
        ))),
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504 | 529)
}

fn is_retryable_transport_error(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_body() || e.is_request()
}

fn parse_retry_after(resp: &Response) -> Option<Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn backoff_delay(attempt: u32, cfg: RetryConfig) -> Duration {
    let multiplier = 1u32 << attempt.min(16);
    let raw = cfg.initial_backoff.saturating_mul(multiplier);
    raw.min(cfg.max_backoff)
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
    content: Vec<TextBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TextBlock {
    Text { text: String },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamEvent {
    ContentBlockDelta { #[allow(dead_code)] index: u32, delta: ContentDelta },
    Error { error: ApiErrorDetail },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentDelta {
    TextDelta { text: String },
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

// ---------------------------------------------------------------------------
// Tool-use path
// ---------------------------------------------------------------------------

/// Full non-streaming response from a tool-aware request.
#[derive(Debug, Clone)]
pub struct AssistantResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
}

#[derive(Serialize)]
struct ToolsRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a String>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    tools: &'a [ToolSpec],
    messages: &'a [ChatMessage],
}

#[derive(Deserialize)]
struct ToolsResponse {
    content: Vec<ContentBlock>,
    stop_reason: StopReason,
}
