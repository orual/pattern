// MOVING TO: pattern_runtime/src/sources/process/mod.rs
// ORIGIN: crates/pattern_core/src/data_source/process/mod.rs
// PHASE: 3
// RESHAPE: Trait surface extracted in phase 2; concrete backends reshape here
//
// This file is retained verbatim for reference during the v3 foundation rewrite.
// It does not compile in this location; rewrite-staging/ is not a cargo workspace member.

//! Process execution data source.
//!
//! Provides shell command execution capability through:
//! - [`ProcessSource`]: DataStream impl managing process lifecycles
//! - [`ShellBackend`]: Trait for swappable execution backends
//! - [`LocalPtyBackend`]: PTY-based local execution
//! - [`CommandValidator`]: Permission validation for commands
//!
//! # Example
//!
//! ```ignore
//! use pattern_core::data_source::process::{
//!     ProcessSource, LocalPtyBackend, ShellPermissionConfig, ShellPermission
//! };
//! use std::sync::Arc;
//! use std::path::PathBuf;
//!
//! // Create a process source with local PTY backend
//! let source = ProcessSource::with_local_backend(
//!     "shell",
//!     PathBuf::from("/tmp"),
//!     ShellPermissionConfig::new(ShellPermission::ReadWrite),
//! );
//!
//! // Start the source (requires agent context)
//! // let rx = source.start(ctx, owner).await?;
//!
//! // Execute a command
//! // let result = source.execute("echo hello", Duration::from_secs(5)).await?;
//! ```

mod backend;
mod error;
mod local_pty;
mod permission;
mod source;

pub use backend::{ExecuteResult, OutputChunk, ShellBackend, TaskId};
pub use error::{ShellError, ShellPermission};
pub use local_pty::LocalPtyBackend;
pub use permission::{CommandValidator, DefaultCommandValidator, ShellPermissionConfig};
pub use source::{ProcessSource, ProcessStatus};

#[cfg(test)]
mod tests;
