use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::json;

use crate::{JsonValue, Tool};
use agenty_memory::MemoryStore;

/// Tool that lets the agent save, search, list, and delete memories.
///
/// The agent chooses an `action` and the tool dispatches accordingly.
pub struct MemoryTool {
    store: Arc<Mutex<MemoryStore>>,
}

impl MemoryTool {
    pub fn new(store: Arc<Mutex<MemoryStore>>) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct Input {
    action: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Persist and recall information across conversations.\n\
         Actions:\n\
         - \"save\": Save a memory. Requires `summary` (one-line) and `content` (full detail). Optional `tags`.\n\
         - \"search\": Search memories by keyword. Requires `query`.\n\
         - \"list\": List all stored memories (summaries only).\n\
         - \"delete\": Delete a memory. Requires `id`."
    }

    fn input_schema(&self) -> JsonValue {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["save", "search", "list", "delete"],
                    "description": "The memory operation to perform."
                },
                "summary": {
                    "type": "string",
                    "description": "One-line summary for the memory index (required for 'save')."
                },
                "content": {
                    "type": "string",
                    "description": "Full memory content (required for 'save')."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional tags for categorization (used with 'save')."
                },
                "query": {
                    "type": "string",
                    "description": "Search query (required for 'search')."
                },
                "id": {
                    "type": "string",
                    "description": "Memory id (required for 'delete')."
                }
            },
            "required": ["action"]
        })
    }

    fn execute(&self, input: JsonValue) -> Result<JsonValue, String> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| format!("invalid input: {e}"))?;

        let store = self.store.lock().map_err(|e| format!("lock error: {e}"))?;

        match input.action.as_str() {
            "save" => {
                let summary = input
                    .summary
                    .as_deref()
                    .ok_or("'summary' is required for save")?;
                let content = input
                    .content
                    .as_deref()
                    .ok_or("'content' is required for save")?;
                let tags = input.tags.unwrap_or_default();
                let mem = store.save(summary, content, tags)?;
                Ok(json!({ "status": "saved", "id": mem.id }))
            }
            "search" => {
                let query = input
                    .query
                    .as_deref()
                    .ok_or("'query' is required for search")?;
                let results = store.search(query)?;
                let entries: Vec<JsonValue> = results
                    .iter()
                    .map(|m| {
                        json!({
                            "id": m.id,
                            "summary": m.summary,
                            "content": m.content,
                            "tags": m.tags,
                        })
                    })
                    .collect();
                Ok(json!({ "results": entries, "count": entries.len() }))
            }
            "list" => {
                let all = store.list()?;
                let entries: Vec<JsonValue> = all
                    .iter()
                    .map(|m| {
                        json!({
                            "id": m.id,
                            "summary": m.summary,
                            "tags": m.tags,
                        })
                    })
                    .collect();
                Ok(json!({ "memories": entries, "count": entries.len() }))
            }
            "delete" => {
                let id = input.id.as_deref().ok_or("'id' is required for delete")?;
                store.delete(id)?;
                Ok(json!({ "status": "deleted", "id": id }))
            }
            other => Err(format!(
                "unknown action: '{other}'. Use save, search, list, or delete."
            )),
        }
    }
}
