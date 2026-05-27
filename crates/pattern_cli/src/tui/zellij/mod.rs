// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Zellij integration: detection, layout generation, and pane spawning.
//!
//! At TUI startup, [`detect::detect`] determines the zellij environment state.
//! Depending on the result, `main.rs` either auto-launches into a new session,
//! starts normally (already in a session), or runs standalone (no zellij).
//!
//! Once running inside a session, the `/pane` and `/float` commands use
//! [`pane`] to spawn additional agent REPLs in new zellij panes.

pub mod detect;
pub mod layout;
pub mod pane;
pub mod session;

/// Locate the currently-running `pattern` binary as an absolute path string.
///
/// Spawned zellij panes and the rendered layout reference the pattern binary
/// by path so they invoke the same build as the caller. This matters during
/// development when `pattern` is not on `PATH` (typically launched from
/// `target/debug/pattern` or via `cargo run`). Falls back to the bare name
/// `"pattern"` (PATH lookup) if `std::env::current_exe()` fails.
pub fn locate_pattern_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "pattern".to_string())
}
