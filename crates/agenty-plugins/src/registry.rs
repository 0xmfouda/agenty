//! Central store for loaded plugins and their tools.

use std::path::Path;

use crate::{Plugin, ScriptPlugin, Tool};

/// Holds all loaded plugins and provides a flat view of their tools.
pub struct PluginRegistry {
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// Register a plugin manually (useful for built-in Rust plugins).
    pub fn register(&mut self, plugin: Box<dyn Plugin>) {
        self.plugins.push(plugin);
    }

    /// Scan `dir` for sub-directories that contain a `plugin.toml` manifest and
    /// load each one as a [`ScriptPlugin`].  Directories without a manifest are
    /// silently skipped.  Returns the number of plugins successfully loaded.
    pub fn discover(&mut self, dir: &Path) -> Result<usize, String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("cannot read plugin directory {}: {e}", dir.display()))?;

        let mut loaded = 0;
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest = path.join("plugin.toml");
            if !manifest.is_file() {
                continue;
            }
            match ScriptPlugin::load(&manifest) {
                Ok(plugin) => {
                    self.plugins.push(Box::new(plugin));
                    loaded += 1;
                }
                Err(e) => {
                    eprintln!("warning: skipping plugin at {}: {e}", path.display());
                }
            }
        }
        Ok(loaded)
    }

    /// All tools across every loaded plugin, in registration order.
    pub fn tools(&self) -> Vec<&dyn Tool> {
        self.plugins.iter().flat_map(|p| p.tools()).collect()
    }

    /// Number of loaded plugins.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Whether the registry has no plugins.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Iterator over loaded plugins.
    pub fn plugins(&self) -> impl Iterator<Item = &dyn Plugin> {
        self.plugins.iter().map(|p| p.as_ref())
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn empty_registry() {
        let reg = PluginRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.tools().is_empty());
    }

    #[test]
    fn discover_skips_dirs_without_manifest() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("not-a-plugin")).unwrap();

        let mut reg = PluginRegistry::new();
        let loaded = reg.discover(dir.path()).unwrap();
        assert_eq!(loaded, 0);
        assert!(reg.is_empty());
    }

    #[test]
    fn discover_loads_valid_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("greet");
        fs::create_dir(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
name = "greet"
description = "A greeting plugin"

[[tools]]
name = "say_hello"
description = "Says hello"
command = "echo hello"

[tools.input_schema]
type = "object"
"#,
        )
        .unwrap();

        let mut reg = PluginRegistry::new();
        let loaded = reg.discover(dir.path()).unwrap();
        assert_eq!(loaded, 1);
        assert_eq!(reg.len(), 1);

        let tools = reg.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "say_hello");
    }
}
