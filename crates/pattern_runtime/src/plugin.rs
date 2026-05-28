// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Plugin subsystem: manifest parsing, registry, install/uninstall lifecycle.
//!
//! Domain types live in `pattern_core::plugin`. This module provides:
//! - KDL and CC JSON manifest parsers (file I/O + parsing)
//! - `PluginRegistry` for discovery, install, uninstall

pub mod cc_adapter;
pub mod host_handler;
pub mod memory_sync_handler;
pub mod manifest;
pub mod marketplace;
pub mod registry;
pub mod transport;
pub mod wire_backed_port;

// Re-export core types for convenience.
pub use pattern_core::plugin::{ManifestError, PluginError, PluginId, PluginScope, RegistryError};
pub use pattern_core::plugin::manifest::PluginManifest;
pub use registry::{LoadedPlugin, PluginRegistry};
