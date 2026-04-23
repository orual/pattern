//! jj CLI adapter for Pattern's memory subsystem.
//!
//! Provides a thin wrapper over the user's installed `jj` binary. Pattern
//! shells out to `jj` for all VCS operations rather than linking against
//! `jj-lib`, to ensure on-disk format ownership stays with whichever `jj`
//! binary the user has installed. See `docs/implementation-plans/2026-04-19-v3-memory-rework/phase_05.md`
//! for the full decision record.
//!
//! # Entry point
//!
//! ```no_run
//! use pattern_memory::jj::JjAdapter;
//!
//! match JjAdapter::detect() {
//!     Ok(Some(adapter)) => {
//!         // jj is available and the version is supported
//!         println!("jj {}", adapter.version());
//!     }
//!     Ok(None) => {
//!         // jj is not on PATH; InRepo mode continues without it
//!     }
//!     Err(e) => {
//!         // jj is present but the version is not supported, or another probe error
//!         eprintln!("jj detection failed: {e}");
//!     }
//! }
//! ```
//!
//! # Module layout
//!
//! - [`adapter`] — [`JjAdapter`] struct + all adapter functions
//! - [`error`] — [`JjError`] and [`JjResult`] type alias
//! - [`templates`] — template string constants
//! - [`types`] — output structs deserialized from jj JSON output
//! - [`version`] — version parsing + supported-range constants

pub mod adapter;
pub mod error;
pub mod templates;
pub mod types;
pub mod version;

pub use adapter::JjAdapter;
pub use error::{JjError, JjResult};
