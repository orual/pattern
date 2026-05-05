//! CC hook subscription wiring.
//!
//! Reads the CC manifest's hook declarations, translates CC event names
//! to Pattern tags via the alias map, and subscribes notification
//! receivers on the HookBus. Each receiver spawns a drain task that
//! dispatches to the appropriate hook handler (command or http).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::task::JoinHandle;
use tracing::{debug, warn, info};

use pattern_core::hooks::{HookEvent, HookFilter};
use pattern_core::hooks::cc_aliases;
use pattern_core::plugin::manifest::{ComponentSpec, PluginManifest};
use pattern_core::traits::plugin::{PluginContext, PluginError};

use super::CcPluginAdapter;

/// A parsed CC hook declaration.
#[derive(Debug, Clone)]
struct CcHookDecl {
    /// CC event name (e.g. "PreToolUse", "PostToolUse").
    event: String,
    /// Optional tool matcher pattern (e.g. "Write|Edit").
    matcher: Option<String>,
    /// What to do when the hook fires.
    handler: HookHandler,
}

/// What a CC hook does when it fires.
#[derive(Debug, Clone)]
enum HookHandler {
    /// Shell out to a command.
    Command {
        command: String,
        env: BTreeMap<String, String>,
    },
    /// POST to an HTTP endpoint.
    Http {
        url: String,
        method: Option<String>,
    },
    /// Recognized but not yet supported hook type.
    Skipped {
        original_type: String,
        reason: String,
    },
}

/// Wire hook subscriptions for a CC plugin's declared hooks.
pub async fn wire_hook_subscriptions(
    adapter: &CcPluginAdapter,
    ctx: &PluginContext,
) -> Result<Vec<JoinHandle<()>>, PluginError> {
    let mut tasks = Vec::new();

    let hook_decls = parse_cc_hook_declarations(&adapter.manifest, &adapter.plugin_root)?;

    if hook_decls.is_empty() {
        debug!(plugin = %adapter.plugin_id, "no CC hook declarations found");
        return Ok(tasks);
    }

    for decl in hook_decls {
        let pattern_tag = match cc_aliases::translate_cc(&decl.event) {
            Some(t) => t,
            None => {
                warn!(
                    plugin = %adapter.plugin_id,
                    cc_event = %decl.event,
                    "unknown CC event name; hook skipped"
                );
                continue;
            }
        };

        let filter = HookFilter::new(pattern_tag).map_err(|e| PluginError::HookHandlerFailed {
            plugin_id: adapter.plugin_id.clone(),
            message: format!("invalid hook filter: {e}"),
        })?;

        let (_sub_id, mut rx) = ctx.hook_bus.subscribe_notifications(filter);

        let handler = decl.handler.clone();
        let plugin_id = adapter.plugin_id.clone();
        let plugin_root = adapter.plugin_root.clone();

        let task = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match &handler {
                    HookHandler::Command { command, env } => {
                        match run_command_hook(&plugin_id, &plugin_root, command, env, &event) {
                            Ok(output) => {
                                debug!(
                                    plugin = %plugin_id,
                                    command = %command,
                                    "command hook executed successfully"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    plugin = %plugin_id,
                                    command = %command,
                                    error = %e,
                                    "command hook execution failed"
                                );
                            }
                        }
                    }
                    HookHandler::Http { url, method } => {
                        debug!(
                            plugin = %plugin_id,
                            url = %url,
                            "http hooks not yet implemented"
                        );
                    }
                    HookHandler::Skipped { original_type, reason } => {
                        debug!(
                            plugin = %plugin_id,
                            hook_type = %original_type,
                            reason = %reason,
                            "hook type not yet supported"
                        );
                    }
                }
            }
            debug!(plugin = %plugin_id, "hook subscription receiver closed");
        });

        tasks.push(task);
    }

    info!(
        plugin = %adapter.plugin_id,
        hook_count = tasks.len(),
        "wired CC hook subscriptions"
    );

    Ok(tasks)
}

/// Parse CC hook declarations from the manifest.
fn parse_cc_hook_declarations(
    manifest: &PluginManifest,
    _plugin_root: &Path,
) -> Result<Vec<CcHookDecl>, PluginError> {
    let mut decls = Vec::new();

    for spec in &manifest.hooks {
        match spec {
            ComponentSpec::Inline(value) => {
                // CC hooks are inline JSON objects.
                if let Some(hooks_array) = value.get("hooks").and_then(|v| v.as_array()) {
                    let matcher = value
                        .get("matcher")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    for hook in hooks_array {
                        let event = hook
                            .get("event")
                            .and_then(|v| v.as_str())
                            .unwrap_or("PostToolUse")
                            .to_string();

                        let hook_type = hook
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("command");

                        let handler = match hook_type {
                            "command" => {
                                let command = hook
                                    .get("command")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                HookHandler::Command {
                                    command,
                                    env: BTreeMap::new(),
                                }
                            }
                            "http" => {
                                let url = hook
                                    .get("url")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let method = hook
                                    .get("method")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                HookHandler::Http { url, method }
                            }
                            other => HookHandler::Skipped {
                                original_type: other.to_string(),
                                reason: "not yet implemented".to_string(),
                            },
                        };

                        decls.push(CcHookDecl {
                            event,
                            matcher: matcher.clone(),
                            handler,
                        });
                    }
                }
            }
            ComponentSpec::Path(path) => {
                // TODO: read hooks from <plugin_root>/<path>/hooks.json
                debug!(path = %path.display(), "path-based hook declarations not yet supported");
            }
            _ => {}
        }
    }

    Ok(decls)
}

/// Execute a command hook synchronously.
fn run_command_hook(
    plugin_id: &str,
    plugin_root: &Path,
    command: &str,
    env: &BTreeMap<String, String>,
    event: &HookEvent,
) -> Result<String, std::io::Error> {
    // Expand $CLAUDE_PLUGIN_ROOT in the command string.
    let expanded = command.replace("${CLAUDE_PLUGIN_ROOT}", &plugin_root.to_string_lossy());
    let expanded = expanded.replace("$CLAUDE_PLUGIN_ROOT", &plugin_root.to_string_lossy());

    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(&expanded);
    cmd.current_dir(plugin_root);

    // Set CC-compatible environment variables.
    cmd.env("CLAUDE_PLUGIN_ROOT", plugin_root);
    for (k, v) in env {
        cmd.env(k, v);
    }

    let output = cmd.output()?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
