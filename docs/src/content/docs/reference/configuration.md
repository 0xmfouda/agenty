---
title: Configuration Reference
description: Reference for agenty's runtime configuration and core types.
---

## Config

The `Config` struct (from `agenty-core`) controls how the agent communicates with the LLM provider.

```rust
pub struct Config {
    pub model: String,
    pub provider: Provider,
    pub max_tokens: u32,
    pub system_prompt: String,
    pub thinking_budget: Option<u32>,
}
```

| Field | Type | Description |
|---|---|---|
| `model` | `String` | Model ID sent to the provider. |
| `provider` | `Provider` | Which LLM backend to use. |
| `max_tokens` | `u32` | Maximum tokens per response. |
| `system_prompt` | `String` | System message prepended to each request. |
| `thinking_budget` | `Option<u32>` | Token budget for extended thinking (Anthropic only). |

All fields are set from CLI flags. There is no config file.

## Provider

```rust
pub enum Provider {
    Anthropic,
    OpenAI,
    Gemini,
}
```

## Core message types

### Role

```rust
pub enum Role {
    User,
    Assistant,
    Tool,
}
```

### ChatMessage

A message in the conversation history. Contains one or more content blocks.

```rust
pub struct ChatMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}
```

### ContentBlock

```rust
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: JsonValue,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}
```

### ToolCall

Extracted from a `ToolUse` content block for tool dispatch.

```rust
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: JsonValue,
}
```

### ToolResult

Returned after executing a tool.

```rust
pub struct ToolResult {
    pub call_id: String,
    pub output: ToolOutput,
}

pub enum ToolOutput {
    Success(JsonValue),
    Error(String),
}
```

### ToolSpec

Describes a tool's interface for the provider API.

```rust
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: JsonValue,
}
```

## StopReason

Indicates why the LLM stopped generating.

```rust
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Other(String),
}
```

| Variant | Meaning |
|---|---|
| `EndTurn` | The model finished its response. |
| `ToolUse` | The model wants to call one or more tools. |
| `MaxTokens` | The response hit the `max_tokens` limit. |
| `StopSequence` | A stop sequence was encountered. |
| `Other` | Provider-specific reason. |

## AgentError

```rust
pub enum AgentError {
    Provider(String),
    Tool(String),
    Config(String),
    Session(String),
    Other(String),
}
```

| Variant | When |
|---|---|
| `Provider` | LLM API call failed (network, auth, rate limit). |
| `Tool` | A tool execution returned an unrecoverable error. |
| `Config` | Invalid configuration (missing API key, bad model ID). |
| `Session` | Session persistence failed. |
| `Other` | Anything else. |

## REPL options

```rust
pub struct ReplOptions {
    pub max_turns: usize,
}
```

| Field | Type | Default | Description |
|---|---|---|---|
| `max_turns` | `usize` | `20` | Safety cap on how many provider round-trips a single `run()` call may take. If the agent exceeds this, it returns an error. |

## Default paths

| Path | Purpose |
|---|---|
| `~/.agenty/memory/` | Persistent memory store |
| `~/.agenty/memory/MEMORY.md` | Memory index (summaries) |
| `~/.agenty/memory/entries/` | Individual memory JSON files |
| `~/.agenty/plugins/` | Plugin discovery directory |
| `.env` (working directory) | Environment variable overrides |
