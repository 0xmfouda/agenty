use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use agenty_core::{AgentError, Config, Provider};
use agenty_memory::MemoryStore;
use agenty_plugins::PluginRegistry;
use agenty_providers::ChatClient;
use agenty_providers::anthropic::AnthropicClient;
use agenty_providers::gemini::GeminiClient;
use agenty_providers::openai::OpenAIClient;
use agenty_providers::rate_limit::RateLimitedClient;
use agenty_repl::Repl;
#[cfg(target_os = "linux")]
use agenty_tools::sandbox::SandboxPolicy;
use agenty_tools::{
    BashTool, ListFilesTool, MemoryTool, ReadFileTool, Tool, WebSearchTool, WriteFileTool,
};
use clap::{Parser, ValueEnum};

const DEFAULT_ANTHROPIC_MODEL: &str = "claude-haiku-4-5-20251001";
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

    /// Directory to scan for plugins (default: ~/.agenty/plugins).
    #[arg(long, value_name = "DIR")]
    plugins_dir: Option<String>,

    /// Rate limit: max requests per minute to the provider (e.g. `--rpm 60`).
    /// Omit to disable rate limiting.
    #[arg(long, value_name = "N")]
    rpm: Option<u32>,
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

fn build_memory_store() -> Result<Arc<Mutex<MemoryStore>>, AgentError> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| AgentError::Config("cannot determine home directory".into()))?;
    let path = std::path::PathBuf::from(home)
        .join(".agenty")
        .join("memory");
    let store = MemoryStore::open(&path)
        .map_err(|e| AgentError::Config(format!("failed to open memory store: {e}")))?;
    Ok(Arc::new(Mutex::new(store)))
}

fn build_client(provider: ProviderArg) -> Result<ChatClient, AgentError> {
    Ok(match provider {
        ProviderArg::Anthropic => ChatClient::Anthropic(AnthropicClient::new(None)?),
        ProviderArg::Openai => ChatClient::OpenAI(OpenAIClient::new(None)?),
        ProviderArg::Gemini => ChatClient::Gemini(GeminiClient::new(None)?),
    })
}

fn build_plugin_registry(cli: &Cli) -> PluginRegistry {
    let mut registry = PluginRegistry::new();

    let plugins_dir = match &cli.plugins_dir {
        Some(dir) => std::path::PathBuf::from(dir),
        None => {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_default();
            std::path::PathBuf::from(home)
                .join(".agenty")
                .join("plugins")
        }
    };

    if plugins_dir.is_dir() {
        match registry.discover(&plugins_dir) {
            Ok(n) if n > 0 => eprintln!("loaded {n} plugin(s) from {}", plugins_dir.display()),
            Ok(_) => {}
            Err(e) => eprintln!("warning: plugin discovery failed: {e}"),
        }
    }

    registry
}

async fn run_headless(cli: &Cli, prompt: &str) -> Result<(), AgentError> {
    let config = build_config(cli);
    let client = build_client(cli.provider)?;
    let memory_store = build_memory_store()?;
    let registry = build_plugin_registry(cli);

    #[cfg(target_os = "linux")]
    let bash = BashTool::new(SandboxPolicy::default());
    #[cfg(not(target_os = "linux"))]
    let bash = BashTool::new();
    let read = ReadFileTool;
    let write = WriteFileTool;
    let list = ListFilesTool;
    let search = WebSearchTool;
    let mem = MemoryTool::new(memory_store);
    let mut tools: Vec<&dyn Tool> = vec![&bash, &read, &write, &list, &search, &mem];
    let plugin_tools = registry.tools();
    tools.extend(&plugin_tools);

    let print_result = |conversation: &[agenty_core::ChatMessage]| {
        if let Some(last) = conversation.last() {
            let text = last.text();
            if !text.is_empty() {
                println!("{text}");
            }
        }
    };

    if let Some(rpm) = cli.rpm {
        let client = RateLimitedClient::new(client, rpm);
        // Log to stderr when the rate limiter kicks in.
        let mut status_rx = client.status_rx();
        tokio::spawn(async move {
            while status_rx.changed().await.is_ok() {
                if *status_rx.borrow() == agenty_providers::rate_limit::RateLimitStatus::Throttled {
                    eprintln!("rate limited — waiting for permit…");
                }
            }
        });
        let repl = Repl::new(&client, &config, tools);
        let conversation = repl.run(prompt).await?;
        print_result(&conversation);
    } else {
        let repl = Repl::new(&client, &config, tools);
        let conversation = repl.run(prompt).await?;
        print_result(&conversation);
    }
    Ok(())
}

async fn run_tui(cli: &Cli) -> Result<(), AgentError> {
    let config = build_config(cli);
    let client = build_client(cli.provider)?;
    let memory_store = build_memory_store()?;
    let registry = build_plugin_registry(cli);

    #[cfg(target_os = "linux")]
    let bash = BashTool::new(SandboxPolicy::default());
    #[cfg(not(target_os = "linux"))]
    let bash = BashTool::new();
    let read = ReadFileTool;
    let write = WriteFileTool;
    let list = ListFilesTool;
    let search = WebSearchTool;
    let mem = MemoryTool::new(memory_store);
    let mut tools: Vec<&dyn Tool> = vec![&bash, &read, &write, &list, &search, &mem];
    let plugin_tools = registry.tools();
    tools.extend(&plugin_tools);

    if let Some(rpm) = cli.rpm {
        let client = RateLimitedClient::new(client, rpm);
        let status_rx = client.status_rx();
        let repl = Repl::new(&client, &config, tools);
        agenty_tui::run_with_rate_limit(repl, status_rx).await
    } else {
        let repl = Repl::new(&client, &config, tools);
        agenty_tui::run(repl).await
    }
}
