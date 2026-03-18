pub use agenty_tools::Tool;

pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;

    fn description(&self) -> &str;

    fn tools(&self) -> Vec<&dyn Tool>;
}
