// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Core trait surface for pattern_core.
//!
//! This module collects the abstract contracts every Pattern v3 component
//! implements or consumes. Concrete implementations live in sibling crates
//! (`pattern_runtime`, `pattern_provider`) or inside this crate's own
//! subsystem modules (e.g. memory storage).

#[cfg(feature = "provider")]
pub mod agent_runtime;
pub mod embedding_provider;
#[cfg(feature = "provider")]
pub mod endpoint;
#[cfg(feature = "provider")]
pub mod endpoint_registry;
pub mod memory_store;
pub mod plugin;
pub mod port;
pub mod port_registry;
#[cfg(feature = "provider")]
pub mod provider_client;
#[cfg(feature = "provider")]
pub mod session;
#[cfg(feature = "provider")]
pub mod turn_sink;

#[cfg(feature = "provider")]
pub use agent_runtime::AgentRuntime;
pub use embedding_provider::EmbeddingProvider;
#[cfg(feature = "provider")]
pub use endpoint::Endpoint;
#[cfg(feature = "provider")]
pub use endpoint_registry::EndpointRegistry;
pub use memory_store::MemoryStore;
pub use port::Port;
pub use port_registry::PortRegistry;
#[cfg(feature = "provider")]
pub use provider_client::ProviderClient;
#[cfg(feature = "provider")]
pub use session::Session;
#[cfg(feature = "provider")]
pub mod spawn_sink_factory;
#[cfg(feature = "provider")]
pub use spawn_sink_factory::SpawnSinkFactory;
#[cfg(feature = "provider")]
pub use turn_sink::{DisplayKind, NoOpSink, TurnEvent, TurnSink, VecSink};
