pub use agenty_core::{JsonValue, ToolCall, ToolResult};

mod bash;
mod list_files;
mod memory;
mod read_file;
pub mod sandbox;
mod web_search;
mod write_file;

pub use bash::BashTool;
pub use list_files::ListFilesTool;
pub use memory::MemoryTool;
pub use read_file::ReadFileTool;
pub use web_search::WebSearchTool;
pub use write_file::WriteFileTool;

/// A tool the agent can invoke.
///
/// Tools declare a name, a human-readable description (used as the LLM-facing
/// prompt), and a JSON-Schema describing their input. Execution takes a JSON
/// input value and returns either a JSON success payload or a plain-text
/// error message.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> JsonValue;
    fn execute(&self, input: JsonValue) -> Result<JsonValue, String>;
}

/// Run `tool` against a [`ToolCall`] and package the outcome as a [`ToolResult`].
pub fn invoke(tool: &dyn Tool, call: &ToolCall) -> ToolResult {
    match tool.execute(call.input.clone()) {
        Ok(value) => ToolResult::success(&call.id, value),
        Err(message) => ToolResult::error(&call.id, message),
    }
}
