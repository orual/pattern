// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! PortRegistry trait: the registry of `Port` implementations.
//!
//! Unlike Phase 3's `ProcessManager` (concrete, lives in `pattern_runtime`),
//! `PortRegistry` is split into a trait (here, in `pattern_core`) and a
//! concrete implementation (`PortRegistryImpl` in `pattern_runtime`). This
//! keeps the boundary clean for Plan 4's plugin system, which references
//! `&dyn PortRegistry` from plugin host code without pulling in runtime types.
//!
//! # Registration contract
//!
//! Duplicate registrations (same `PortId`) are **errors**, not silent
//! overwrites. `register()` returns `Err(PortError::AlreadyRegistered(id))`
//! in that case. Plugins that hot-reload must `unregister()` the prior port
//! before re-registering a new version under the same id.
//!
//! # Interior mutability
//!
//! Implementations use interior mutability (e.g., `DashMap`) so the registry
//! can be shared by reference across many call sites without threading a
//! mutable borrow through every call.

use std::sync::Arc;

use async_trait::async_trait;

use crate::traits::port::Port;
use crate::types::port::{PortError, PortId, PortMetadata};

/// Registry of [`Port`] implementations.
///
/// One registry per `TidepoolRuntime`; shared across sessions via `Arc`.
/// Runtime-provided ports register at startup; plugin-registered ports
/// (Plan 4 — v3-extensibility) register at plugin load time.
///
/// # Duplicate registration
///
/// Calling `register` with an id that is already registered returns
/// `Err(PortError::AlreadyRegistered(id))` rather than silently overwriting
/// the existing port. Hot-reload flows must `unregister` the old port first.
#[async_trait]
pub trait PortRegistry: Send + Sync {
    /// Register a port.
    ///
    /// Returns `Err(PortError::AlreadyRegistered(id))` if a port with the
    /// same id is already registered. Idempotent registration requires the
    /// caller to `unregister` first.
    async fn register(&self, port: Arc<dyn Port>) -> Result<(), PortError>;

    /// Unregister a port by id.
    ///
    /// No-op if no port with that id is registered. Any active subscriptions
    /// for the port are cancelled by the dispatcher actor.
    async fn unregister(&self, id: &PortId);

    /// List metadata for all registered ports.
    ///
    /// Used by `Port.List` to surface the available port surface area to the
    /// agent. The order of entries is unspecified.
    fn list(&self) -> Vec<PortMetadata>;

    /// Fetch a port by id.
    ///
    /// Returns `None` if no port with that id is registered. The returned
    /// `Arc` keeps the port alive for the duration of the call even if the
    /// port is concurrently unregistered.
    fn get(&self, id: &PortId) -> Option<Arc<dyn Port>>;
}
