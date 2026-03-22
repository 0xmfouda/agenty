//! Integration tests with Anthropic API.
//!
//! These are skipped when `ANTHROPIC_API_KEY` is not set so CI without a key
//! stays green. Run with:
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-... cargo test -p agenty-providers --features anthropic --test anthropic
//! ```

use agenty_providers::anthropic::AnthropicClient;
use agenty_core::{Config, Message, Provider, Role};
use futures::StreamExt;

const TEST_MODEL: &str = "claude-haiku-4-5-20251001";

fn test_config() -> Config {
    Config {
        model: TEST_MODEL.to_string(),
        provider: Provider::Anthropic,
        max_tokens: 64,
        system_prompt: String::new(),
        thinking_budget: None,
    }
}

fn client_or_skip() -> Option<AnthropicClient> {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!("skipping: ANTHROPIC_API_KEY is not set");
        return None;
    }
    Some(AnthropicClient::new(None).expect("client should build when key is set"))
}

#[tokio::test]
async fn send_message_returns_non_empty_assistant_reply() {
    let Some(client) = client_or_skip() else {
        return;
    };
    let config = test_config();
    let messages = vec![Message::new(
        Role::User,
        "Reply with exactly the single word: pong.",
    )];

    let reply = client
        .send_message(&config, &messages)
        .await
        .expect("send_message should succeed");

    assert_eq!(reply.role, Role::Assistant);
    assert!(
        !reply.content.trim().is_empty(),
        "assistant reply was empty"
    );
}

#[tokio::test]
async fn stream_message_yields_multiple_tokens() {
    let Some(client) = client_or_skip() else {
        return;
    };
    let config = test_config();
    let messages = vec![Message::new(
        Role::User,
        "Count from 1 to 5, separated by spaces.",
    )];

    let mut stream = client
        .stream_message(&config, &messages)
        .await
        .expect("stream_message should succeed");

    let mut token_count = 0;
    let mut full = String::new();
    while let Some(token) = stream.next().await {
        let t = token.expect("stream yielded an error mid-stream");
        full.push_str(&t);
        token_count += 1;
    }

    assert!(
        token_count > 0,
        "expected at least one token from the stream"
    );
    assert!(
        !full.trim().is_empty(),
        "accumulated stream output was empty"
    );
}

#[tokio::test]
async fn send_message_surfaces_auth_error() {
    let client = AnthropicClient::new(Some("sk-ant-invalid-key-for-testing".to_string()))
        .expect("client should build from explicit key");
    let config = test_config();
    let messages = vec![Message::new(Role::User, "hello")];

    let err = client
        .send_message(&config, &messages)
        .await
        .expect_err("expected an auth error");

    let msg = err.to_string();
    assert!(
        msg.contains("401") || msg.to_lowercase().contains("authentication"),
        "unexpected error: {msg}"
    );
}
