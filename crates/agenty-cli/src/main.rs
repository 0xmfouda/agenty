use std::process::ExitCode;

use agenty_providers::anthropic::AnthropicClient;
use agenty_repl::Repl;
use agenty_tools::{BashTool, ListFilesTool, ReadFileTool, Tool, WriteFileTool};
use agenty_types::{Config, Provider};
use clap::Parser;

const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";

/// Headless agent runner.
#[derive(Parser, Debug)]
#[command(name = "agenty", version, about)]
struct Cli {
    /// Run a single prompt non-interactively and print the final answer.
    #[arg(short = 'p', long, value_name = "TEXT")]
    prompt: Option<String>,

    /// Model id to use for the provider.
    #[arg(short = 'm', long, default_value = DEFAULT_MODEL)]
    model: String,

    /// Max tokens per provider response.
    #[arg(long, default_value_t = 1024)]
    max_tokens: u32,

    /// System prompt prepended to every request.
    #[arg(short = 's', long, default_value = "")]
    system: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let Some(ref prompt) = cli.prompt else {
        eprintln!("interactive mode is not implemented yet; pass -p <PROMPT>");
        return ExitCode::from(2);
    };

    match run_headless(&cli, prompt).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run_headless(cli: &Cli, prompt: &str) -> Result<(), agenty_types::AgentError> {
    let config = Config {
        model: cli.model.clone(),
        provider: Provider::Anthropic,
        max_tokens: cli.max_tokens,
        system_prompt: cli.system.clone(),
    };

    let client = AnthropicClient::new(None)?;

    let bash = BashTool;
    let read = ReadFileTool;
    let write = WriteFileTool;
    let list = ListFilesTool;
    let tools: Vec<&dyn Tool> = vec![&bash, &read, &write, &list];

    let repl = Repl::new(&client, &config, tools);
    let conversation = repl.run(prompt).await?;

    if let Some(last) = conversation.last() {
        let text = last.text();
        if !text.is_empty() {
            println!("{text}");
        }
    }
    Ok(())
}
