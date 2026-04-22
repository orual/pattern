//! TUI subsystem for the Pattern REPL.
//!
//! Provides a ratatui-based terminal interface with conversation rendering,
//! markdown display, virtual scrolling, and layout management.

pub mod app;
pub mod autocomplete;
pub mod clipboard;
pub mod commands;
pub mod conversation;
pub mod input;
pub mod layout;
pub mod markdown;
pub mod model;
pub mod panel;
pub mod scroll;
pub mod status_bar;
pub mod toast;

#[cfg(test)]
pub mod test_utils;
