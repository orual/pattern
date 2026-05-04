//! CC (Claude Code) event alias map.
//!
//! Maps CC event names to Pattern hook tags. Applied at plugin-load time
//! by the CC adapter (Phase 3+). The bus itself only knows Pattern tags.

use std::collections::HashMap;

/// Build the CC → Pattern event alias map.
///
/// CC events use camelCase names; Pattern uses dot-separated hierarchical tags.
/// Some CC events map to multiple Pattern tags (variant-specific targets).
pub fn cc_alias_map() -> HashMap<&'static str, Vec<&'static str>> {
    let mut map = HashMap::new();

    // Turn lifecycle
    map.insert("onTurnStart", vec![super::tags::TURN_BEFORE]);
    map.insert("onTurnEnd", vec![super::tags::TURN_STOP]);

    // Tool dispatch
    map.insert("onToolCall", vec![super::tags::TOOL_BEFORE]);
    map.insert("onToolResult", vec![super::tags::TOOL_AFTER]);

    // Memory
    map.insert("onMemoryRead", vec![super::tags::MEMORY_READ]);
    map.insert("onMemoryWrite", vec![super::tags::MEMORY_WRITE]);

    // Shell
    map.insert("onShellExecute", vec![super::tags::SHELL_EXECUTE_BEFORE]);
    map.insert("onShellResult", vec![super::tags::SHELL_EXECUTE_AFTER]);

    // Tasks
    map.insert("onTaskCreated", vec![super::tags::TASK_CREATED]);
    map.insert("onTaskCompleted", vec![super::tags::TASK_TRANSITIONED_DONE]);

    // File
    map.insert("onFileRead", vec![super::tags::FILE_READ]);
    map.insert("onFileWrite", vec![super::tags::FILE_WRITE]);

    // Spawn
    map.insert("onAgentSpawn", vec![super::tags::SPAWN_EPHEMERAL_START]);
    map.insert("onAgentExit", vec![super::tags::SPAWN_EPHEMERAL_EXIT]);

    // Session
    map.insert("onSessionStart", vec![super::tags::SESSION_OPENED]);
    map.insert("onSessionEnd", vec![super::tags::SESSION_CLOSED]);

    // Message
    map.insert("onMessageSent", vec![super::tags::MESSAGE_SENT]);
    map.insert("onMessageReceived", vec![super::tags::MESSAGE_RECEIVED]);

    map
}
