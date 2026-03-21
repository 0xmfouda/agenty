use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub use agenty_types::{AgentError, ChatMessage, Config};

/// A persisted conversation: config and full tool-aware message history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub config: Config,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tokens: TokenUsage,
}

/// Cumulative token counts over the lifetime of a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input + self.output
    }

    pub fn add(&mut self, other: TokenUsage) {
        self.input += other.input;
        self.output += other.output;
    }
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
            tokens: TokenUsage::default(),
        }
    }

    pub fn push_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
        self.updated_at = now_secs();
    }

    /// Record token usage from a single provider turn.
    pub fn record_tokens(&mut self, usage: TokenUsage) {
        self.tokens.add(usage);
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
    use agenty_types::Provider;

    fn sample_config() -> Config {
        Config {
            model: "claude-sonnet-4-6".into(),
            provider: Provider::Anthropic,
            max_tokens: 1024,
            system_prompt: "you are helpful".into(),
            thinking_budget: None,
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempdir();
        let path = Session::path_in(&dir, "abc");

        let mut session = Session::new("abc", sample_config());
        session.push_message(ChatMessage::user_text("hi"));
        session.push_message(ChatMessage::assistant(vec![
            agenty_types::ContentBlock::Text { text: "hello".into() },
        ]));
        session.save(&path).unwrap();

        session.record_tokens(TokenUsage { input: 120, output: 45 });
        session.record_tokens(TokenUsage { input: 30, output: 10 });
        session.save(&path).unwrap();

        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.id, "abc");
        assert_eq!(loaded.messages, session.messages);
        assert_eq!(loaded.config, session.config);
        assert_eq!(loaded.tokens, TokenUsage { input: 150, output: 55 });
        assert_eq!(loaded.tokens.total(), 205);

        fs::remove_dir_all(&dir).ok();
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("agenty-session-{}", now_secs()));
        fs::create_dir_all(&p).unwrap();
        p
    }
}
