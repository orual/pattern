// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Plugin types: manifest, scope, errors.
//!
//! Domain types and pure parsing logic live here.
//! I/O (file reading, registry persistence) lives in `pattern_runtime::plugin`.

pub mod error;
pub mod manifest;
pub mod scope;

#[cfg(feature = "plugin-transport")]
pub mod protocol;

#[cfg(feature = "plugin-transport")]
pub mod auth;

pub use error::{ManifestError, PluginError, RegistryError};
pub use manifest::PluginManifest;
pub use scope::PluginScope;

/// Stable plugin identifier (kebab-case, matches CC `name` field shape).
pub type PluginId = smol_str::SmolStr;
