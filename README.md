# Agenty

<img src="./assets/agenty.png">

## CLI

Requires an API key for the provider you use. Set it in your shell environment, or drop a `.env` file in the directory you run `agenty` from:

```
ANTHROPIC_API_KEY=sk-ant-...
OPENAI_API_KEY=sk-...
GEMINI_API_KEY=...
```

```bash
# Interactive TUI (default provider: anthropic, default model: claude-sonnet-4-6)
cargo run

# One-shot headless prompt
cargo run -- -p "list the files in the current dir"

# Switch providers
cargo run -- --provider openai                  # uses gpt-4o-mini by default
cargo run -- --provider openai -m gpt-4o
cargo run -- --provider gemini                  # uses gemini-2.0-flash by default
cargo run -- --provider gemini -m gemini-1.5-pro

# Common flags
cargo run -- -m claude-haiku-4-5-20251001       # pick a model
cargo run -- --thinking 4096                    # enable extended thinking (Anthropic only)
cargo run -- -s "You are terse." -p "hello"     # system prompt
cargo run -- --max-tokens 2048
```

In the TUI: `Enter` to send · `PageUp`/`PageDown`, `Shift+↑/↓`, mouse wheel to scroll · `End` jumps to bottom · `/clear`, `/exit` · `Ctrl+C` to cancel.
