//! Plugin registry: discovery, pin state, install/uninstall.
//!
//! Uses `PatternPaths` for directory resolution and `knus` for KDL
//! registry file persistence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use smol_str::SmolStr;

use pattern_core::plugin::manifest::PluginManifest;
use pattern_core::plugin::scope::PluginScope;
use pattern_core::plugin::PluginId;
use pattern_core::CapabilitySet;

use pattern_core::plugin::RegistryError;

/// A plugin loaded into the registry with its resolved scope and config.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    /// The plugin's identifier.
    pub id: PluginId,
    /// Where this plugin was discovered/pinned.
    pub scope: PluginScope,
    /// Path to the plugin's directory on disk.
    pub source_path: PathBuf,
    /// The parsed manifest.
    pub manifest: PluginManifest,
    /// User-level configuration values.
    pub user_config: serde_json::Value,
    /// Capability overrides from the registry.
    pub capability_overrides: Option<CapabilitySet>,
}

// ---- KDL persistence types (knus::Decode) -----------------------------------

/// A single plugin installation entry in a registry KDL file.
#[derive(Debug, Clone, knus::Decode)]
pub struct PluginInstallation {
    #[knus(argument)]
    pub id: String,
    #[knus(child, unwrap(argument), default)]
    pub source: Option<String>,
    #[knus(child, unwrap(argument), default)]
    pub installed_at: Option<String>,
    #[knus(child)]
    pub user_config: Option<UserConfigBlock>,
    #[knus(child)]
    pub capability_override: Option<CapabilitiesBlock>,
}

/// User-configurable values from the registry KDL.
#[derive(Debug, Clone, knus::Decode)]
pub struct UserConfigBlock {
    #[knus(children)]
    pub entries: Vec<UserConfigEntry>,
}

/// A single user config key-value entry.
#[derive(Debug, Clone, knus::Decode)]
pub struct UserConfigEntry {
    #[knus(node_name)]
    pub key: String,
    #[knus(argument)]
    pub value: String,
}

/// Capability override block from the registry KDL.
#[derive(Debug, Clone, knus::Decode)]
pub struct CapabilitiesBlock {
    #[knus(children)]
    pub effects: Vec<EffectEntry>,
}

/// A single effect category entry.
#[derive(Debug, Clone, knus::Decode)]
pub struct EffectEntry {
    #[knus(node_name)]
    pub name: String,
}

/// Top-level registry file structure.
#[derive(Debug, Clone, knus::Decode)]
pub struct RegistryFile {
    #[knus(children(name = "plugin"))]
    pub plugins: Vec<PluginInstallation>,
}

/// Hook emission callback seam. Phase 1: no-op. Phase 2: wired to real bus.
pub type HookEmitter = Box<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// Registry of all discovered and pinned plugins.
pub struct PluginRegistry {
    paths: Arc<pattern_memory::paths::PatternPaths>,
    mount_path: Option<PathBuf>,
    inner: RwLock<HashMap<PluginId, LoadedPlugin>>,
    hook_emit: HookEmitter,
}

impl std::fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRegistry")
            .field("mount_path", &self.mount_path)
            .field("plugin_count", &self.inner.read().len())
            .finish_non_exhaustive()
    }
}

impl PluginRegistry {
    /// Create an empty registry.
    pub fn new(
        paths: Arc<pattern_memory::paths::PatternPaths>,
        mount_path: Option<PathBuf>,
    ) -> Self {
        Self {
            paths,
            mount_path,
            inner: RwLock::new(HashMap::new()),
            hook_emit: Box::new(|_, _| {}),
        }
    }

    /// Swap the hook emitter (Phase 2 integration point).
    pub fn with_hook_emitter(mut self, emitter: HookEmitter) -> Self {
        self.hook_emit = emitter;
        self
    }

    /// Number of loaded plugins.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a loaded plugin by id.
    pub fn get(&self, id: &str) -> Option<LoadedPlugin> {
        self.inner.read().get(id).cloned()
    }

    /// List all loaded plugins.
    pub fn list(&self) -> Vec<LoadedPlugin> {
        self.inner.read().values().cloned().collect()
    }

    /// Read and parse a registry KDL file.
    pub fn read_registry_file(path: &Path) -> Result<Option<RegistryFile>, RegistryError> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path).map_err(|source| RegistryError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let file: RegistryFile =
            knus::parse("<registry>", &raw).map_err(|e| RegistryError::Kdl {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;
        Ok(Some(file))
    }

    /// Insert a plugin into the in-memory registry.
    pub fn insert(&self, plugin: LoadedPlugin) {
        let id = plugin.id.clone();
        self.inner.write().insert(id.clone(), plugin);
        (self.hook_emit)(
            "plugin.registered",
            serde_json::json!({ "id": id }),
        );
    }

    /// Remove a plugin from the in-memory registry.
    pub fn remove(&self, id: &str) -> Option<LoadedPlugin> {
        let removed = self.inner.write().remove(id);
        if removed.is_some() {
            (self.hook_emit)(
                "plugin.unregistered",
                serde_json::json!({ "id": id }),
            );
        }
        removed
    }
}
