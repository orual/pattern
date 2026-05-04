//! CC hook subscription wiring.

use std::sync::Arc;

use tokio::task::JoinHandle;

use pattern_core::traits::plugin::{PluginContext, PluginError};

use super::CcPluginAdapter;

/// Wire hook subscriptions for a CC plugin's declared hooks.
///
/// Reads the CC manifest's hook declarations, translates CC event names
/// to Pattern tags via the CC alias map, and subscribes notification
/// receivers on the HookBus. Each receiver spawns a drain task that
/// dispatches to the hook handler (command or http).
pub async fn wire_hook_subscriptions(
    _adapter: &CcPluginAdapter,
    _ctx: &PluginContext,
) -> Result<Vec<JoinHandle<()>>, PluginError> {
    // TODO: Task 5 implements this.
    // For each hook in manifest.hooks:
    //   1. Translate CC event name → Pattern tag via cc_aliases
    //   2. Subscribe a notification receiver on ctx.hook_bus
    //   3. Spawn a drain task that dispatches to command/http handler
    Ok(Vec::new())
}
