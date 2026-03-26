//! Manifest-driven plugins that wrap shell commands as tools.
//!
//! A **script plugin** lives in a directory containing a `plugin.toml`:
//!
//! ```toml
//! name = "my-plugin"
//! description = "Does useful things"
//!
//! [[tools]]
//! name = "greet"
//! description = "Greet someone by name"
//! command = "python3 greet.py"
//!
//! [tools.input_schema]
//! type = "object"
//! properties.name.type = "string"
//! properties.name.description = "Who to greet"
//! required = ["name"]
//! ```
//!
//! When the agent invokes a script tool the plugin:
//!
//! 1. Spawns `command` with its working directory set to the plugin directory.
//! 2. Writes the JSON input object to the child's **stdin**, then closes it.
//! 3. Reads **stdout** as the tool result (returned as a JSON string value).
//! 4. If the process exits non-zero, the tool returns an error containing stderr.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use agenty_core::JsonValue;
use agenty_tools::Tool;
use serde::Deserialize;

use crate::Plugin;

// ── Manifest (TOML) ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Manifest {
    name: String,
    description: String,
    #[serde(default)]
    tools: Vec<ToolEntry>,
}

#[derive(Debug, Deserialize)]
struct ToolEntry {
    name: String,
    description: String,
    command: String,
    #[serde(default = "default_input_schema")]
    input_schema: toml::Value,
}

fn default_input_schema() -> toml::Value {
    toml::Value::Table({
        let mut m = toml::map::Map::new();
        m.insert("type".into(), toml::Value::String("object".into()));
        m
    })
}

// ── ScriptTool ─────────────────────────────────────────────────────────

/// A single tool backed by a shell command.
pub struct ScriptTool {
    tool_name: String,
    tool_description: String,
    command: String,
    schema: JsonValue,
    /// Working directory for the spawned process (the plugin directory).
    workdir: PathBuf,
}

impl Tool for ScriptTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn input_schema(&self) -> JsonValue {
        self.schema.clone()
    }

    fn execute(&self, input: JsonValue) -> Result<JsonValue, String> {
        let input_bytes =
            serde_json::to_vec(&input).map_err(|e| format!("failed to serialize input: {e}"))?;

        let mut child = Command::new(shell_program())
            .args(shell_args(&self.command))
            .current_dir(&self.workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn `{}`: {e}", self.command))?;

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&input_bytes);
        }

        let output = child
            .wait_with_output()
            .map_err(|e| format!("failed to wait on `{}`: {e}", self.command))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let code = output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".into());
            return Err(format!(
                "command `{}` exited with code {code}: {stderr}",
                self.command
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(JsonValue::String(stdout))
    }
}

#[cfg(windows)]
fn shell_program() -> &'static str {
    "cmd"
}

#[cfg(windows)]
fn shell_args(command: &str) -> Vec<String> {
    vec!["/C".into(), command.into()]
}

#[cfg(not(windows))]
fn shell_program() -> &'static str {
    "sh"
}

#[cfg(not(windows))]
fn shell_args(command: &str) -> Vec<String> {
    vec!["-c".into(), command.into()]
}

// ── ScriptPlugin ───────────────────────────────────────────────────────

/// A plugin loaded from a `plugin.toml` manifest.
pub struct ScriptPlugin {
    plugin_name: String,
    plugin_description: String,
    tools: Vec<ScriptTool>,
}

impl ScriptPlugin {
    /// Parse a `plugin.toml` at `manifest_path` and build the plugin.
    ///
    /// Commands in the manifest are resolved relative to the directory
    /// containing `plugin.toml`.
    pub fn load(manifest_path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(manifest_path)
            .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;

        let manifest: Manifest = toml::from_str(&text)
            .map_err(|e| format!("invalid manifest {}: {e}", manifest_path.display()))?;

        let workdir = manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();

        let tools = manifest
            .tools
            .into_iter()
            .map(|entry| {
                let schema = toml_to_json(&entry.input_schema);
                ScriptTool {
                    tool_name: entry.name,
                    tool_description: entry.description,
                    command: entry.command,
                    schema,
                    workdir: workdir.clone(),
                }
            })
            .collect();

        Ok(Self {
            plugin_name: manifest.name,
            plugin_description: manifest.description,
            tools,
        })
    }
}

