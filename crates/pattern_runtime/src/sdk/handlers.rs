// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Rust-side effect handlers.
//!
//! Phase 3 wires `time`, `log`, and `display` to fully-implemented handlers;
//! Phase 5 adds `search`, `recall`, and shared-block access on `memory`.
//! `file`, `mcp`, and `spawn` are stubbed out to return an actionable
//! `EffectError::Handler("…not yet implemented…")`. `port` is a real Phase 4
//! handler.

pub mod constellation;
pub mod diagnostics;
pub mod display;
pub mod file;
pub mod fronting;
pub mod log;
pub mod mcp;
pub mod memory;
pub mod message;
pub mod port;
pub mod recall;
pub mod scope;
pub mod search;
pub mod shell;
pub mod skills;
pub mod spawn;
pub mod tasks;
pub mod time;
pub mod wake;
pub mod web;

pub use constellation::ConstellationHandler;
pub use diagnostics::DiagnosticsHandler;
pub use display::DisplayHandler;
pub use file::FileHandler;
pub use fronting::FrontingHandler;
pub use log::LogHandler;
pub use mcp::McpHandler;
pub use memory::MemoryHandler;
pub use message::MessageHandler;
pub use port::PortHandler;
pub use recall::RecallHandler;
pub use search::SearchHandler;
pub use shell::ShellHandler;
pub use skills::SkillsHandler;
pub use spawn::SpawnHandler;
pub use tasks::TasksHandler;
pub use time::TimeHandler;
pub use wake::WakeHandler;
pub use web::WebHandler;
