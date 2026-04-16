---
title: Getting Started
description: Install and run your first agenty session.
---

## Prerequisites

- **Rust** 2024 edition (1.85+)
- An API key for at least one provider: Anthropic, OpenAI, or Google Gemini

## Installation

Clone the repository and build from source:

```bash
git clone https://github.com/anthropics/agenty.git
cd agenty
cargo build --release
```

The binary is produced at `target/release/agenty`.

## Setting up your API key

Agenty reads provider keys from environment variables. Set the one that matches your chosen provider:

```bash
# Anthropic (default)
export ANTHROPIC_API_KEY="sk-ant-..."

# OpenAI
export OPENAI_API_KEY="sk-..."

# Google Gemini
export GEMINI_API_KEY="..."
```

You can also place these in a `.env` file in the working directory. Agenty loads it automatically on startup via `dotenvy`.

## Running in headless mode

Pass a prompt with `-p` to run a single query and print the answer:

```bash
agenty -p "List every .rs file in the current directory"
```

The agent will plan, invoke tools (shell, file reads, etc.), and print its final response to stdout.

## Running in interactive mode

Launch without `-p` to enter the TUI:

```bash
agenty
```

You get a conversational interface where you can send messages, watch the agent think and call tools in real time, and continue the conversation across multiple turns.

## Choosing a provider and model

Use `--provider` and `--model` to override the defaults:

```bash
# Use OpenAI GPT-4o
agenty --provider openai --model gpt-4o -p "Summarize this repo"

# Use Gemini
agenty --provider gemini -p "What time is it in Tokyo?"
```

Default models per provider:

| Provider | Default model |
|---|---|
| Anthropic | `claude-haiku-4-5-20251001` |
| OpenAI | `gpt-4o-mini` |
| Gemini | `gemini-2.0-flash` |

## Enabling extended thinking

For complex reasoning tasks on Anthropic models, enable extended thinking with a token budget:

```bash
agenty --thinking 4096 -p "Explain the trade-offs of this architecture"
```

This flag is Anthropic-only and ignored for other providers.

## Rate limiting

If your API plan has request caps, use `--rpm` to throttle outgoing calls:

```bash
agenty --rpm 30 -p "Refactor the auth module"
```

The agent pauses automatically when it hits the limit and resumes when a slot opens.

## Next steps

- [Built-in Tools](/guides/tools/) to see what the agent can do out of the box
- [Plugins](/guides/plugins/) to extend the agent with your own tools
- [CLI Reference](/reference/cli/) for the full list of flags
