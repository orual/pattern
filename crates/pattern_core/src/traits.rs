//! Core trait surface for pattern_core.
//!
//! This module collects the abstract contracts every Pattern v3 component
//! implements or consumes. Concrete implementations live in sibling crates
//! (`pattern_runtime`, `pattern_provider`) or inside this crate's own
//! subsystem modules (e.g. memory storage).

pub mod agent_runtime;
pub mod memory_store;
pub mod provider_client;
pub mod session;

pub use agent_runtime::AgentRuntime;
pub use memory_store::MemoryStore;
pub use provider_client::ProviderClient;
pub use session::Session;
