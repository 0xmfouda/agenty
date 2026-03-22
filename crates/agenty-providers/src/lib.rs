//! Provider abstraction over chat/completions backends.
//!
//! Concrete backends (Anthropic, OpenAI) implement the same surface exposed by
//! [`ChatClient`]: a non-streaming tool-aware request and a streaming
//! tool-aware request that yields [`ProviderStreamEvent`]s.

#[cfg(feature = "anthropic")]
pub mod anthropic;
#[cfg(feature = "gemini")]
pub mod gemini;
#[cfg(feature = "openai")]
pub mod openai;

use std::pin::Pin;

use futures::Stream;

pub use agenty_core::{AgentError, ChatMessage, Config, ContentBlock, Message, StopReason, ToolSpec};

/// A token yielded by a streaming completion.
pub type Token = String;

/// A pinned, heap-allocated token stream.
pub type TokenStream<'a> =
    Pin<Box<dyn Stream<Item = Result<Token, AgentError>> + Send + 'a>>;

// ---------------------------------------------------------------------------
// Unified response / event types
// ---------------------------------------------------------------------------

/// Full non-streaming response from a tool-aware request.
#[derive(Debug, Clone)]
pub struct AssistantResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
}

/// Kind of content block announced by a `BlockStart` event.
#[derive(Debug, Clone)]
pub enum BlockKind {
    Text,
    ToolUse { id: String, name: String },
    Thinking,
}

/// A single decoded event from a streaming provider response.
///
/// The shape is modeled on Anthropic's SSE events; OpenAI's chat-completions
/// SSE deltas are translated into the same shape so consumers (TUI, REPL) can
/// treat both backends identically.
#[derive(Debug, Clone)]
pub enum ProviderStreamEvent {
    BlockStart { index: u32, kind: BlockKind },
    TextDelta { index: u32, text: String },
    ThinkingDelta { index: u32, text: String },
    /// Anthropic-specific: signature that authenticates a thinking block.
    /// Must be echoed back verbatim in follow-up turns when tool use is
    /// involved. OpenAI does not emit this.
    SignatureDelta { index: u32, signature: String },
    ToolInputDelta { index: u32, partial_json: String },
    BlockStop { index: u32 },
    StopReason(StopReason),
    Usage { input_tokens: u32, output_tokens: u32 },
    MessageStop,
}

pub type ProviderEventStream =
    Pin<Box<dyn Stream<Item = Result<ProviderStreamEvent, AgentError>> + Send>>;

// ---------------------------------------------------------------------------
// ChatClient — enum dispatch across backends
// ---------------------------------------------------------------------------

/// Unified chat client that dispatches to a concrete provider backend.
pub enum ChatClient {
    #[cfg(feature = "anthropic")]
    Anthropic(anthropic::AnthropicClient),
    #[cfg(feature = "openai")]
    OpenAI(openai::OpenAIClient),
    #[cfg(feature = "gemini")]
    Gemini(gemini::GeminiClient),
}

impl ChatClient {
    /// Send a tool-aware request and return the full assistant response.
    pub async fn send_with_tools(
        &self,
        config: &Config,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantResponse, AgentError> {
        match self {
            #[cfg(feature = "anthropic")]
            ChatClient::Anthropic(c) => c.send_with_tools(config, messages, tools).await,
            #[cfg(feature = "openai")]
            ChatClient::OpenAI(c) => c.send_with_tools(config, messages, tools).await,
            #[cfg(feature = "gemini")]
            ChatClient::Gemini(c) => c.send_with_tools(config, messages, tools).await,
        }
    }

    /// Send a tool-aware streaming request.
    pub async fn stream_with_tools(
        &self,
        config: &Config,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<ProviderEventStream, AgentError> {
        match self {
            #[cfg(feature = "anthropic")]
            ChatClient::Anthropic(c) => c.stream_with_tools(config, messages, tools).await,
            #[cfg(feature = "openai")]
            ChatClient::OpenAI(c) => c.stream_with_tools(config, messages, tools).await,
            #[cfg(feature = "gemini")]
            ChatClient::Gemini(c) => c.stream_with_tools(config, messages, tools).await,
        }
    }
}

/// Legacy trait kept for the simple non-tool streaming path used by any
/// caller that wants raw token streams rather than the richer
/// [`ProviderStreamEvent`] interface.
pub trait Provider: Send + Sync {
    fn complete<'a>(&'a self, messages: &'a [Message]) -> TokenStream<'a>;
}
