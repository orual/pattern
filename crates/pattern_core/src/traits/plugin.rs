//! Plugin trait boundary.
//!
//! `PluginExtension` is the runtime-facing trait every plugin implements.
//! `HostApi` is the trait plugins call back into the runtime through — the
//! make host calls (memory access, messaging, etc.).
//! `PluginContext` carries the runtime context passed to lifecycle methods.

pub mod extension;
pub mod host;
pub mod types;
pub mod wire;

pub use extension::PluginExtension;
pub use host::HostApi;
pub use types::{PluginContext, PluginError};
