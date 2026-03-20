//! Query loop that drives a provider through tool-call / tool-result turns
//! until the model stops calling tools, plus slash-command dispatch on top.

mod commands;

pub use commands::{Command, ReplOutcome, ReplSession, parse_command};

use agenty_providers::anthropic::AnthropicClient;
use agenty_tools::Tool;
use agenty_types::{
    AgentError, ChatMessage, Config, ContentBlock, StopReason, ToolSpec,
};

/// Incremental progress from a streaming turn. TUIs and other consumers
/// assemble these into live UI updates.
#[derive(Debug, Clone)]
pub enum StreamDelta {
    TextDelta { index: u32, text: String },
    ThinkingDelta { index: u32, text: String },
    ToolUseStart { index: u32, id: String, name: String },
    ToolInputDelta { index: u32, partial_json: String },
    BlockStop { index: u32 },
    MessageComplete { content: Vec<ContentBlock>, stop_reason: StopReason },
    Error(String),
}

/// Behavioural options for the query loop.
#[derive(Debug, Clone, Copy)]
pub struct ReplOptions {
    /// Safety cap on how many provider turns a single `run()` may take.
    pub max_turns: usize,
}

impl Default for ReplOptions {
    fn default() -> Self {
        Self { max_turns: 20 }
    }
}

/// Orchestrates a single user-prompt → final-answer query, running any tool
/// calls the model requests in between.
pub struct Repl<'a> {
    client: &'a AnthropicClient,
    config: &'a Config,
    tools: Vec<&'a dyn Tool>,
    options: ReplOptions,
}

impl<'a> Repl<'a> {
    pub fn new(
        client: &'a AnthropicClient,
        config: &'a Config,
        tools: Vec<&'a dyn Tool>,
    ) -> Self {
        Self { client, config, tools, options: ReplOptions::default() }
    }

    pub fn with_options(mut self, options: ReplOptions) -> Self {
        self.options = options;
        self
    }

    /// Run the loop for `prompt` starting from an empty conversation.
    pub async fn run(&self, prompt: &str) -> Result<Vec<ChatMessage>, AgentError> {
        let mut conversation = Vec::new();
        self.run_turn(&mut conversation, prompt).await?;
        Ok(conversation)
    }

    /// Append a user prompt to `conversation`, then loop through tool calls,
    /// appending everything to the same conversation, until the model stops
    /// calling tools.
    pub async fn run_turn(
        &self,
        conversation: &mut Vec<ChatMessage>,
        prompt: &str,
    ) -> Result<(), AgentError> {
        conversation.push(ChatMessage::user_text(prompt));
        let specs = self.tool_specs();

        for _ in 0..self.options.max_turns {
            let response = self
                .client
                .send_with_tools(self.config, conversation, &specs)
                .await?;

            conversation.push(ChatMessage::assistant(response.content.clone()));

            if response.stop_reason != StopReason::ToolUse {
                return Ok(());
            }

            let tool_results = self.run_tool_calls(&response.content);
            if tool_results.is_empty() {
                // Model said tool_use but produced no tool_use blocks — bail
                // rather than loop forever.
                return Ok(());
            }
            conversation.push(ChatMessage::user(tool_results));
        }

        Err(AgentError::Other(format!(
            "repl exceeded max_turns = {}",
            self.options.max_turns
        )))
    }

    pub fn client(&self) -> &'a AnthropicClient {
        self.client
    }

    pub fn config(&self) -> &Config {
        self.config
    }

    /// Append a user-text message to `conversation`.
    pub fn add_user_message(&self, conversation: &mut Vec<ChatMessage>, text: &str) {
        conversation.push(ChatMessage::user_text(text));
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    pub fn run_tool_calls(&self, blocks: &[ContentBlock]) -> Vec<ContentBlock> {
        blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => {
                    let (content, is_error) = match self.find_tool(name) {
                        Some(tool) => match tool.execute(input.clone()) {
                            Ok(value) => (stringify_output(&value), false),
                            Err(err) => (err, true),
                        },
                        None => (format!("unknown tool: {name}"), true),
                    };
                    Some(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content,
                        is_error,
                    })
                }
                _ => None,
            })
            .collect()
    }

    fn find_tool(&self, name: &str) -> Option<&&'a dyn Tool> {
        self.tools.iter().find(|t| t.name() == name)
    }
}

fn stringify_output(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
