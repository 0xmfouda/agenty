---
title: Plugins
description: Extend the agent with custom script-based tools.
---

Agenty discovers and loads plugins from a directory at startup. Each plugin is a folder containing a `plugin.toml` manifest and one or more scripts. The agent sees plugin tools alongside built-in tools and can call them the same way.

## Plugin directory

By default, agenty looks for plugins in `~/.agenty/plugins/`. Override this with the `--plugins-dir` flag:

```bash
agenty --plugins-dir ./my-plugins -p "Use my custom tool"
```

Each subdirectory that contains a `plugin.toml` is loaded as a separate plugin.

```
~/.agenty/plugins/
  weather/
    plugin.toml
    weather.py
  jira/
    plugin.toml
    query.sh
```

## Writing a plugin manifest

A `plugin.toml` file declares the plugin name, description, and one or more tools:

```toml
name = "weather"
description = "Fetch current weather for a location"

[[tools]]
name = "get_weather"
description = "Get the current weather for a city"
command = "python3 weather.py"

[tools.input_schema]
type = "object"
required = ["city"]

[tools.input_schema.properties.city]
type = "string"
description = "City name, e.g. 'Berlin'"
```

### Fields

| Field | Required | Description |
|---|---|---|
| `name` | Yes | Plugin name, used for logging. |
| `description` | Yes | Human-readable description. |
| `[[tools]]` | Yes | One or more tool definitions. |
| `tools.name` | Yes | Tool name the LLM will call. |
| `tools.description` | Yes | Description shown to the LLM. |
| `tools.command` | Yes | Shell command to execute. |
| `tools.input_schema` | Yes | JSON Schema object describing the tool's input. |

## How tool execution works

When the LLM calls a plugin tool:

1. Agenty serializes the tool input as JSON.
2. The `command` is spawned via `sh -c` (or `cmd /C` on Windows) with the working directory set to the plugin folder.
3. The JSON input is written to the child's **stdin**, then stdin is closed.
4. Agenty waits for the process to exit.
5. If the exit code is 0, **stdout** is returned to the LLM as the tool result.
6. If the exit code is non-zero, **stderr** is returned as an error.

## Example: a Python plugin

`~/.agenty/plugins/greeting/plugin.toml`:

```toml
name = "greeting"
description = "Generates personalized greetings"

[[tools]]
name = "greet"
description = "Greet someone by name"
command = "python3 greet.py"

[tools.input_schema]
type = "object"
required = ["name"]

[tools.input_schema.properties.name]
type = "string"
description = "The person to greet"
```

`~/.agenty/plugins/greeting/greet.py`:

```python
import json
import sys

data = json.load(sys.stdin)
name = data["name"]
print(json.dumps({"message": f"Hello, {name}!"}))
```

Run:

```bash
agenty -p "Greet Alice"
```

The agent will discover the `greet` tool, call it with `{"name": "Alice"}`, and receive `{"message": "Hello, Alice!"}`.

## Example: a shell plugin

`~/.agenty/plugins/disk/plugin.toml`:

```toml
name = "disk"
description = "Disk usage utilities"

[[tools]]
name = "disk_usage"
description = "Show disk usage for a path"
command = "bash disk.sh"

[tools.input_schema]
type = "object"
required = ["path"]

[tools.input_schema.properties.path]
type = "string"
description = "Filesystem path to check"
```

`~/.agenty/plugins/disk/disk.sh`:

```bash
#!/usr/bin/env bash
path=$(jq -r '.path' <&0)
du -sh "$path" 2>&1 | jq -Rs '{output: .}'
```

## Multiple tools per plugin

A single plugin can expose several tools:

```toml
name = "devops"
description = "DevOps helper tools"

[[tools]]
name = "container_list"
description = "List running containers"
command = "docker ps --format json"

[tools.input_schema]
type = "object"
properties = {}

[[tools]]
name = "container_logs"
description = "Fetch logs for a container"
command = "bash logs.sh"

[tools.input_schema]
type = "object"
required = ["container_id"]

[tools.input_schema.properties.container_id]
type = "string"
description = "Container ID or name"
```

## Plugin discovery at startup

When agenty starts, it:

1. Scans the plugins directory for subdirectories containing `plugin.toml`.
2. Parses each manifest with `ScriptPlugin::load()`.
3. Registers all plugin tools in the `PluginRegistry`.
4. Flattens plugin tools into the same tool list as built-in tools.
5. Prints the count of loaded plugins to stderr (e.g., `loaded 2 plugin(s) from ~/.agenty/plugins`).

If a manifest fails to parse, agenty prints a warning and continues loading other plugins.

## Security note

Plugin commands run without sandboxing. They execute with the same permissions as the agenty process. If you are running untrusted plugins, consider running the entire agent inside a container.
