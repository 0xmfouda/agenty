use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub use agenty_types::{AgentError, Config, Message, ToolCall, ToolResult};

/// A persisted conversation: config, full message history, and any
/// outstanding tool-call/result pairs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub config: Config,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub tool_results: Vec<ToolResult>,
}

impl Session {
    pub fn new(id: impl Into<String>, config: Config) -> Self {
        let now = now_secs();
        Self {
            id: id.into(),
            created_at: now,
            updated_at: now,
            config,
            messages: Vec::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    pub fn push_message(&mut self, message: Message) {
        self.messages.push(message);
        self.updated_at = now_secs();
    }

    /// Serialize the session as pretty JSON and write it to `path`, creating
    /// the parent directory if needed.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), AgentError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_err)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(serde_err)?;
        fs::write(path, bytes).map_err(io_err)
    }

    /// Load a session from a JSON file on disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, AgentError> {
        let bytes = fs::read(path.as_ref()).map_err(io_err)?;
        serde_json::from_slice(&bytes).map_err(serde_err)
    }

    /// Default on-disk location for a session id: `<dir>/<id>.json`.
    pub fn path_in(dir: impl AsRef<Path>, id: &str) -> PathBuf {
        dir.as_ref().join(format!("{id}.json"))
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn io_err(e: std::io::Error) -> AgentError {
    AgentError::Session(e.to_string())
}

fn serde_err(e: serde_json::Error) -> AgentError {
    AgentError::Session(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agenty_types::{Provider, Role};

    fn sample_config() -> Config {
        Config {
            model: "claude-sonnet-4-6".into(),
            provider: Provider::Anthropic,
            max_tokens: 1024,
            system_prompt: "you are helpful".into(),
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempdir();
        let path = Session::path_in(&dir, "abc");

        let mut session = Session::new("abc", sample_config());
        session.push_message(Message::new(Role::User, "hi"));
        session.push_message(Message::new(Role::Assistant, "hello"));
        session.save(&path).unwrap();

        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.id, "abc");
        assert_eq!(loaded.messages, session.messages);
        assert_eq!(loaded.config, session.config);

        fs::remove_dir_all(&dir).ok();
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("agenty-session-{}", now_secs()));
        fs::create_dir_all(&p).unwrap();
        p
    }
}
