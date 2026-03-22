use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use serde_json::Value as JsonValue;

// ---------------------------------------------------------------------------
// Role
// ---------------------------------------------------------------------------

/// Who authored a message in the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The human user driving the session.
    User,
    /// The LLM assistant.
    Assistant,
    /// A tool returning results back to the model.
    Tool,
}

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// A single turn in the conversation history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self { role, content: content.into() }
    }
}

// ---------------------------------------------------------------------------
// ToolCall
// ---------------------------------------------------------------------------

/// A request made by the assistant to invoke a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique call ID so the matching `ToolResult` can be correlated.
    pub id: String,
    /// Name of the tool to invoke.
    pub name: String,
    /// Arbitrary JSON arguments passed to the tool.
    pub input: JsonValue,
}

// ---------------------------------------------------------------------------
// ToolResult
// ---------------------------------------------------------------------------

/// The response returned by a tool execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Matches the `id` of the originating `ToolCall`.
    pub call_id: String,
    /// Either the JSON success payload or a plain-text error message.
    pub output: ToolOutput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "type", content = "value")]
pub enum ToolOutput {
    Success(JsonValue),
    Error(String),
}

impl ToolResult {
    pub fn success(call_id: impl Into<String>, value: JsonValue) -> Self {
        Self { call_id: call_id.into(), output: ToolOutput::Success(value) }
    }

    pub fn error(call_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self { call_id: call_id.into(), output: ToolOutput::Error(message.into()) }
    }

    pub fn is_success(&self) -> bool {
        matches!(self.output, ToolOutput::Success(_))
    }
}

// ---------------------------------------------------------------------------
// ContentBlock / ChatMessage
// ---------------------------------------------------------------------------

/// A single block inside a tool-aware chat message.
///
/// Field names and the `type` tag are chosen to match Anthropic's Messages API
/// wire format so the same struct round-trips in both directions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    /// Extended-thinking output. `signature` must be echoed back in follow-up
    /// assistant turns when tool use is involved, per Anthropic's API.
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        signature: String,
    },
    ToolUse { id: String, name: String, input: JsonValue },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

/// A tool-aware conversation message: role + a sequence of content blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl ChatMessage {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: vec![ContentBlock::Text { text: text.into() }] }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self { role: Role::Assistant, content }
    }

    pub fn user(content: Vec<ContentBlock>) -> Self {
        Self { role: Role::User, content }
    }

    /// Concatenate all `Text` blocks, ignoring tool-use/tool-result blocks.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// A tool declaration sent to the provider so the model can call it.
///
/// Matches Anthropic's `tools[]` entry shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: JsonValue,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    #[serde(other)]
    Other,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// LLM provider the agent routes requests to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    OpenAI,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Runtime configuration for the agent.
///
/// API keys are intentionally absent; each provider crate reads them from
/// environment variables at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Model identifier string (e.g. `"claude-sonnet-4-6"`).
    pub model: String,
    /// Which provider to route requests to.
    pub provider: Provider,
    /// Maximum number of tokens the model may generate per response.
    pub max_tokens: u32,
    /// System prompt prepended to every conversation.
    pub system_prompt: String,
    /// Extended-thinking budget in tokens. `None` disables thinking.
    #[serde(default)]
    pub thinking_budget: Option<u32>,
}

// ---------------------------------------------------------------------------
// AgentError
// ---------------------------------------------------------------------------

/// Top-level error enum for the workspace.
#[derive(Debug, Error)]
pub enum AgentError {
    /// Provider or API-level failure (network, rate-limit, invalid response, …).
    #[error("provider error: {0}")]
    Provider(String),

    /// A tool failed during execution.
    #[error("tool `{tool}` failed: {reason}")]
    Tool { tool: String, reason: String },

    /// Configuration is missing or malformed.
    #[error("config error: {0}")]
    Config(String),

    /// Session persistence / I/O error.
    #[error("session error: {0}")]
    Session(String),

    /// Catch-all for anything else.
    #[error("{0}")]
    Other(String),
}
