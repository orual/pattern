//! Plugin subsystem: manifest parsing, registry, install/uninstall lifecycle.
//!
//! Domain types live in `pattern_core::plugin`. This module provides:
//! - KDL and CC JSON manifest parsers (file I/O + parsing)
//! - `PluginRegistry` for discovery, install, uninstall

pub mod manifest;
pub mod registry;

// Re-export core types for convenience.
pub use pattern_core::plugin::{ManifestError, PluginError, PluginId, PluginScope, RegistryError};
pub use pattern_core::plugin::manifest::PluginManifest;
pub use registry::{LoadedPlugin, PluginRegistry};
