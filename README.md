# Agenty

<img src="./assets/agenty.png">

## CLI

Requires `ANTHROPIC_API_KEY` in your environment.

```bash
# Interactive TUI (default model: claude-sonnet-4-6)
cargo run

# One-shot headless prompt
cargo run -- -p "list the files in the current dir"

# Common flags
cargo run -- -m claude-haiku-4-5-20251001       # pick a model
cargo run -- --thinking 4096                    # enable extended thinking
cargo run -- -s "You are terse." -p "hello"     # system prompt
cargo run -- --max-tokens 2048
```

In the TUI: `Enter` to send · `PageUp`/`PageDown`, `Shift+↑/↓`, mouse wheel to scroll · `End` jumps to bottom · `/clear`, `/exit` · `Ctrl+C` to cancel.
