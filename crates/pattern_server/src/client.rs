//! TUI/UI client for the pattern daemon.
//!
//! The canonical client lives in pattern-plugin-sdk's `tui_channel` module so
//! that plugins authoring TUI-shaped surfaces consume the same types and
//! helpers the TUI does. This module is a transparent re-export shim.

pub use pattern_plugin_sdk::tui_channel::*;
