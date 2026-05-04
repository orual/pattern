//! The `PluginExtension` trait — runtime-facing plugin contract.

use async_trait::async_trait;

use crate::hooks::event::{HookEvent, HookResponse};
use super::types::{PluginContext, PluginError, PortDeclaration};

/// Plugin trait. Every plugin — native IRPC, CC adapter, MCP adapter —
/// implements this.
///
/// Lifecycle methods (`on_install`, `on_enable`, `on_disable`) are async
/// because plugin code may await network/IO. Event dispatch (`on_event`)
/// is sync — it operates against an already-extracted `HookEvent` payload.
#[async_trait]
pub trait PluginExtension: Send + Sync + std::fmt::Debug {
    /// What ports/tools this plugin provides.
    fn ports(&self) -> Vec<PortDeclaration> {
        Vec::new()
    }

    /// Optional Haskell library text spliced into agent prelude when enabled.
    fn library(&self) -> Option<&str> {
        None
    }

    /// Lifecycle: install. Called once when added to the registry.
    async fn on_install(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let _ = ctx;
        Ok(())
    }

    /// Lifecycle: enable. Called when bound to a session/runtime context.
    async fn on_enable(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let _ = ctx;
        Ok(())
    }

    /// Lifecycle: disable. Called when detached or session ends.
    async fn on_disable(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let _ = ctx;
        Ok(())
    }

    /// Hook event handler. Called when a HookEvent matches this plugin's
    /// registered tag globs. Returns `Some(HookResponse)` for blocking events;
    /// `None` for notifications.
    fn on_event(&self, event: &HookEvent) -> Option<HookResponse> {
        let _ = event;
        None
    }
}
