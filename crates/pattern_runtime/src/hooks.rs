//! Runtime-side hook helpers.
//!
//! The core types live in `pattern_core::hooks`. This module provides
//! runtime-specific helpers for emitting events from handler code.

pub mod metadata;

pub use metadata::build_metadata;
