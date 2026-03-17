//! End-to-end test for the REPL query loop against the real Anthropic API.
//!
//! Skipped when `ANTHROPIC_API_KEY` is not set. Run with:
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-... cargo test -p agenty-repl --test repl -- --nocapture
//! ```

use agenty_providers::anthropic::AnthropicClient;
use agenty_repl::Repl;
use agenty_tools::{ListFilesTool, Tool};
use agenty_types::{ChatMessage, Config, ContentBlock, Provider};

const TEST_MODEL: &str = "claude-haiku-4-5-20251001";

fn client_or_skip() -> Option<AnthropicClient> {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!("skipping: ANTHROPIC_API_KEY is not set");
        return None;
    }
    Some(AnthropicClient::new(None).expect("client should build when key is set"))
}

fn has_tool_use(conversation: &[ChatMessage]) -> bool {
    conversation.iter().any(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    })
}

#[tokio::test]
async fn repl_invokes_list_files_tool_and_reports_results() {
    let Some(client) = client_or_skip() else {
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha.txt"), "").unwrap();
    std::fs::write(dir.path().join("beta.txt"), "").unwrap();

    let config = Config {
        model: TEST_MODEL.to_string(),
        provider: Provider::Anthropic,
        max_tokens: 1024,
        system_prompt: "You are a helpful assistant. When the user asks about files, call the list_files tool. After tool results come back, reply with a concise message naming the files.".to_string(),
    };

    let tool = ListFilesTool;
    let repl = Repl::new(&client, &config, vec![&tool as &dyn Tool]);

    let prompt = format!(
        "List the files in the directory {}. After calling the tool, tell me the file names.",
        dir.path().display()
    );

    let conversation = repl.run(&prompt).await.expect("repl run should succeed");

    assert!(
        has_tool_use(&conversation),
        "expected at least one tool_use block in the conversation; got: {conversation:#?}"
    );

    let final_text = conversation
        .last()
        .expect("conversation should be non-empty")
        .text();

    assert!(
        final_text.contains("alpha") && final_text.contains("beta"),
        "expected final assistant text to mention alpha and beta; got: {final_text}"
    );
}
