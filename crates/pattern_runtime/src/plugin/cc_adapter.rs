//! CC (Claude Code) plugin adapter.
//!
//! Wraps a CC plugin directory as a `PluginExtension` implementation.
//! Translates CC event names → Pattern hook tags, CC SKILL.md → Pattern
//! skill blocks, CC command/http hooks → Pattern hook subscribers.

pub mod hooks;
pub mod lifecycle;
pub mod mcp_config;
pub mod skills;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::task::JoinHandle;

use pattern_core::hooks::event::{HookEvent, HookResponse};
use pattern_core::traits::plugin::{
    PluginContext, PluginError, PluginExtension, PortDeclaration,
};
use pattern_core::plugin::manifest::PluginManifest;

/// Adapter that wraps a CC plugin directory as a PluginExtension.
///
/// CC plugins are event-driven (no host callbacks). The adapter translates
/// CC event names to Pattern hook tags and dispatches command/http hooks.
#[derive(Debug)]
pub struct CcPluginAdapter {
    pub(crate) plugin_id: smol_str::SmolStr,
    pub(crate) plugin_root: PathBuf,
    pub(crate) manifest: PluginManifest,
    state: RwLock<AdapterState>,
}

#[derive(Debug, Default)]
struct AdapterState {
    hook_drain_tasks: Vec<JoinHandle<()>>,
    enabled: bool,
}

impl CcPluginAdapter {
    /// Wrap a CC plugin directory as a PluginExtension.
    pub fn wrap(
        plugin_id: smol_str::SmolStr,
        plugin_root: PathBuf,
        manifest: PluginManifest,
    ) -> Arc<Self> {
        Arc::new(Self {
            plugin_id,
            plugin_root,
            manifest,
            state: RwLock::new(AdapterState::default()),
        })
    }
}

#[async_trait]
impl PluginExtension for CcPluginAdapter {
    fn ports(&self) -> Vec<PortDeclaration> {
        Vec::new()
    }

    async fn on_install(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        // Skills are loaded at on_enable (session context has memory store).
        // Install just stages files.
        Ok(())
    }

    async fn on_enable(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        // Load skills into memory (needs the session's memory store).
        skills::install_skills(&self.plugin_id, &self.plugin_root, &self.manifest, ctx).await?;
        let tasks = hooks::wire_hook_subscriptions(self, ctx).await?;
        let mut state = self.state.write();
        state.hook_drain_tasks = tasks;
        state.enabled = true;
        Ok(())
    }

    async fn on_disable(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        let mut state = self.state.write();
        for task in state.hook_drain_tasks.drain(..) {
            task.abort();
        }
        state.enabled = false;
        Ok(())
    }

    fn on_event(&self, _event: &HookEvent) -> Option<HookResponse> {
        // CC adapter uses subscription receivers (wired in on_enable),
        // not centralized on_event dispatch.
        None
    }
}
