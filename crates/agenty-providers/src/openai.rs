//! OpenAI chat-completions backend.
//!
//! Translates the workspace's unified [`ChatMessage`] / [`ContentBlock`] /
//! [`ToolSpec`] types to OpenAI's wire format, and decodes both non-streaming
//! responses and SSE deltas back into the shared [`AssistantResponse`] /
//! [`ProviderStreamEvent`] shape used by the rest of the system.

use std::collections::BTreeMap;
use std::time::Duration;

use agenty_core::{
    AgentError, ChatMessage, Config, ContentBlock, JsonValue, Role, StopReason, ToolSpec,
};
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};

use crate::{AssistantResponse, BlockKind, ProviderEventStream, ProviderStreamEvent};

const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
const OPENAI_CHAT_URL: &str = "https://api.openai.com/v1/chat/completions";

/// Retry policy for transient OpenAI API failures.
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

pub struct OpenAIClient {
    api_key: String,
    http: Client,
    retry: RetryConfig,
}

impl OpenAIClient {
    pub fn new(api_key: Option<String>) -> Result<Self, AgentError> {
        let api_key = match api_key {
            Some(k) if !k.is_empty() => k,
            _ => std::env::var(OPENAI_API_KEY_ENV).map_err(|_| {
                AgentError::Config(format!(
                    "{OPENAI_API_KEY_ENV} is not set and no api_key was provided"
                ))
            })?,
        };
        Ok(Self { api_key, http: Client::new(), retry: RetryConfig::default() })
    }

    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    pub async fn send_with_tools(
        &self,
        config: &Config,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantResponse, AgentError> {
        let body = build_request(config, messages, tools, false);
        let resp = self.send_with_retry(&body).await?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| {
            AgentError::Provider(format!("failed to read response body (HTTP {status}): {e}"))
        })?;

        let parsed: ChatResponse = serde_json::from_str(&body_text).map_err(|e| {
            AgentError::Provider(format!(
                "failed to decode OpenAI response: {e}; body: {body_text}"
            ))
        })?;

        let choice = parsed.choices.into_iter().next().ok_or_else(|| {
            AgentError::Provider("OpenAI response contained no choices".into())
        })?;

        let stop_reason = finish_to_stop(choice.finish_reason.as_deref());
        let content = assistant_message_to_blocks(choice.message);
        Ok(AssistantResponse { content, stop_reason })
    }

