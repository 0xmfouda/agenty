use serde::Deserialize;
use serde_json::json;

use crate::{JsonValue, Tool};

#[cfg(target_os = "linux")]
use crate::sandbox::{SandboxPolicy, spawn_sandboxed};

/// Runs a shell command inside a Linux sandbox.
///
/// On Linux the command is executed with Landlock filesystem restrictions,
/// network namespace isolation, resource limits, and a wall-clock timeout.
///
/// On non-Linux platforms the tool refuses to execute — sandboxing is mandatory.
pub struct BashTool {
    #[cfg(target_os = "linux")]
    policy: SandboxPolicy,
}

impl BashTool {
    /// Create a `BashTool` with the given sandbox policy (Linux only).
    #[cfg(target_os = "linux")]
    pub fn new(policy: SandboxPolicy) -> Self {
        Self { policy }
    }

    /// On non-Linux platforms the tool is constructed but every call will fail.
    #[cfg(not(target_os = "linux"))]
    pub fn new() -> Self {
        Self {}
    }
}

#[derive(Deserialize)]
struct Input {
    command: String,
}

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command inside a sandboxed environment and return its stdout, stderr, and exit code. \
         Network access is blocked by default. Filesystem access is restricted to explicitly allowed paths."
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

        #[cfg(target_os = "linux")]
        {
            let output = spawn_sandboxed(&command, &self.policy)?;
            Ok(json!({
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr),
                "exit_code": output.status.code(),
            }))
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = command;
            Err(
                "bash tool requires Linux — sandboxed execution is not available on this platform"
                    .into(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    fn test_tool() -> BashTool {
        use std::path::PathBuf;
        use std::time::Duration;
        BashTool::new(SandboxPolicy {
            allow_network: false,
            read_paths: vec![PathBuf::from("/")],
            write_paths: vec![PathBuf::from("/tmp")],
            timeout: Duration::from_secs(5),
            max_memory_bytes: 256 * 1024 * 1024,
            max_pids: 32,
        })
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runs_a_successful_command() {
        let out = test_tool()
            .execute(json!({ "command": "echo hello" }))
            .unwrap();
        assert_eq!(out["stdout"].as_str().unwrap().trim(), "hello");
        assert_eq!(out["exit_code"].as_i64(), Some(0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn captures_nonzero_exit() {
        let out = test_tool().execute(json!({ "command": "exit 7" })).unwrap();
        assert_eq!(out["exit_code"].as_i64(), Some(7));
    }

    #[test]
    fn rejects_missing_command() {
        #[cfg(target_os = "linux")]
        let tool = test_tool();
        #[cfg(not(target_os = "linux"))]
        let tool = BashTool::new();

        let err = tool.execute(json!({})).unwrap_err();
        assert!(err.contains("invalid input"), "got: {err}");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn refuses_on_non_linux() {
        let tool = BashTool::new();
        let err = tool.execute(json!({ "command": "echo hi" })).unwrap_err();
        assert!(err.contains("Linux"), "got: {err}");
    }
}
