//! Google Gemini (Generative Language API) backend.
//!
//! Translates the workspace's unified [`ChatMessage`] / [`ContentBlock`] /
//! [`ToolSpec`] types to Gemini's wire format (roles `user` / `model`,
//! `parts` with `text` / `functionCall` / `functionResponse`), and decodes
//! both non-streaming responses and SSE deltas back into the shared
//! [`AssistantResponse`] / [`ProviderStreamEvent`] shape used elsewhere.
//!
//! Notable translation caveats:
//! * Gemini does not emit tool-call IDs; we synthesize stable ones from
//!   `<function_name>-<index>` so follow-up `functionResponse` parts can be
//!   matched back on subsequent turns.
//! * `ToolResult` blocks carry `tool_use_id` but not the original function
//!   name, which Gemini requires in `functionResponse.name`. We walk the
//!   conversation history to recover the name from the earlier `ToolUse`.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use agenty_core::{
    AgentError, ChatMessage, Config, ContentBlock, JsonValue, Role, StopReason, ToolSpec,
};
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};

use crate::{AssistantResponse, BlockKind, ProviderEventStream, ProviderStreamEvent};

const GEMINI_API_KEY_ENV: &str = "GEMINI_API_KEY";
const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

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

pub struct GeminiClient {
    api_key: String,
    http: Client,
    retry: RetryConfig,
}

