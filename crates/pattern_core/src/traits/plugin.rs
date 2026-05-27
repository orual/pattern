// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Plugin trait boundary.
//!
//! `PluginExtension` is the runtime-facing trait every plugin implements.
//! `HostApi` is the trait plugins call back into the runtime through — the
//! make host calls (memory access, messaging, etc.).
//! `PluginContext` carries the runtime context passed to lifecycle methods.

pub mod extension;
pub mod host;
pub mod types;
pub mod wire;

pub use extension::PluginExtension;
pub use host::HostApi;
pub use types::{PluginContext, PluginError};
