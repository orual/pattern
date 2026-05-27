// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! IRPC service contract for the Pattern daemon.
//!
//! The wire types + protocol enum live in `pattern_core::wire::ui` so that
//! both this crate and pattern-plugin-sdk-consuming plugins (e.g. the
//! first-party discord plugin) reference the same canonical definitions.
//! This module is a transparent re-export shim; new code should prefer
//! `pattern_core::wire::ui::*` directly.

pub use pattern_core::wire::ui::*;
