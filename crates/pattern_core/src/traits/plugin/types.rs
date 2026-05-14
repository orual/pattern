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
    /// Mount path this plugin instance is scoped to. Plugins spawned by a
    /// session-open at mount M get `mount_path = Some(M)`; ambient/global
    /// fixtures get `None`. Plugins that dial back to the daemon's TUI channel
    /// (e.g. for `DaemonClient::subscribe_all`) use this to identify their
    /// mount-scoped event stream.
    pub mount_path: Option<std::path::PathBuf>,
    /// Memory store for persisting skill blocks and other plugin data.
    pub memory_store: Option<Arc<dyn crate::traits::MemoryStore>>,
    /// Default scope for memory operations.
    pub scope: Option<crate::types::memory_types::Scope>,
}

impl PluginContext {
    /// Build a minimal plugin context — used by SDK guest-side when converting
    /// `WirePluginContext` into a local `PluginContext` for an OOP plugin's
    /// lifecycle methods. Memory store + scope default to `None`; the plugin
    /// reaches those via `HostApi` (host-protocol) calls instead.
    ///
    /// Handles the `memory` feature gate internally so downstream crates
    /// don't have to mirror the cfg attribute at every construction site.
    pub fn minimal(
        plugin_id: PluginId,
        hook_bus: Arc<HookBus>,
        plugin_root: std::path::PathBuf,
    ) -> Self {
        Self {
            plugin_id,
            hook_bus,
            plugin_root,
            mount_path: None,
            memory_store: None,
            scope: None,
        }
    }
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
    HookHandlerFailed { plugin_id: SmolStr, message: String },

    #[error("{0}")]
    Other(String),
}
