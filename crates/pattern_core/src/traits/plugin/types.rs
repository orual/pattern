//! Supporting types for the plugin trait boundary.

use smol_str::SmolStr;
use std::sync::Arc;

use crate::hooks::HookBus;
use crate::plugin::PluginId;

/// Context passed to plugin lifecycle methods.
#[derive(Debug, Clone)]
pub struct PluginContext {
    /// The plugin's identifier.
    pub plugin_id: PluginId,
    /// The hook bus for subscribing to events.
    pub hook_bus: Arc<HookBus>,
    /// Root directory of the plugin on disk.
    pub plugin_root: std::path::PathBuf,
    /// Memory store for persisting skill blocks and other plugin data.
    pub memory_store: Option<Arc<dyn crate::traits::MemoryStore>>,
    /// Default scope for memory operations.
    pub scope: Option<crate::types::memory_types::Scope>,
}

/// A port/tool declaration from a plugin.
#[derive(Debug, Clone)]
pub struct PortDeclaration {
    /// Port identifier.
    pub id: SmolStr,
    /// Human-readable description.
    pub description: String,
    /// Methods this port exposes.
    pub methods: Vec<SmolStr>,
}

/// Errors from plugin operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginError {
    #[error("plugin lifecycle error: {0}")]
    Lifecycle(String),

    #[error("plugin host callback failed: {0}")]
    HostCallback(String),

    #[error("plugin IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("skill translation failed for plugin {plugin_id} at {path}: {message}")]
    SkillTranslationFailed {
        plugin_id: SmolStr,
        path: std::path::PathBuf,
        message: String,
    },

    #[error("hook handler failed for plugin {plugin_id}: {message}")]
    HookHandlerFailed {
        plugin_id: SmolStr,
        message: String,
    },

    #[error("{0}")]
    Other(String),
}
