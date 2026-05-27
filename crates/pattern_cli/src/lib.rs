// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Pattern CLI library target.
//!
//! Exposes internal modules for integration testing. The binary entry point
//! is `main.rs`; this file creates a library target alongside it so that
//! `tests/` can import modules by path without duplicating code.

pub mod commands;
pub mod tui;
