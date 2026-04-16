---
title: CLI Reference
description: Complete reference for the agenty command-line interface.
---

## Usage

```
agenty [OPTIONS]
```

When run without `--prompt`, agenty starts an interactive TUI session. With `--prompt`, it runs a single query and exits.

## Options

### `-p`, `--prompt <TEXT>`

Run a single prompt non-interactively and print the final answer to stdout.

```bash
agenty -p "Explain this codebase"
```

### `--provider <PROVIDER>`

LLM provider to route requests to.

| Value | Description |
|---|---|
| `anthropic` | Anthropic Claude (default) |
| `openai` | OpenAI GPT |
| `gemini` | Google Gemini |

```bash
agenty --provider openai -p "Hello"
```

### `-m`, `--model <MODEL>`

Model ID to use. If omitted, a default is chosen based on the provider:

| Provider | Default model |
|---|---|
| `anthropic` | `claude-haiku-4-5-20251001` |
| `openai` | `gpt-4o-mini` |
| `gemini` | `gemini-2.0-flash` |

```bash
agenty --provider anthropic -m claude-sonnet-4-20250514 -p "Refactor main.rs"
```

### `--max-tokens <N>`

Maximum number of tokens per provider response. Default: `1024`.

```bash
agenty --max-tokens 4096 -p "Write a detailed report"
```

### `-s`, `--system <TEXT>`

System prompt prepended to every request. Default: empty string.

```bash
agenty -s "You are a senior Rust engineer." -p "Review this function"
```

### `--thinking <BUDGET>`

Enable extended thinking with the given token budget. Anthropic-only; ignored for other providers.

```bash
agenty --thinking 4096 -p "Solve this optimization problem"
```

### `--plugins-dir <DIR>`

Directory to scan for plugins. Default: `~/.agenty/plugins`.

```bash
agenty --plugins-dir ./my-plugins -p "Use the weather tool"
```

### `--rpm <N>`

Rate limit: maximum requests per minute to the provider. Omit to disable rate limiting.

When the limit is reached, agenty pauses and prints a message to stderr:

```
rate limited - waiting for permit...
```

```bash
agenty --rpm 30 -p "Process these files"
```

## Environment variables

| Variable | Provider | Required when |
|---|---|---|
| `ANTHROPIC_API_KEY` | Anthropic | `--provider anthropic` (default) |
| `OPENAI_API_KEY` | OpenAI | `--provider openai` |
| `GEMINI_API_KEY` | Gemini | `--provider gemini` |

These can also be set in a `.env` file in the current working directory.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Error (missing API key, provider failure, etc.) |
