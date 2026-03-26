//! Plugin system for extending Agenty with custom tools.
//!
//! Plugins can be registered programmatically (built-in) or discovered from
//! manifest files on disk (`plugin.toml`).  Each plugin exposes zero or more
//! [`Tool`] implementations that get added to the agent's tool belt.

mod registry;
mod script;

pub use agenty_tools::Tool;
pub use registry::PluginRegistry;
pub use script::{ScriptPlugin, ScriptTool};

/// A named bundle of tools that extends the agent.
///
/// Implement this trait for built-in (Rust) plugins.  For manifest-driven
/// plugins that wrap shell commands, see [`ScriptPlugin`].
pub trait Plugin: Send + Sync {
    /// Unique plugin name (used in logs and error messages).
    fn name(&self) -> &str;

    /// One-line human-readable description.
    fn description(&self) -> &str;

    /// The tools this plugin contributes to the agent.
    fn tools(&self) -> Vec<&dyn Tool>;
}
