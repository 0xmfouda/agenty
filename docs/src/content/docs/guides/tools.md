---
title: Built-in Tools
description: The tools available to the agent out of the box.
---

Agenty ships with six built-in tools. The LLM decides when to call them based on the conversation and the task at hand. You do not need to enable them manually; they are always registered.

## bash

Runs a shell command via `sh -c` and returns stdout, stderr, and the exit code.

On Linux, every invocation runs inside a sandbox that restricts filesystem access, blocks networking, enforces resource limits, and kills the process after a timeout. See [Sandbox and Security](/guides/sandbox/) for details.

On non-Linux platforms, this tool returns an error because sandboxed execution is not available.

```json
{
  "command": "find . -name '*.rs' | head -20"
}
```

Response:

```json
{
  "stdout": "./src/main.rs\n./src/lib.rs\n",
  "stderr": "",
  "exit_code": 0
}
```

## read_file

Reads a UTF-8 text file and returns its contents.

```json
{
  "path": "src/main.rs"
}
```

Response:

```json
{
  "path": "src/main.rs",
  "contents": "fn main() { ... }"
}
```

## write_file

Writes UTF-8 contents to a file, creating parent directories if needed. Overwrites any existing file at the path.

```json
{
  "path": "out/report.txt",
  "contents": "All tests passed."
}
```

Response:

```json
{
  "path": "out/report.txt",
  "bytes_written": 17
}
```

## list_files

Lists the immediate entries of a directory. Each entry includes its name, full path, and whether it is a directory.

```json
{
  "path": "src"
}
```

Response:

```json
{
  "path": "src",
  "entries": [
    { "name": "main.rs", "path": "src/main.rs", "is_dir": false },
    { "name": "lib.rs", "path": "src/lib.rs", "is_dir": false },
    { "name": "utils", "path": "src/utils", "is_dir": true }
  ]
}
```

## web_search

Searches the web via DuckDuckGo and returns the top results with title, URL, and snippet.

```json
{
  "query": "Rust landlock tutorial",
  "count": 3
}
```

Response:

```json
{
  "results": [
    {
      "title": "Landlock in Rust",
      "url": "https://example.com/landlock",
      "snippet": "A practical guide to..."
    }
  ]
}
```

The `count` parameter is optional and defaults to 5.

## memory

Persists and recalls information across conversations. The store lives at `~/.agenty/memory/`.

### Save

```json
{
  "action": "save",
  "summary": "User prefers concise answers",
  "content": "The user asked to keep responses short and avoid repeating context.",
  "tags": ["preference"]
}
```

### Search

```json
{
  "action": "search",
  "query": "preference"
}
```

### List

```json
{
  "action": "list"
}
```

### Delete

```json
{
  "action": "delete",
  "id": "a1b2c3d4"
}
```

## The Tool trait

All tools implement the same trait:

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> JsonValue;
    fn execute(&self, input: JsonValue) -> Result<JsonValue, String>;
}
```

The `input_schema` method returns a JSON Schema object that the LLM uses to understand what parameters the tool accepts. The `execute` method receives the validated input and returns either a JSON success payload or a plain-text error.

This is also the trait you implement when writing a Rust-native plugin tool.
