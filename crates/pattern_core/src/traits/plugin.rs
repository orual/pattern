//! Plugin trait boundary.
//!
//! `PluginExtension` is the runtime-facing trait every plugin implements.
//! `PluginHost` is the runtime → plugin callback trait for plugins that
//! make host calls (memory access, messaging, etc.).
//! `PluginContext` carries the runtime context passed to lifecycle methods.

pub mod extension;
pub mod host;
pub mod types;

pub use extension::PluginExtension;
pub use host::PluginHost;
pub use types::{PluginContext, PluginError, PortDeclaration};
