//! Runtime-provided port implementations.
//!
//! Ports expose external services (HTTP, Slack, databases, etc.) to agents
//! through the `Port` trait. This module owns the implementations that
//! `pattern_runtime` ships — the first being [`HttpPort`]. Plugin-provided
//! ports register via the same `PortRegistry` surface but live outside this
//! crate.

pub mod http;

pub use http::HttpPort;
