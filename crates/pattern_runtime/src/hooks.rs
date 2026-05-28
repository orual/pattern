// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Runtime-side hook helpers.
//!
//! The core types live in `pattern_core::hooks`. This module provides
//! runtime-specific helpers for emitting events from handler code.

pub mod bridge;
pub mod metadata;

pub use bridge::HookBridge;
pub use metadata::build_metadata;