    pub async fn stream_with_tools(
        &self,
        config: &Config,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<ProviderEventStream, AgentError> {
        let body = build_request(config, messages, tools, true);
        let resp = self.send_with_retry(&body).await?;

        let events = resp.bytes_stream().eventsource();
        let mut decoder = StreamDecoder::new();

        let stream = events
            .map(move |event| match event {
                Err(e) => vec![Err(AgentError::Provider(format!("SSE transport error: {e}")))],
                Ok(event) => {
                    if event.data.trim() == "[DONE]" {
                        decoder
                            .finish()
                            .into_iter()
                            .map(Ok)
                            .collect()
                    } else {
                        match decoder.push(&event.data) {
                            Ok(out) => out.into_iter().map(Ok).collect(),
                            Err(e) => vec![Err(e)],
                        }
                    }
                }
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(stream) as ProviderEventStream)
    }

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
            .post(OPENAI_CHAT_URL)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                let err = AgentError::Provider(format!("HTTP request to OpenAI failed: {e}"));
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
            .map(|env| format!("{}: {}", env.error.kind.unwrap_or_default(), env.error.message))
            .unwrap_or_else(|_| {
                if body_text.is_empty() { "<empty body>".to_string() } else { body_text }
            });
        let err = AgentError::Provider(format!("OpenAI API error (HTTP {status}): {detail}"));
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

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

fn build_request(
    config: &Config,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    stream: bool,
) -> ChatRequest {
    let mut wire_messages: Vec<WireMessage> = Vec::new();
    if !config.system_prompt.is_empty() {
        wire_messages.push(WireMessage {
            role: "system".into(),
            content: Some(WireContent::Text(config.system_prompt.clone())),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    for msg in messages {
        wire_messages.extend(chat_to_wire(msg));
    }

    let wire_tools = tools
        .iter()
        .map(|t| WireTool {
            kind: "function",
            function: WireFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.input_schema.clone(),
            },
        })
        .collect();

    let stream_options = stream.then_some(StreamOptions { include_usage: true });

    ChatRequest {
        model: config.model.clone(),
        messages: wire_messages,
        tools: wire_tools,
        max_tokens: Some(config.max_tokens),
        stream,
        stream_options,
    }
}

/// Translate a unified [`ChatMessage`] into one or more OpenAI wire messages.
///
/// Fan-out happens when a user turn carries `ToolResult` blocks: OpenAI
/// models each tool result as its own `role: "tool"` message.
fn chat_to_wire(msg: &ChatMessage) -> Vec<WireMessage> {
    match msg.role {
        Role::User => {
            let mut out = Vec::new();
            let mut text_buf = String::new();
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        if !text_buf.is_empty() {
                            text_buf.push('\n');
                        }
                        text_buf.push_str(text);
                    }
                    ContentBlock::ToolResult { tool_use_id, content, .. } => {
                        out.push(WireMessage {
                            role: "tool".into(),
                            content: Some(WireContent::Text(content.clone())),
                            tool_calls: None,
                            tool_call_id: Some(tool_use_id.clone()),
                        });
                    }
                    _ => {}
                }
            }
            if !text_buf.is_empty() {
                // User text precedes any tool results when both are present.
                out.insert(
                    0,
                    WireMessage {
                        role: "user".into(),
                        content: Some(WireContent::Text(text_buf)),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                );
            }
            out
        }
        Role::Assistant => {
            let mut text_buf = String::new();
            let mut tool_calls: Vec<WireToolCall> = Vec::new();
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        if !text_buf.is_empty() {
                            text_buf.push('\n');
                        }
                        text_buf.push_str(text);
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_calls.push(WireToolCall {
                            id: id.clone(),
                            kind: "function",
                            function: WireToolCallFn {
                                name: name.clone(),
                                arguments: serde_json::to_string(input).unwrap_or_default(),
                            },
                        });
                    }
                    _ => {}
                }
            }
            let content = if text_buf.is_empty() {
                None
            } else {
                Some(WireContent::Text(text_buf))
            };
            vec![WireMessage {
                role: "assistant".into(),
                content,
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                tool_call_id: None,
            }]
        }
        Role::Tool => Vec::new(),
    }
}

fn assistant_message_to_blocks(msg: WireAssistantMessage) -> Vec<ContentBlock> {
    let mut blocks: Vec<ContentBlock> = Vec::new();
    if let Some(text) = msg.content
        && !text.is_empty()
    {
        blocks.push(ContentBlock::Text { text });
    }
    if let Some(calls) = msg.tool_calls {
        for call in calls {
            let input: JsonValue = if call.function.arguments.is_empty() {
                JsonValue::Object(serde_json::Map::new())
            } else {
                serde_json::from_str(&call.function.arguments).unwrap_or(JsonValue::Null)
            };
            blocks.push(ContentBlock::ToolUse {
                id: call.id,
                name: call.function.name,
                input,
            });
        }
    }
    blocks
}

fn finish_to_stop(finish: Option<&str>) -> StopReason {
    match finish {
        Some("stop") => StopReason::EndTurn,
        Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::Other,
        _ => StopReason::EndTurn,
    }
}

// ---------------------------------------------------------------------------
// Streaming decoder
// ---------------------------------------------------------------------------

/// Buffered per-tool-call state used to emit `BlockStart` the first time we
/// see a given `index` and subsequent `ToolInputDelta`s as argument fragments
/// arrive.
struct ToolCallState {
    id: Option<String>,
    name: Option<String>,
    started: bool,
    block_index: u32,
}

/// Decodes OpenAI streaming deltas into a sequence of [`ProviderStreamEvent`]s
/// in the Anthropic-inspired shape used workspace-wide.
struct StreamDecoder {
    /// `true` once we've emitted the initial `BlockStart` for the text block
    /// (block index 0). Tool-use blocks use indices 1..N in first-seen order.
    text_started: bool,
    /// Buffered text so we can detect whether to emit a text BlockStart lazily.
    text_block_index: u32,
    tool_calls: BTreeMap<u32, ToolCallState>,
    /// Next block index to hand out to a new tool call.
    next_block_index: u32,
    stop_reason: Option<StopReason>,
    stopped: bool,
}

impl StreamDecoder {
    fn new() -> Self {
        Self {
            text_started: false,
            text_block_index: 0,
            tool_calls: BTreeMap::new(),
            next_block_index: 1,
            stop_reason: None,
            stopped: false,
        }
    }

