---
title: Plugin Manifest Reference
description: Complete reference for the plugin.toml manifest format.
---

## File location

Each plugin lives in its own directory under the plugins root (default: `~/.agenty/plugins/`). The manifest must be named `plugin.toml`.

```
~/.agenty/plugins/
  my-plugin/
    plugin.toml      # required
    script.py        # referenced by command field
```

## Full schema

```toml
name = "my-plugin"
description = "What this plugin does"

[[tools]]
name = "tool_name"
description = "What this tool does"
command = "python3 script.py"

[tools.input_schema]
type = "object"
required = ["param1"]

[tools.input_schema.properties.param1]
type = "string"
description = "Description of param1"

[tools.input_schema.properties.param2]
type = "integer"
description = "Optional numeric parameter"
```

## Top-level fields

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | Yes | Plugin name. Used in log messages. |
| `description` | string | Yes | Human-readable description of the plugin. |

## Tool definition (`[[tools]]`)

Each `[[tools]]` entry defines one tool the agent can call.

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | Yes | Tool name. This is what the LLM uses to call the tool. Must be unique across all tools (built-in and plugin). |
| `description` | string | Yes | Description shown to the LLM. Should explain what the tool does and when to use it. |
| `command` | string | Yes | Shell command to execute. Runs via `sh -c` on Unix or `cmd /C` on Windows. |
| `input_schema` | object | Yes | JSON Schema describing the tool's input. Must have `type = "object"`. |

## Input schema

The `input_schema` follows [JSON Schema](https://json-schema.org/) conventions. It must be an object type. Common patterns:

### Required string parameter

```toml
[tools.input_schema]
type = "object"
required = ["query"]

[tools.input_schema.properties.query]
type = "string"
description = "The search query"
```

### Optional parameter with default

```toml
[tools.input_schema]
type = "object"
required = ["path"]

[tools.input_schema.properties.path]
type = "string"
description = "File path to process"

[tools.input_schema.properties.verbose]
type = "boolean"
description = "Enable verbose output (default: false)"
```

### Array parameter

```toml
[tools.input_schema]
type = "object"
required = ["files"]

[tools.input_schema.properties.files]
type = "array"
description = "List of file paths"

[tools.input_schema.properties.files.items]
type = "string"
```

### No parameters

```toml
[tools.input_schema]
type = "object"
properties = {}
```

## Execution model

1. The `command` is spawned with the working directory set to the plugin folder (the directory containing `plugin.toml`).
2. The tool input is serialized as JSON and written to the child's **stdin**.
3. Stdin is closed after writing.
4. The child runs to completion.
5. **Exit code 0**: stdout is returned to the LLM as the tool result (treated as a JSON string).
6. **Non-zero exit code**: stderr is returned as a tool error.

## Multiple tools

A single manifest can define any number of tools:

```toml
name = "devops"
description = "DevOps utilities"

[[tools]]
name = "list_pods"
description = "List Kubernetes pods"
command = "kubectl get pods -o json"

[tools.input_schema]
type = "object"
properties = {}

[[tools]]
name = "pod_logs"
description = "Fetch logs for a pod"
command = "bash get_logs.sh"

[tools.input_schema]
type = "object"
required = ["pod"]

[tools.input_schema.properties.pod]
type = "string"
description = "Pod name"
```

## Discovery

At startup, `PluginRegistry::discover()` walks the plugins directory and calls `ScriptPlugin::load()` for each `plugin.toml` found. Manifests that fail to parse produce a warning on stderr and are skipped. The count of successfully loaded plugins is printed:

```
loaded 2 plugin(s) from /home/user/.agenty/plugins
```