impl Plugin for ScriptPlugin {
    fn name(&self) -> &str {
        &self.plugin_name
    }

    fn description(&self) -> &str {
        &self.plugin_description
    }

    fn tools(&self) -> Vec<&dyn Tool> {
        self.tools.iter().map(|t| t as &dyn Tool).collect()
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Convert a TOML value to a serde_json value (for input schemas written in
/// TOML that need to be served as JSON to providers).
fn toml_to_json(val: &toml::Value) -> JsonValue {
    match val {
        toml::Value::String(s) => JsonValue::String(s.clone()),
        toml::Value::Integer(n) => serde_json::json!(*n),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => JsonValue::Bool(*b),
        toml::Value::Datetime(dt) => JsonValue::String(dt.to_string()),
        toml::Value::Array(arr) => JsonValue::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(tbl) => {
            let map = tbl
                .iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect();
            JsonValue::Object(map)
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn load_manifest() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("plugin.toml"),
            r#"
name = "test-plugin"
description = "A test plugin"

[[tools]]
name = "echo_input"
description = "Echoes back the JSON input"
command = "cat"

[tools.input_schema]
type = "object"
"#,
        )
        .unwrap();

        let plugin = ScriptPlugin::load(&dir.path().join("plugin.toml")).unwrap();
        assert_eq!(plugin.name(), "test-plugin");
        assert_eq!(plugin.description(), "A test plugin");
        assert_eq!(plugin.tools().len(), 1);
        assert_eq!(plugin.tools()[0].name(), "echo_input");
    }

    #[test]
    fn load_manifest_no_tools() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("plugin.toml"),
            r#"
name = "empty"
description = "No tools"
"#,
        )
        .unwrap();

        let plugin = ScriptPlugin::load(&dir.path().join("plugin.toml")).unwrap();
        assert_eq!(plugin.name(), "empty");
        assert!(plugin.tools().is_empty());
    }

    #[test]
    fn invalid_manifest_errors() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("plugin.toml"), "not valid toml {{{{").unwrap();

        let result = ScriptPlugin::load(&dir.path().join("plugin.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn schema_defaults_to_empty_object() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("plugin.toml"),
            r#"
name = "default-schema"
description = "test"

[[tools]]
name = "t"
description = "d"
command = "echo hi"
"#,
        )
        .unwrap();

        let plugin = ScriptPlugin::load(&dir.path().join("plugin.toml")).unwrap();
        let schema = plugin.tools()[0].input_schema();
        assert_eq!(schema, serde_json::json!({"type": "object"}));
    }

    #[cfg(not(windows))]
    #[test]
    fn script_tool_executes_command() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ScriptTool {
            tool_name: "echo_test".into(),
            tool_description: "test".into(),
            command: "cat".into(),
            schema: serde_json::json!({"type": "object"}),
            workdir: dir.path().to_path_buf(),
        };

        let input = serde_json::json!({"hello": "world"});
        let result = tool.execute(input.clone()).unwrap();
        // `cat` echoes stdin back to stdout
        let output: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        assert_eq!(output, input);
    }

    #[cfg(not(windows))]
    #[test]
    fn script_tool_returns_error_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ScriptTool {
            tool_name: "fail".into(),
            tool_description: "test".into(),
            command: "exit 1".into(),
            schema: serde_json::json!({"type": "object"}),
            workdir: dir.path().to_path_buf(),
        };

        let result = tool.execute(serde_json::json!({}));
        assert!(result.is_err());
    }
}
