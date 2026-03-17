use std::fs;

use serde::Deserialize;
use serde_json::json;

use crate::{JsonValue, Tool};

/// Reads a UTF-8 text file and returns its contents.
pub struct ReadFileTool;

#[derive(Deserialize)]
struct Input {
    path: String,
}

impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file from the local filesystem and return its contents."
    }

    fn input_schema(&self) -> JsonValue {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read." }
            },
            "required": ["path"]
        })
    }

    fn execute(&self, input: JsonValue) -> Result<JsonValue, String> {
        let Input { path } =
            serde_json::from_value(input).map_err(|e| format!("invalid input: {e}"))?;

        let contents =
            fs::read_to_string(&path).map_err(|e| format!("failed to read {path}: {e}"))?;

        Ok(json!({ "path": path, "contents": contents }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_existing_file() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "hello world").unwrap();
        let path = f.path().to_str().unwrap().to_owned();

        let out = ReadFileTool.execute(json!({ "path": path })).unwrap();
        assert_eq!(out["contents"].as_str().unwrap().trim(), "hello world");
    }

    #[test]
    fn errors_on_missing_file() {
        let err = ReadFileTool
            .execute(json!({ "path": "/nonexistent/definitely-not-here" }))
            .unwrap_err();
        assert!(err.contains("failed to read"), "got: {err}");
    }
}
