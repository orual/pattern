// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! TUI/UI client for the pattern daemon.
//!
//! The canonical client lives in pattern-plugin-sdk's `tui_channel` module so
//! that plugins authoring TUI-shaped surfaces consume the same types and
//! helpers the TUI does. This module is a transparent re-export shim.

pub use pattern_plugin_sdk::tui_channel::*;
