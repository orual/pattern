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

    #[error("{0}")]
    Other(String),
}
