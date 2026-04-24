//! Rust-side effect handlers.
//!
//! Phase 3 wires `time`, `log`, and `display` to fully-implemented handlers;
//! Phase 5 adds `search`, `recall`, and shared-block access on `memory`.
//! `shell`, `file`, `sources`, `mcp`, `rpc`, and `spawn` are stubbed out to
//! return an actionable `EffectError::Handler("…not yet implemented…")`.

pub mod diagnostics;
pub mod display;
pub mod file;
pub mod log;
pub mod mcp;
pub mod memory;
pub mod message;
pub mod recall;
pub mod rpc;
pub mod scope;
pub mod search;
pub mod shell;
pub mod sources;
pub mod spawn;
pub mod tasks;
pub mod time;

pub use diagnostics::DiagnosticsHandler;
pub use display::DisplayHandler;
pub use file::FileHandler;
pub use log::LogHandler;
pub use mcp::McpHandler;
pub use memory::MemoryHandler;
pub use message::MessageHandler;
pub use recall::RecallHandler;
pub use rpc::RpcHandler;
pub use search::SearchHandler;
pub use shell::ShellHandler;
pub use sources::SourcesHandler;
pub use spawn::SpawnHandler;
pub use tasks::TasksHandler;
pub use time::TimeHandler;
