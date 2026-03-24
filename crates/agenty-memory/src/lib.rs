use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A single memory entry stored as its own file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    /// Unix timestamp when the memory was created.
    pub created_at: u64,
    /// Short one-line summary for the MEMORY.md index.
    pub summary: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Manages a directory of memory files and a `MEMORY.md` index.
///
/// Layout:
/// ```text
/// <root>/
///   MEMORY.md          — one-line summaries for fast scanning
///   entries/
///     <id>.json        — full memory content
/// ```
pub struct MemoryStore {
    root: PathBuf,
}

impl MemoryStore {
    /// Open or create a memory store at `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("entries"))
            .map_err(|e| format!("failed to create memory directory: {e}"))?;
        let store = Self { root };
        // Ensure MEMORY.md exists
        if !store.index_path().exists() {
            fs::write(store.index_path(), "# Memories\n")
                .map_err(|e| format!("failed to create MEMORY.md: {e}"))?;
        }
        Ok(store)
    }

    /// Save a new memory. Writes the entry file and updates the index.
    pub fn save(&self, summary: &str, content: &str, tags: Vec<String>) -> Result<Memory, String> {
        let id = generate_id();
        let memory = Memory {
            id: id.clone(),
            created_at: now_secs(),
            summary: summary.to_string(),
            content: content.to_string(),
            tags,
        };

        let entry_path = self.entry_path(&id);
        let bytes = serde_json::to_vec_pretty(&memory)
            .map_err(|e| format!("failed to serialize memory: {e}"))?;
        fs::write(&entry_path, bytes).map_err(|e| format!("failed to write memory file: {e}"))?;

        self.rebuild_index()?;
        Ok(memory)
    }

    /// Delete a memory by id.
    pub fn delete(&self, id: &str) -> Result<(), String> {
        let path = self.entry_path(id);
        if !path.exists() {
            return Err(format!("memory '{id}' not found"));
        }
        fs::remove_file(&path).map_err(|e| format!("failed to delete memory: {e}"))?;
        self.rebuild_index()
    }

    /// Load a single memory by id.
    pub fn get(&self, id: &str) -> Result<Memory, String> {
        let path = self.entry_path(id);
        let bytes = fs::read(&path).map_err(|e| format!("failed to read memory '{id}': {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("failed to parse memory '{id}': {e}"))
    }

    /// List all memories (summary only, loaded from entry files).
    pub fn list(&self) -> Result<Vec<Memory>, String> {
        let entries_dir = self.root.join("entries");
        let mut memories = Vec::new();

        let read_dir = fs::read_dir(&entries_dir)
            .map_err(|e| format!("failed to read entries directory: {e}"))?;

        for entry in read_dir {
            let entry = entry.map_err(|e| format!("failed to read dir entry: {e}"))?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let bytes = fs::read(&path)
                    .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
                let memory: Memory = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
                memories.push(memory);
            }
        }

        memories.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(memories)
    }

    /// Search memories by keyword
    pub fn search(&self, query: &str) -> Result<Vec<Memory>, String> {
        let query_lower = query.to_lowercase();
        let keywords: Vec<&str> = query_lower.split_whitespace().collect();

        let all = self.list()?;
        let results: Vec<Memory> = all
            .into_iter()
            .filter(|m| {
                let haystack =
                    format!("{} {} {}", m.summary, m.content, m.tags.join(" ")).to_lowercase();
                keywords.iter().all(|kw| haystack.contains(kw))
            })
            .collect();

        Ok(results)
    }

    /// Read the MEMORY.md index contents.
    pub fn read_index(&self) -> Result<String, String> {
        fs::read_to_string(self.index_path()).map_err(|e| format!("failed to read MEMORY.md: {e}"))
    }

    /// Rebuild MEMORY.md from all entry files.
    fn rebuild_index(&self) -> Result<(), String> {
        let memories = self.list()?;
        let mut index = String::from("# Memories\n\n");

        for m in &memories {
            let tags = if m.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", m.tags.join(", "))
            };
            index.push_str(&format!("- **{}** — {}{}\n", m.id, m.summary, tags));
        }

        fs::write(self.index_path(), index).map_err(|e| format!("failed to write MEMORY.md: {e}"))
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("MEMORY.md")
    }

    fn entry_path(&self, id: &str) -> PathBuf {
        self.root.join("entries").join(format!("{id}.json"))
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn generate_id() -> String {
    let ts = now_secs();
    let random: u32 = (ts as u32).wrapping_mul(2654435761); // simple hash spread
    format!("{ts:x}-{random:06x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (MemoryStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::open(dir.path().join("memory")).unwrap();
        (store, dir)
    }

    #[test]
    fn save_and_retrieve() {
        let (store, _dir) = temp_store();
        let mem = store
            .save("test summary", "full content here", vec!["tag1".into()])
            .unwrap();
        let loaded = store.get(&mem.id).unwrap();
        assert_eq!(loaded.summary, "test summary");
        assert_eq!(loaded.content, "full content here");
        assert_eq!(loaded.tags, vec!["tag1"]);
    }

    #[test]
    fn list_returns_all() {
        let (store, _dir) = temp_store();
        store.save("first", "content 1", vec![]).unwrap();
        store.save("second", "content 2", vec![]).unwrap();
        let all = store.list().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn search_by_keyword() {
        let (store, _dir) = temp_store();
        store
            .save(
                "API rotation",
                "the API key rotates weekly",
                vec!["infra".into()],
            )
            .unwrap();
        store
            .save("meeting notes", "discussed frontend refactor", vec![])
            .unwrap();

        let results = store.search("API").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].summary, "API rotation");

        let results = store.search("infra").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn delete_removes_entry() {
        let (store, _dir) = temp_store();
        let mem = store.save("to delete", "bye", vec![]).unwrap();
        assert!(store.get(&mem.id).is_ok());
        store.delete(&mem.id).unwrap();
        assert!(store.get(&mem.id).is_err());
        assert_eq!(store.list().unwrap().len(), 0);
    }

    #[test]
    fn index_reflects_entries() {
        let (store, _dir) = temp_store();
        store
            .save("first memory", "content", vec!["tag".into()])
            .unwrap();
        let index = store.read_index().unwrap();
        assert!(index.contains("first memory"));
        assert!(index.contains("[tag]"));
    }
}
