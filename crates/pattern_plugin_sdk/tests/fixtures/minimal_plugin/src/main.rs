//! Minimal plugin smoke fixture for Phase 6 Task 7.
//!
//! Compiles against `pattern-plugin-sdk` with no extra features and provides a real
//! consumer to assert the SDK's dep tree omits forbidden crates.

use pattern_plugin_sdk::{
    HookEvent, HookResponse, PluginContext, PluginError,
    PluginExtension, PortDeclaration, register_plugin, tags,
};

#[derive(Debug, Default)]
struct MinimalPlugin;

#[async_trait::async_trait]
impl PluginExtension for MinimalPlugin {
    fn ports(&self) -> Vec<PortDeclaration> { vec![] }

    async fn on_enable(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        tracing::info!("minimal plugin enabled");
        Ok(())
    }

    fn on_event(&self, event: &HookEvent) -> Option<HookResponse> {
        if event.tag == tags::TURN_BEFORE {
            tracing::debug!(tag = ?event.tag, "minimal plugin saw turn.before");
        }
        None
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let plugin_id = std::env::var("PATTERN_PLUGIN_ID")
        .unwrap_or_else(|_| "minimal_plugin".into());
    let _handle = register_plugin(plugin_id.into(), MinimalPlugin::default()).await?;
    // Block until ctrl-c; the daemon supervises via the child process.
    tokio::signal::ctrl_c().await?;
    Ok(())
}
