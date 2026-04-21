//! TUI subsystem for the Pattern REPL.
//!
//! Provides a ratatui-based terminal interface with conversation rendering,
//! markdown display, virtual scrolling, and layout management.

pub mod app;
pub mod conversation;
pub mod layout;
pub mod markdown;
pub mod model;
pub mod scroll;

#[cfg(test)]
pub mod test_utils;
