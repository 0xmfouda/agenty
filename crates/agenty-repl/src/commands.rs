//! Slash-command dispatch wrapped around a persistent [`Repl`].
//!
//! [`Repl`]: crate::Repl

use std::path::{Path, PathBuf};

use agenty_session::Session;
use agenty_types::{AgentError, ChatMessage};

use crate::Repl;

/// A parsed slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Checkpoint,
    Clear,
    Exit,
    Unknown(String),
}

/// The result of handling a single line of user input.
#[derive(Debug)]
pub enum ReplOutcome {
    /// A normal model response; contains the messages appended this turn.
    Response { new_messages: Vec<ChatMessage> },
    Help(String),
    Cleared,
    Checkpointed(PathBuf),
    Exit,
    UnknownCommand(String),
}

pub const HELP_TEXT: &str = "\
Available commands:
  /help        Show this help message
  /checkpoint  Save the current conversation to disk
  /clear       Clear the conversation history
  /exit        Exit the REPL

Any other input is sent to the model as a user prompt.";

/// Parse a line of input as a slash command.
/// Returns `None` when the input is not a slash command.
pub fn parse_command(input: &str) -> Option<Command> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix('/')?;
    let name = rest.split_whitespace().next().unwrap_or("");
    Some(match name {
        "help" => Command::Help,
        "checkpoint" => Command::Checkpoint,
        "clear" => Command::Clear,
        "exit" | "quit" => Command::Exit,
        other => Command::Unknown(other.to_string()),
    })
}

/// Persistent REPL: holds conversation state across multiple inputs and
/// dispatches slash commands.
pub struct ReplSession<'a> {
    repl: Repl<'a>,
    session_id: String,
    conversation: Vec<ChatMessage>,
    checkpoint_dir: PathBuf,
}

impl<'a> ReplSession<'a> {
    pub fn new(
        repl: Repl<'a>,
        session_id: impl Into<String>,
        checkpoint_dir: impl AsRef<Path>,
    ) -> Self {
        Self {
            repl,
            session_id: session_id.into(),
            conversation: Vec::new(),
            checkpoint_dir: checkpoint_dir.as_ref().to_path_buf(),
        }
    }

