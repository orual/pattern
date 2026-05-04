//! The `PluginHost` trait — runtime → plugin callback contract.
//!
//! Method signatures mirror `PluginProtocol`'s host-callback variants
//! one-for-one. Two real implementations (Phase 6):
//! - `RuntimePluginHost`: wraps the actual runtime handles
//! - `IrpcPluginHost`: wraps an irpc::Client for out-of-process plugins
//!
//! CC adapter holds `host: None` — CC plugins never make host callbacks.

use async_trait::async_trait;
use smol_str::SmolStr;

use super::types::PluginError;

/// Plugin → runtime callback trait.
///
/// Plugins that need to read/write memory, send messages, or interact
/// with the task system do so through this trait. The runtime provides
/// a concrete implementation; out-of-process plugins get an IRPC proxy.
#[async_trait]
pub trait PluginHost: Send + Sync + std::fmt::Debug {
    /// Read a memory block's rendered content.
    async fn memory_get(&self, scope: &str, label: &str) -> Result<String, PluginError>;

    /// Write content to a memory block (upsert).
    async fn memory_put(
        &self,
        scope: &str,
        label: &str,
        content: &str,
    ) -> Result<(), PluginError>;

    /// Search memory blocks.
    async fn memory_search(&self, query: &str) -> Result<Vec<SmolStr>, PluginError>;

    /// Send a message to another agent.
    async fn send_message(
        &self,
        recipient: &str,
        body: &str,
    ) -> Result<(), PluginError>;

    /// Insert an archival entry.
    async fn archival_insert(&self, content: &str) -> Result<SmolStr, PluginError>;
}