    fn push(&mut self, data: &str) -> Result<Vec<ProviderStreamEvent>, AgentError> {
        let parsed: StreamChunk = serde_json::from_str(data).map_err(|e| {
            AgentError::Provider(format!("failed to decode OpenAI stream chunk: {e}; data: {data}"))
        })?;

        let mut out = Vec::new();

        if let Some(usage) = parsed.usage {
            out.push(ProviderStreamEvent::Usage {
                input_tokens: usage.prompt_tokens.unwrap_or(0),
                output_tokens: usage.completion_tokens.unwrap_or(0),
            });
        }

        let Some(choice) = parsed.choices.into_iter().next() else {
            return Ok(out);
        };

        if let Some(delta) = choice.delta {
            if let Some(text) = delta.content
                && !text.is_empty()
            {
                if !self.text_started {
                    self.text_started = true;
                    out.push(ProviderStreamEvent::BlockStart {
                        index: self.text_block_index,
                        kind: BlockKind::Text,
                    });
                }
                out.push(ProviderStreamEvent::TextDelta {
                    index: self.text_block_index,
                    text,
                });
            }

            if let Some(calls) = delta.tool_calls {
                for call in calls {
                    let state = self.tool_calls.entry(call.index).or_insert_with(|| {
                        let block_index = self.next_block_index;
                        self.next_block_index += 1;
                        ToolCallState {
                            id: None,
                            name: None,
                            started: false,
                            block_index,
                        }
                    });

                    if let Some(id) = call.id {
                        state.id = Some(id);
                    }
                    if let Some(func) = call.function.as_ref() {
                        if let Some(name) = &func.name {
                            state.name = Some(name.clone());
                        }
                    }

                    if !state.started && state.id.is_some() && state.name.is_some() {
                        state.started = true;
                        out.push(ProviderStreamEvent::BlockStart {
                            index: state.block_index,
                            kind: BlockKind::ToolUse {
                                id: state.id.clone().unwrap(),
                                name: state.name.clone().unwrap(),
                            },
                        });
                    }

                    if state.started
                        && let Some(func) = call.function
                        && let Some(args) = func.arguments
                        && !args.is_empty()
                    {
                        out.push(ProviderStreamEvent::ToolInputDelta {
                            index: state.block_index,
                            partial_json: args,
                        });
                    }
                }
            }
        }

        if let Some(finish) = choice.finish_reason {
            self.stop_reason = Some(finish_to_stop(Some(&finish)));
        }

        Ok(out)
    }

    fn finish(&mut self) -> Vec<ProviderStreamEvent> {
        if self.stopped {
            return Vec::new();
        }
        self.stopped = true;
        let mut out = Vec::new();
        if self.text_started {
            out.push(ProviderStreamEvent::BlockStop { index: self.text_block_index });
        }
        for state in self.tool_calls.values() {
            if state.started {
                out.push(ProviderStreamEvent::BlockStop { index: state.block_index });
            }
        }
        out.push(ProviderStreamEvent::StopReason(
            self.stop_reason.unwrap_or(StopReason::EndTurn),
        ));
        out.push(ProviderStreamEvent::MessageStop);
        out
    }
}

// ---------------------------------------------------------------------------
// Transport helpers
// ---------------------------------------------------------------------------

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504)
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

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct WireMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<WireContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireContent {
    Text(String),
}

#[derive(Serialize)]
struct WireTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireFunction,
}

#[derive(Serialize)]
struct WireFunction {
    name: String,
    description: String,
    parameters: JsonValue,
}

#[derive(Serialize)]
struct WireToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireToolCallFn,
}

#[derive(Serialize)]
struct WireToolCallFn {
    name: String,
    arguments: String,
}

// Response (non-streaming)

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: WireAssistantMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireAssistantMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireAssistantToolCall>>,
}

#[derive(Deserialize)]
struct WireAssistantToolCall {
    id: String,
    function: WireAssistantToolCallFn,
}

#[derive(Deserialize)]
struct WireAssistantToolCallFn {
    name: String,
    #[serde(default)]
    arguments: String,
}

// Streaming

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<StreamUsage>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Option<StreamDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<StreamToolCallDelta>>,
}

#[derive(Deserialize)]
struct StreamToolCallDelta {
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamToolCallFnDelta>,
}

#[derive(Deserialize)]
struct StreamToolCallFnDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct StreamUsage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    #[serde(rename = "type")]
    #[serde(default)]
    kind: Option<String>,
    message: String,
}

