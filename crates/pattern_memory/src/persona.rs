// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Persona discovery across global and project scopes.
//!
//! Scans `<pattern_home>/personas/@<name>/persona.kdl` (global) and
//! `<mount>/personas/@<name>/persona.kdl` (project-scoped) directories.
//! Project-scoped personas take precedence on name collision.
//!
//! # Module layout
//!
//! - `persona.rs` — this file; re-exports public API.
//! - `persona/discover.rs` — [`discover_personas`] scan + error types.

mod discover;

pub use discover::{PersonaDiscoveryError, PersonaIndex, discover_personas};
