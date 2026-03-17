use std::fs;

use serde::Deserialize;
use serde_json::json;

use crate::{JsonValue, Tool};

/// Lists the immediate entries of a directory.
pub struct ListFilesTool;

#[derive(Deserialize)]
struct Input {
    path: String,
}

impl Tool for ListFilesTool {
    fn name(&self) -> &str {
        "list_files"
    }

    fn description(&self) -> &str {
        "List the immediate entries of a directory. Returns name, path, and whether each entry is a directory."
    }

    fn input_schema(&self) -> JsonValue {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list." }
            },
            "required": ["path"]
        })
    }

    fn execute(&self, input: JsonValue) -> Result<JsonValue, String> {
        let Input { path } =
            serde_json::from_value(input).map_err(|e| format!("invalid input: {e}"))?;

        let mut entries = Vec::new();
        for entry in fs::read_dir(&path).map_err(|e| format!("failed to read {path}: {e}"))? {
            let entry = entry.map_err(|e| format!("failed to read entry: {e}"))?;
            let file_type = entry
                .file_type()
                .map_err(|e| format!("failed to stat entry: {e}"))?;
            entries.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "path": entry.path().to_string_lossy(),
                "is_dir": file_type.is_dir(),
            }));
        }
        entries.sort_by(|a, b| {
            a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or(""))
        });

        Ok(json!({ "path": path, "entries": entries }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_files_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();

        let out = ListFilesTool
            .execute(json!({ "path": dir.path().to_str().unwrap() }))
            .unwrap();

        let entries = out["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["name"], "a.txt");
        assert_eq!(entries[0]["is_dir"], false);
        assert_eq!(entries[1]["name"], "sub");
        assert_eq!(entries[1]["is_dir"], true);
    }

    #[test]
    fn errors_on_missing_dir() {
        let err = ListFilesTool
            .execute(json!({ "path": "/nonexistent/definitely-not-here" }))
            .unwrap_err();
        assert!(err.contains("failed to read"), "got: {err}");
    }
}
