use std::pin::Pin;

use futures::Stream;

pub use agenty_types::{AgentError, Message};

/// A token yielded by a streaming completion.
pub type Token = String;

/// A pinned, heap-allocated token stream returned by [`Provider::complete`].
pub type TokenStream<'a> =
    Pin<Box<dyn Stream<Item = Result<Token, AgentError>> + Send + 'a>>;

/// Trait that every LLM backend must implement.
///
/// Implementors are responsible for turning a conversation history into a
/// stream of string tokens.  No network I/O is required by this interface
/// itself; concrete provider crates supply that.
pub trait Provider: Send + Sync {
    /// Stream completion tokens for the given conversation `messages`.
    ///
    /// The returned stream yields one [`Token`] per item.  Errors that occur
    /// mid-stream (e.g. a dropped connection) are surfaced as
    /// `Err(AgentError::Provider(…))` items rather than terminating the stream
    /// abruptly.
    fn complete<'a>(&'a self, messages: &'a [Message]) -> TokenStream<'a>;
}
