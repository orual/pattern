//! Core trait surface for pattern_core.
//!
//! This module collects the abstract contracts every Pattern v3 component
//! implements or consumes. Concrete implementations live in sibling crates
//! (`pattern_runtime`, `pattern_provider`) or inside this crate's own
//! subsystem modules (e.g. memory storage).

pub mod agent_runtime;
pub mod data_stream;
pub mod embedding_provider;
pub mod endpoint;
pub mod endpoint_registry;
pub mod memory_store;
pub mod provider_client;
pub mod session;
pub mod source_manager;
pub mod turn_sink;

pub use agent_runtime::AgentRuntime;
pub use data_stream::{DataStream, StreamEvent};
pub use embedding_provider::EmbeddingProvider;
pub use endpoint::Endpoint;
pub use endpoint_registry::EndpointRegistry;
pub use memory_store::MemoryStore;
pub use provider_client::ProviderClient;
pub use session::Session;
pub use source_manager::{SourceManager, SourceName};
pub use turn_sink::{DisplayKind, NoOpSink, TurnEvent, TurnSink, VecSink};