    pub fn conversation(&self) -> &[ChatMessage] {
        &self.conversation
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Handle one line of user input: dispatch to a slash command or run the
    /// provider query loop.
    pub async fn handle_input(&mut self, input: &str) -> Result<ReplOutcome, AgentError> {
        if let Some(cmd) = parse_command(input) {
            return self.run_command(cmd);
        }

        let start = self.conversation.len();
        self.repl.run_turn(&mut self.conversation, input.trim()).await?;
        Ok(ReplOutcome::Response {
            new_messages: self.conversation[start..].to_vec(),
        })
    }

    fn run_command(&mut self, cmd: Command) -> Result<ReplOutcome, AgentError> {
        match cmd {
            Command::Help => Ok(ReplOutcome::Help(HELP_TEXT.to_string())),
            Command::Clear => {
                self.conversation.clear();
                Ok(ReplOutcome::Cleared)
            }
            Command::Exit => Ok(ReplOutcome::Exit),
            Command::Checkpoint => {
                let mut session =
                    Session::new(self.session_id.clone(), self.repl.config().clone());
                for msg in &self.conversation {
                    session.push_message(msg.clone());
                }
                let path = Session::path_in(&self.checkpoint_dir, &self.session_id);
                session.save(&path)?;
                Ok(ReplOutcome::Checkpointed(path))
            }
            Command::Unknown(name) => Ok(ReplOutcome::UnknownCommand(name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agenty_providers::anthropic::AnthropicClient;
    use agenty_types::{ChatMessage, ContentBlock, Provider};

    fn dummy_config() -> agenty_types::Config {
        agenty_types::Config {
            model: "test".into(),
            provider: Provider::Anthropic,
            max_tokens: 64,
            system_prompt: String::new(),
        }
    }

    fn dummy_client() -> AnthropicClient {
        // An explicit key means `new` does not require the env var; no network
        // call happens for the command-only tests below.
        AnthropicClient::new(Some("sk-ant-test-not-real".into())).unwrap()
    }

    fn dummy_session<'a>(
        client: &'a AnthropicClient,
        config: &'a agenty_types::Config,
        dir: &std::path::Path,
    ) -> ReplSession<'a> {
        let repl = Repl::new(client, config, Vec::new());
        ReplSession::new(repl, "test-session", dir)
    }

    #[test]
    fn parses_known_commands() {
        assert_eq!(parse_command("/help"), Some(Command::Help));
        assert_eq!(parse_command("/checkpoint"), Some(Command::Checkpoint));
        assert_eq!(parse_command("/clear"), Some(Command::Clear));
        assert_eq!(parse_command("/exit"), Some(Command::Exit));
        assert_eq!(parse_command("/quit"), Some(Command::Exit));
        assert_eq!(
            parse_command("/checkpoint with trailing args"),
            Some(Command::Checkpoint)
        );
        assert_eq!(
            parse_command("  /help  "),
            Some(Command::Help),
            "leading/trailing whitespace should be ignored"
        );
    }

    #[test]
    fn parses_unknown_command() {
        assert_eq!(
            parse_command("/blahblah foo"),
            Some(Command::Unknown("blahblah".into()))
        );
    }

    #[test]
    fn treats_non_slash_input_as_prompt() {
        assert_eq!(parse_command("hello world"), None);
        assert_eq!(parse_command(""), None);
    }

    #[tokio::test]
    async fn help_returns_help_text() {
        let dir = tempfile::tempdir().unwrap();
        let client = dummy_client();
        let config = dummy_config();
        let mut session = dummy_session(&client, &config, dir.path());

        let outcome = session.handle_input("/help").await.unwrap();
        match outcome {
            ReplOutcome::Help(text) => assert!(text.contains("/checkpoint")),
            other => panic!("expected Help, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn clear_empties_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let client = dummy_client();
        let config = dummy_config();
        let mut session = dummy_session(&client, &config, dir.path());

        // Seed the conversation without going through the provider.
        session.conversation.push(ChatMessage::user_text("hello"));
        session.conversation.push(ChatMessage::assistant(vec![
            ContentBlock::Text { text: "hi".into() },
        ]));
        assert_eq!(session.conversation().len(), 2);

        let outcome = session.handle_input("/clear").await.unwrap();
        assert!(matches!(outcome, ReplOutcome::Cleared));
        assert!(session.conversation().is_empty());
    }

    #[tokio::test]
    async fn exit_returns_exit_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let client = dummy_client();
        let config = dummy_config();
        let mut session = dummy_session(&client, &config, dir.path());

        let outcome = session.handle_input("/exit").await.unwrap();
        assert!(matches!(outcome, ReplOutcome::Exit));
    }

    #[tokio::test]
    async fn checkpoint_writes_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let client = dummy_client();
        let config = dummy_config();
        let mut session = dummy_session(&client, &config, dir.path());

        session.conversation.push(ChatMessage::user_text("hello"));
        session.conversation.push(ChatMessage::assistant(vec![
            ContentBlock::Text { text: "hi there".into() },
        ]));

        let outcome = session.handle_input("/checkpoint").await.unwrap();
        let path = match outcome {
            ReplOutcome::Checkpointed(p) => p,
            other => panic!("expected Checkpointed, got {other:?}"),
        };

        assert!(path.exists(), "checkpoint file should exist at {path:?}");
        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.id, "test-session");
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].text(), "hello");
        assert_eq!(loaded.messages[1].text(), "hi there");
    }

    #[tokio::test]
    async fn unknown_slash_command_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let client = dummy_client();
        let config = dummy_config();
        let mut session = dummy_session(&client, &config, dir.path());

        let outcome = session.handle_input("/nonsense").await.unwrap();
        match outcome {
            ReplOutcome::UnknownCommand(name) => assert_eq!(name, "nonsense"),
            other => panic!("expected UnknownCommand, got {other:?}"),
        }
    }
}
