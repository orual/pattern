//! Plugin subsystem: manifest parsing, registry, install/uninstall lifecycle.
//!
//! Domain types live in `pattern_core::plugin`. This module provides:
//! - KDL and CC JSON manifest parsers (file I/O + parsing)
//! - `PluginRegistry` for discovery, install, uninstall

pub mod cc_adapter;
pub mod host_handler;
pub mod manifest;
pub mod registry;
pub mod transport;
pub mod wire_backed_port;

// Re-export core types for convenience.
pub use pattern_core::plugin::{ManifestError, PluginError, PluginId, PluginScope, RegistryError};
pub use pattern_core::plugin::manifest::PluginManifest;
pub use registry::{LoadedPlugin, PluginRegistry};
