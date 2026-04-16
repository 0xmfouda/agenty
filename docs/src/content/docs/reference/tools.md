---
title: Tools Reference
description: Complete input/output reference for all built-in tools.
---

## Tool trait

Every tool implements this trait:

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> JsonValue;
    fn execute(&self, input: JsonValue) -> Result<JsonValue, String>;
}
```

The `invoke` helper wraps a `ToolCall` into a `ToolResult`:

```rust
pub fn invoke(tool: &dyn Tool, call: &ToolCall) -> ToolResult;
```

---

## bash

**Name:** `bash`

**Description:** Run a shell command inside a sandboxed environment and return its stdout, stderr, and exit code.

### Input schema

| Field | Type | Required | Description |
|---|---|---|---|
| `command` | string | Yes | Shell command to execute. |

### Output

| Field | Type | Description |
|---|---|---|
| `stdout` | string | Standard output of the command. |
| `stderr` | string | Standard error of the command. |
| `exit_code` | integer or null | Process exit code. Null if the process was killed by a signal. |

### Errors

- `"invalid input: ..."` if the JSON input is malformed.
- `"failed to spawn sandboxed command: ..."` if the process could not start.
- `"command timed out after N seconds"` if the timeout was exceeded.
- `"sandboxed execution is only supported on Linux"` on non-Linux platforms.

---

## read_file

**Name:** `read_file`

**Description:** Read a UTF-8 text file from the local filesystem and return its contents.

### Input schema

| Field | Type | Required | Description |
|---|---|---|---|
| `path` | string | Yes | Path to the file to read. |

### Output

| Field | Type | Description |
|---|---|---|
| `path` | string | The path that was read. |
| `contents` | string | UTF-8 contents of the file. |

### Errors

- `"invalid input: ..."` if the JSON input is malformed.
- File I/O errors (not found, permission denied, non-UTF-8).

---

## write_file

**Name:** `write_file`

**Description:** Write UTF-8 contents to a file, creating parent directories if they do not exist. Overwrites any existing file.

### Input schema

| Field | Type | Required | Description |
|---|---|---|---|
| `path` | string | Yes | Destination path. |
| `contents` | string | Yes | UTF-8 contents to write. |

### Output

| Field | Type | Description |
|---|---|---|
| `path` | string | The path that was written. |
| `bytes_written` | integer | Number of bytes written. |

### Errors

- `"invalid input: ..."` if the JSON input is malformed.
- File I/O errors (permission denied, disk full).

---

## list_files

**Name:** `list_files`

**Description:** List the immediate entries of a directory.

### Input schema

| Field | Type | Required | Description |
|---|---|---|---|
| `path` | string | Yes | Directory to list. |

### Output

| Field | Type | Description |
|---|---|---|
| `path` | string | The directory that was listed. |
| `entries` | array | Array of entry objects. |
| `entries[].name` | string | File or directory name. |
| `entries[].path` | string | Full path to the entry. |
| `entries[].is_dir` | boolean | Whether the entry is a directory. |

### Errors

- `"invalid input: ..."` if the JSON input is malformed.
- Directory I/O errors (not found, not a directory, permission denied).

---

## web_search

**Name:** `web_search`

**Description:** Search the web using DuckDuckGo and return the top results.

### Input schema

| Field | Type | Required | Description |
|---|---|---|---|
| `query` | string | Yes | The search query. |
| `count` | integer | No | Maximum number of results to return. Default: 5. |

### Output

| Field | Type | Description |
|---|---|---|
| `results` | array | Array of search result objects. |
| `results[].title` | string | Page title. |
| `results[].url` | string | Page URL. |
| `results[].snippet` | string | Text snippet from the result. |

### Errors

- `"invalid input: ..."` if the JSON input is malformed.
- HTTP or parsing errors from DuckDuckGo.

---

## memory

**Name:** `memory`

**Description:** Persist and recall information across conversations.

### Input schema

| Field | Type | Required | Description |
|---|---|---|---|
| `action` | string | Yes | One of `save`, `search`, `list`, `delete`. |
| `summary` | string | For `save` | One-line summary for the memory index. |
| `content` | string | For `save` | Full memory content. |
| `tags` | string[] | No | Optional tags for categorization (used with `save`). |
| `query` | string | For `search` | Search query string. |
| `id` | string | For `delete` | Memory ID to delete. |

### Output by action

**save:**

| Field | Type | Description |
|---|---|---|
| `status` | string | `"saved"` |
| `id` | string | Assigned memory ID. |

**search:**

| Field | Type | Description |
|---|---|---|
| `results` | array | Matching memories. |
| `count` | integer | Number of results. |

**list:**

| Field | Type | Description |
|---|---|---|
| `memories` | array | All stored memories (newest first). |
| `count` | integer | Total count. |

**delete:**

| Field | Type | Description |
|---|---|---|
| `status` | string | `"deleted"` |
| `id` | string | The deleted memory ID. |

### Errors

- `"invalid input: ..."` if the JSON input is malformed.
- `"missing field: ..."` if a required field for the action is absent.
- Memory store I/O errors.
