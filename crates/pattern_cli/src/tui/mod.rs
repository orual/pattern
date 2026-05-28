// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! TUI subsystem for the Pattern REPL.
//!
//! Provides a ratatui-based terminal interface with conversation rendering,
//! markdown display, virtual scrolling, and layout management.

pub mod app;
pub mod autocomplete;
pub mod commands;
pub mod constellation_view;
pub mod conversation;
pub mod input;
pub mod layout;
pub mod markdown;
pub mod model;
pub mod panel;
pub mod scroll;
pub mod status_bar;
pub mod toast;
pub mod zellij;

#[cfg(test)]
pub mod test_utils;
