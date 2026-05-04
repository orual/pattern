//! Plugin registry: discovery, pin state, install/uninstall.
//!
//! Uses `PatternPaths` for directory resolution and `JjAdapter` for
//! git clone installs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use smol_str::SmolStr;

use pattern_core::plugin::manifest::PluginManifest;
use pattern_core::plugin::scope::PluginScope;
use pattern_core::plugin::PluginId;

/// A plugin loaded into the registry with its resolved scope.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    /// The parsed manifest.
    pub manifest: PluginManifest,
    /// Where this plugin was discovered/pinned.
    pub scope: PluginScope,
    /// Path to the plugin's directory on disk.
    pub source_path: PathBuf,
    /// User-level configuration overrides.
    pub user_config: serde_json::Value,
    /// Capability overrides from the registry.
    pub capability_overrides: Option<pattern_core::CapabilitySet>,
}

/// Registry of all discovered and pinned plugins.
#[derive(Debug)]
pub struct PluginRegistry {
    inner: RwLock<HashMap<PluginId, LoadedPlugin>>,
    mount_path: Option<PathBuf>,
}

impl PluginRegistry {
    /// Create an empty registry.
    pub fn new(mount_path: Option<PathBuf>) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            mount_path,
        }
    }

    /// Number of loaded plugins.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a loaded plugin by id.
    pub fn get(&self, id: &str) -> Option<LoadedPlugin> {
        self.inner.read().unwrap().get(id).cloned()
    }

    /// List all loaded plugins.
    pub fn list(&self) -> Vec<LoadedPlugin> {
        self.inner.read().unwrap().values().cloned().collect()
    }
}
