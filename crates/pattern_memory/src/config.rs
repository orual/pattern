//! Config loading for `.pattern.kdl` mount configuration files.
//!
//! The entry point for callers is [`load_mount_config`], which reads and
//! validates a `.pattern.kdl` file at a given path, returning a typed
//! [`MountConfig`].
//!
//! # Module layout
//!
//! - `config.rs` — this file; re-exports public API.
//! - `config/pattern_kdl.rs` — typed structs + knus derive + loader.
//! - `config/error.rs` — [`ConfigError`] type.

mod error;
mod pattern_kdl;

pub use error::ConfigError;
pub use pattern_kdl::{
    IsolateSection, JjSection, ModeKind, MountConfig, MountSection, PersonaBinding,
    PersonasSection, ProjectSection, load_mount_config,
};