impl GeminiClient {
    pub fn new(api_key: Option<String>) -> Result<Self, AgentError> {
        let api_key = match api_key {
            Some(k) if !k.is_empty() => k,
            _ => std::env::var(GEMINI_API_KEY_ENV).map_err(|_| {
                AgentError::Config(format!(
                    "{GEMINI_API_KEY_ENV} is not set and no api_key was provided"
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
        let body = build_request(config, messages, tools);
        let url = format!("{GEMINI_BASE_URL}/{}:generateContent", config.model);
        let resp = self.send_with_retry(&url, &body).await?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| {
            AgentError::Provider(format!("failed to read response body (HTTP {status}): {e}"))
        })?;

        let parsed: GenerateResponse = serde_json::from_str(&body_text).map_err(|e| {
            AgentError::Provider(format!(
                "failed to decode Gemini response: {e}; body: {body_text}"
            ))
        })?;

        let candidate = parsed.candidates.into_iter().next().ok_or_else(|| {
            AgentError::Provider("Gemini response contained no candidates".into())
        })?;

        let parts = candidate.content.map(|c| c.parts).unwrap_or_default();
        let mut content = Vec::new();
        let mut tool_counter: HashMap<String, u32> = HashMap::new();
        let mut saw_tool = false;
        for part in parts {
            if let Some(text) = part.text {
                if !text.is_empty() {
                    content.push(ContentBlock::Text { text });
                }
            } else if let Some(fc) = part.function_call {
                saw_tool = true;
                let id = synth_tool_id(&mut tool_counter, &fc.name);
                content.push(ContentBlock::ToolUse {
                    id,
                    name: fc.name,
                    input: fc.args.unwrap_or(JsonValue::Null),
                });
            }
        }

        let stop_reason = if saw_tool {
            StopReason::ToolUse
        } else {
            finish_to_stop(candidate.finish_reason.as_deref())
        };

        Ok(AssistantResponse { content, stop_reason })
    }

    pub async fn stream_with_tools(
        &self,
        config: &Config,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<ProviderEventStream, AgentError> {
        let body = build_request(config, messages, tools);
        let url = format!(
            "{GEMINI_BASE_URL}/{}:streamGenerateContent?alt=sse",
            config.model
        );
        let resp = self.send_with_retry(&url, &body).await?;

        let events = resp.bytes_stream().eventsource();
        let mut decoder = StreamDecoder::new();

        let stream = events
            .map(move |event| match event {
                Err(e) => vec![Err(AgentError::Provider(format!("SSE transport error: {e}")))],
                Ok(event) => match decoder.push(&event.data) {
                    Ok(out) => out.into_iter().map(Ok).collect(),
                    Err(e) => vec![Err(e)],
                },
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(stream) as ProviderEventStream)
    }

    async fn send_with_retry<B: Serialize + ?Sized>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<Response, AgentError> {
        let mut attempt: u32 = 0;
        loop {
            match self.send_once(url, body).await {
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
        url: &str,
        body: &B,
    ) -> Result<Response, AttemptError> {
        let resp = self
            .http
            .post(url)
            .header("x-goog-api-key", &self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                let err = AgentError::Provider(format!("HTTP request to Gemini failed: {e}"));
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
            .map(|env| {
                format!(
                    "{}: {}",
                    env.error.status.unwrap_or_default(),
                    env.error.message
                )
            })
            .unwrap_or_else(|_| {
                if body_text.is_empty() { "<empty body>".to_string() } else { body_text }
            });
        let err = AgentError::Provider(format!("Gemini API error (HTTP {status}): {detail}"));
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

fn synth_tool_id(counter: &mut HashMap<String, u32>, name: &str) -> String {
    let n = counter.entry(name.to_string()).or_insert(0);
    let id = format!("{name}-{n}");
    *n += 1;
    id
}

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

fn build_request(config: &Config, messages: &[ChatMessage], tools: &[ToolSpec]) -> GenerateRequest {
    // Build a (tool_use_id → name) map by walking the assistant turns. Needed
    // because `ContentBlock::ToolResult` doesn't carry the function name but
    // Gemini's `functionResponse` requires it.
    let mut id_to_name: HashMap<String, String> = HashMap::new();
    for msg in messages {
        if msg.role == Role::Assistant {
            for block in &msg.content {
                if let ContentBlock::ToolUse { id, name, .. } = block {
                    id_to_name.insert(id.clone(), name.clone());
                }
            }
        }
    }

    let contents: Vec<GeminiContent> = messages
        .iter()
        .filter_map(|m| chat_to_content(m, &id_to_name))
        .collect();

    let system_instruction = (!config.system_prompt.is_empty()).then(|| GeminiContent {
        role: None,
        parts: vec![GeminiPartOut::Text { text: config.system_prompt.clone() }],
    });

    let wire_tools = (!tools.is_empty()).then(|| {
        vec![GeminiTool {
            function_declarations: tools
                .iter()
                .map(|t| GeminiFunctionDecl {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                })
                .collect(),
        }]
    });

    GenerateRequest {
        contents,
        system_instruction,
        tools: wire_tools,
        generation_config: Some(GenerationConfig {
            max_output_tokens: Some(config.max_tokens),
        }),
    }
}

fn chat_to_content(
    msg: &ChatMessage,
    id_to_name: &HashMap<String, String>,
) -> Option<GeminiContent> {
    let mut parts: Vec<GeminiPartOut> = Vec::new();

    for block in &msg.content {
        match block {
            ContentBlock::Text { text } => {
                if !text.is_empty() {
                    parts.push(GeminiPartOut::Text { text: text.clone() });
                }
            }
            ContentBlock::ToolUse { id: _, name, input } => {
                parts.push(GeminiPartOut::FunctionCall {
                    function_call: GeminiFunctionCallOut {
                        name: name.clone(),
                        args: input.clone(),
                    },
                });
            }
            ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                let name = id_to_name
                    .get(tool_use_id)
                    .cloned()
                    .unwrap_or_else(|| tool_use_id.clone());
                let mut response = serde_json::Map::new();
                response.insert("content".into(), JsonValue::String(content.clone()));
                if *is_error {
                    response.insert("is_error".into(), JsonValue::Bool(true));
                }
                parts.push(GeminiPartOut::FunctionResponse {
                    function_response: GeminiFunctionResponseOut {
                        name,
                        response: JsonValue::Object(response),
                    },
                });
            }
            ContentBlock::Thinking { .. } => {}
        }
    }

    if parts.is_empty() {
        return None;
    }

    let role = match msg.role {
        Role::User | Role::Tool => "user",
        Role::Assistant => "model",
    };
    Some(GeminiContent { role: Some(role.to_string()), parts })
}

fn finish_to_stop(finish: Option<&str>) -> StopReason {
    match finish {
        Some("STOP") => StopReason::EndTurn,
        Some("MAX_TOKENS") => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    }
}

// ---------------------------------------------------------------------------
// Streaming decoder
// ---------------------------------------------------------------------------

struct StreamDecoder {
    text_started: bool,
    text_block_index: u32,
    next_block_index: u32,
    /// Synthesized tool-call IDs, keyed by function name so repeat calls get
    /// incrementing suffixes.
    tool_counter: HashMap<String, u32>,
    tool_blocks: BTreeMap<u32, ToolBlockState>,
    stop_reason: Option<StopReason>,
    saw_tool: bool,
    finished: bool,
}

struct ToolBlockState {
    _id: String,
    _name: String,
}

impl StreamDecoder {
    fn new() -> Self {
        Self {
            text_started: false,
            text_block_index: 0,
            next_block_index: 1,
            tool_counter: HashMap::new(),
            tool_blocks: BTreeMap::new(),
            stop_reason: None,
            saw_tool: false,
            finished: false,
        }
    }

    fn push(&mut self, data: &str) -> Result<Vec<ProviderStreamEvent>, AgentError> {
        let parsed: GenerateResponse = serde_json::from_str(data).map_err(|e| {
            AgentError::Provider(format!("failed to decode Gemini stream chunk: {e}; data: {data}"))
        })?;

        let mut out = Vec::new();

        if let Some(usage) = parsed.usage_metadata.as_ref() {
            out.push(ProviderStreamEvent::Usage {
                input_tokens: usage.prompt_token_count.unwrap_or(0),
                output_tokens: usage.candidates_token_count.unwrap_or(0),
            });
        }

        let Some(candidate) = parsed.candidates.into_iter().next() else {
            return Ok(out);
        };

        if let Some(content) = candidate.content {
            for part in content.parts {
                if let Some(text) = part.text {
                    if text.is_empty() {
                        continue;
                    }
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
                } else if let Some(fc) = part.function_call {
                    self.saw_tool = true;
                    let block_index = self.next_block_index;
                    self.next_block_index += 1;
                    let id = synth_tool_id(&mut self.tool_counter, &fc.name);
                    out.push(ProviderStreamEvent::BlockStart {
                        index: block_index,
                        kind: BlockKind::ToolUse {
                            id: id.clone(),
                            name: fc.name.clone(),
                        },
                    });
                    // Gemini emits the full args object in one chunk rather
                    // than streaming it piecewise; serialize to JSON so the
                    // consumer can treat it like any other tool-input delta.
                    let args_json =
                        serde_json::to_string(&fc.args.unwrap_or(JsonValue::Null))
                            .unwrap_or_default();
                    if !args_json.is_empty() && args_json != "null" {
                        out.push(ProviderStreamEvent::ToolInputDelta {
                            index: block_index,
                            partial_json: args_json,
                        });
                    }
                    out.push(ProviderStreamEvent::BlockStop { index: block_index });
                    self.tool_blocks
                        .insert(block_index, ToolBlockState { _id: id, _name: fc.name });
                }
            }
        }

        if let Some(reason) = candidate.finish_reason {
            if !self.finished {
                self.finished = true;
                if self.text_started {
                    out.push(ProviderStreamEvent::BlockStop { index: self.text_block_index });
                }
                let stop = if self.saw_tool {
                    StopReason::ToolUse
                } else {
                    finish_to_stop(Some(&reason))
                };
                self.stop_reason = Some(stop);
                out.push(ProviderStreamEvent::StopReason(stop));
                out.push(ProviderStreamEvent::MessageStop);
            }
        }

        Ok(out)
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
#[serde(rename_all = "camelCase")]
struct GenerateRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
}

#[derive(Serialize)]
struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPartOut>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum GeminiPartOut {
    Text {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    FunctionCall {
        function_call: GeminiFunctionCallOut,
    },
    #[serde(rename_all = "camelCase")]
    FunctionResponse {
        function_response: GeminiFunctionResponseOut,
    },
}

#[derive(Serialize)]
struct GeminiFunctionCallOut {
    name: String,
    args: JsonValue,
}

#[derive(Serialize)]
struct GeminiFunctionResponseOut {
    name: String,
    response: JsonValue,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiTool {
    function_declarations: Vec<GeminiFunctionDecl>,
}

#[derive(Serialize)]
struct GeminiFunctionDecl {
    name: String,
    description: String,
    parameters: JsonValue,
}

// Response

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiContentIn>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct GeminiContentIn {
    #[serde(default)]
    parts: Vec<GeminiPart>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    function_call: Option<GeminiFunctionCallIn>,
    #[serde(default)]
    #[allow(dead_code)]
    function_response: Option<JsonValue>,
}

#[derive(Deserialize)]
struct GeminiFunctionCallIn {
    name: String,
    #[serde(default)]
    args: Option<JsonValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsage {
    #[serde(default)]
    prompt_token_count: Option<u32>,
    #[serde(default)]
    candidates_token_count: Option<u32>,
}

#[derive(Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    #[serde(default)]
    status: Option<String>,
    message: String,
}
