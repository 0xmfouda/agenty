use std::fs;

use serde::Deserialize;
use serde_json::json;

use crate::{JsonValue, Tool};

/// Writes UTF-8 content to a file, creating parent directories as needed.
pub struct WriteFileTool;

#[derive(Deserialize)]
struct Input {
    path: String,
    contents: String,
}

impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write UTF-8 contents to a file on the local filesystem, creating parent directories if they do not exist. Overwrites any existing file."
    }

    fn input_schema(&self) -> JsonValue {
        json!({
            "type": "object",
            "properties": {
                "path":     { "type": "string", "description": "Destination path." },
                "contents": { "type": "string", "description": "UTF-8 contents to write." }
            },
            "required": ["path", "contents"]
        })
    }

    fn execute(&self, input: JsonValue) -> Result<JsonValue, String> {
        let Input { path, contents } =
            serde_json::from_value(input).map_err(|e| format!("invalid input: {e}"))?;

        if let Some(parent) = std::path::Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("failed to create parent dir: {e}"))?;
            }
        }

        let bytes_written = contents.len();
        fs::write(&path, contents).map_err(|e| format!("failed to write {path}: {e}"))?;

        Ok(json!({ "path": path, "bytes_written": bytes_written }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_file_and_creates_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/sub/out.txt");
        let path_str = path.to_str().unwrap().to_owned();

        let out = WriteFileTool
            .execute(json!({ "path": path_str, "contents": "hi" }))
            .unwrap();

        assert_eq!(out["bytes_written"].as_u64(), Some(2));
        assert_eq!(fs::read_to_string(&path).unwrap(), "hi");
    }

    #[test]
    fn overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        fs::write(&path, "old").unwrap();

        WriteFileTool
            .execute(json!({ "path": path.to_str().unwrap(), "contents": "new" }))
            .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn rejects_missing_fields() {
        let err = WriteFileTool
            .execute(json!({ "path": "/tmp/x" }))
            .unwrap_err();
        assert!(err.contains("invalid input"), "got: {err}");
    }
}
