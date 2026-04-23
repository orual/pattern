//! Pattern CLI library target.
//!
//! Exposes internal modules for integration testing. The binary entry point
//! is `main.rs`; this file creates a library target alongside it so that
//! `tests/` can import modules by path without duplicating code.

pub mod commands;
pub mod tui;
