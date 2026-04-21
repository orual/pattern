//! Error hierarchy for pattern-core.
//!
//! The top-level [`CoreError`] wraps three domain-specific sub-errors via
//! `#[from]` conversions:
//!
//! | Sub-error | Covers |
//! |-----------|--------|
//! | [`RuntimeError`] | agent-loop execution (timeouts, crashes, checkpoints) |
//! | [`ProviderError`] | external LLM providers and credential store |
//! | [`MemoryError`] | memory block storage and retrieval |
//!
//! All other concerns (tool dispatch, serialization, config, I/O, database)
//! remain as direct variants on [`CoreError`].
//!
//! # Examples
//!
//! ```
//! use pattern_core::error::{CoreError, MemoryError, ProviderError, RuntimeError};
//! use pattern_core::types::block::BlockHandle;
//! use std::time::Duration;
//!
//! // Sub-errors convert into CoreError via From.
//! let mem_err = MemoryError::BlockNotFound {
//!     handle: BlockHandle::new("persona"),
//!     available: vec![],
//! };
//! let core_err: CoreError = mem_err.into();
//! assert!(core_err.to_string().contains("persona"));
//!
//! let prov_err = ProviderError::RateLimited { retry_after: Duration::from_secs(60) };
//! let core_err: CoreError = prov_err.into();
//! assert!(core_err.to_string().contains("rate limited"));
//!
//! let rt_err = RuntimeError::Timeout {
//!     wall_ms: 5000,
//!     cpu_ms: 1000,
//!     path: pattern_core::error::CancelPath::Soft,
//! };
//! let core_err: CoreError = rt_err.into();
//! assert!(core_err.to_string().contains("timed out"));
//! ```

mod core;
pub mod embedding;
pub(crate) mod memory;
mod provider;
mod runtime;

pub use core::{ConfigError, CoreError};
pub use embedding::EmbeddingError;
pub use memory::{MemoryError, MemoryResult};
pub use provider::ProviderError;
pub use runtime::{CancelPath, RuntimeError, SandboxConstraint};

/// Convenience `Result` alias using [`CoreError`] as the error type.
///
/// All public pattern-core APIs should use `Result<T>` rather than
/// `std::result::Result<T, CoreError>` for brevity.
pub type Result<T> = std::result::Result<T, CoreError>;
