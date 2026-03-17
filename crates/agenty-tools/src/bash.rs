use std::process::Command;

use serde::Deserialize;
use serde_json::json;

use crate::{JsonValue, Tool};

/// Runs a shell command via `sh -c` and returns stdout, stderr, and exit code.
pub struct BashTool;

#[derive(Deserialize)]
struct Input {
    command: String,
}

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command with `sh -c` and return its stdout, stderr, and exit code."
    }

    fn input_schema(&self) -> JsonValue {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute." }
            },
            "required": ["command"]
        })
    }

    fn execute(&self, input: JsonValue) -> Result<JsonValue, String> {
        let Input { command } =
            serde_json::from_value(input).map_err(|e| format!("invalid input: {e}"))?;

        let output = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .output()
            .map_err(|e| format!("failed to spawn command: {e}"))?;

        Ok(json!({
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
            "exit_code": output.status.code(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_a_successful_command() {
        let out = BashTool.execute(json!({ "command": "echo hello" })).unwrap();
        assert_eq!(out["stdout"].as_str().unwrap().trim(), "hello");
        assert_eq!(out["exit_code"].as_i64(), Some(0));
    }

    #[test]
    fn captures_nonzero_exit() {
        let out = BashTool.execute(json!({ "command": "exit 7" })).unwrap();
        assert_eq!(out["exit_code"].as_i64(), Some(7));
    }

    #[test]
    fn rejects_missing_command() {
        let err = BashTool.execute(json!({})).unwrap_err();
        assert!(err.contains("invalid input"), "got: {err}");
    }
}
