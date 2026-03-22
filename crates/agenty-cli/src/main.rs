use std::process::ExitCode;

use agenty_core::{AgentError, Config, Provider};
use agenty_providers::ChatClient;
use agenty_providers::anthropic::AnthropicClient;
use agenty_providers::gemini::GeminiClient;
use agenty_providers::openai::OpenAIClient;
use agenty_repl::Repl;
use agenty_tools::{BashTool, ListFilesTool, ReadFileTool, Tool, WriteFileTool};
use clap::{Parser, ValueEnum};

const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
const DEFAULT_GEMINI_MODEL: &str = "gemini-2.0-flash";

/// Headless agent runner.
#[derive(Parser, Debug)]
#[command(name = "agenty", version, about)]
struct Cli {
    /// Run a single prompt non-interactively and print the final answer.
    #[arg(short = 'p', long, value_name = "TEXT")]
    prompt: Option<String>,

    /// LLM provider to route requests to.
    #[arg(long, value_enum, default_value_t = ProviderArg::Anthropic)]
    provider: ProviderArg,

    /// Model id to use for the provider. Defaults depend on `--provider`.
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// Max tokens per provider response.
    #[arg(long, default_value_t = 1024)]
    max_tokens: u32,

    /// System prompt prepended to every request.
    #[arg(short = 's', long, default_value = "")]
    system: String,

    /// Enable extended thinking with the given token budget (e.g. `--thinking 4096`).
    /// Anthropic-only; ignored for other providers.
    #[arg(long, value_name = "BUDGET")]
    thinking: Option<u32>,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ProviderArg {
    Anthropic,
    Openai,
    Gemini,
}

impl ProviderArg {
    fn to_core(self) -> Provider {
        match self {
            ProviderArg::Anthropic => Provider::Anthropic,
            ProviderArg::Openai => Provider::OpenAI,
            ProviderArg::Gemini => Provider::Gemini,
        }
    }
    fn default_model(self) -> &'static str {
        match self {
            ProviderArg::Anthropic => DEFAULT_ANTHROPIC_MODEL,
            ProviderArg::Openai => DEFAULT_OPENAI_MODEL,
            ProviderArg::Gemini => DEFAULT_GEMINI_MODEL,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    let result = match cli.prompt.clone() {
        Some(prompt) => run_headless(&cli, &prompt).await,
        None => run_tui(&cli).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn build_config(cli: &Cli) -> Config {
    Config {
        model: cli
            .model
            .clone()
            .unwrap_or_else(|| cli.provider.default_model().to_string()),
        provider: cli.provider.to_core(),
        max_tokens: cli.max_tokens,
        system_prompt: cli.system.clone(),
        thinking_budget: cli.thinking,
    }
}

fn build_client(provider: ProviderArg) -> Result<ChatClient, AgentError> {
    Ok(match provider {
        ProviderArg::Anthropic => ChatClient::Anthropic(AnthropicClient::new(None)?),
        ProviderArg::Openai => ChatClient::OpenAI(OpenAIClient::new(None)?),
        ProviderArg::Gemini => ChatClient::Gemini(GeminiClient::new(None)?),
    })
}

async fn run_headless(cli: &Cli, prompt: &str) -> Result<(), AgentError> {
    let config = build_config(cli);
    let client = build_client(cli.provider)?;

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

async fn run_tui(cli: &Cli) -> Result<(), AgentError> {
    let config = build_config(cli);
    let client = build_client(cli.provider)?;

    let bash = BashTool;
    let read = ReadFileTool;
    let write = WriteFileTool;
    let list = ListFilesTool;
    let tools: Vec<&dyn Tool> = vec![&bash, &read, &write, &list];

    let repl = Repl::new(&client, &config, tools);
    agenty_tui::run(repl).await
}
