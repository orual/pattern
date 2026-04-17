//! Rust-side effect handlers.
//!
//! Phase 3 wires `time`, `log`, and `display` to fully-implemented handlers;
//! `shell`, `file`, `sources`, `mcp`, `ipc`, and `spawn` are stubbed out to
//! return an actionable `EffectError::Handler("…not yet implemented…")`.

pub mod file;
pub mod ipc;
pub mod mcp;
pub mod shell;
pub mod sources;
pub mod spawn;
