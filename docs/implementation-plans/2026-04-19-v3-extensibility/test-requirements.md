# v3-extensibility Test Requirements

Maps each acceptance criterion to its test coverage. Source: design plan + 7 implementation phases.

Test-running convention: `cargo nextest run` (never `cargo test`). Doctests via `cargo test --doc` only.

## Automated tests

### AC1.1: KDL-format plugin manifest parses to `PluginManifest` with all declared fields (name, skills, agents, commands, hooks, transport, declared_effects)

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/plugin/manifest.rs` (unit, inline `#[cfg(test)] mod tests`); `crates/pattern_runtime/tests/plugin_manifest.rs` (integration)
- **Test name(s):** `kdl_full_manifest_parses_all_fields`, `kdl_and_cc_yield_equivalent_manifest`
- **Verifies:** Loading `tests/fixtures/plugins/manifest_full.kdl` populates every field on `PluginManifest`.
- **Phase(s) producing the test:** Phase 1 (Tasks 3 + 5)

### AC1.2: CC-format JSON `plugin.json` parses to the same `PluginManifest` type; normalized representation matches equivalent KDL manifest

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/plugin/manifest/cc.rs` (unit); `crates/pattern_runtime/tests/plugin_manifest.rs` (integration)
- **Test name(s):** `cc_json_translates_to_pluginmanifest`, `kdl_and_cc_yield_equivalent_manifest`
- **Verifies:** Fixture `cc_full.json` translates to a `PluginManifest` that deep-equals the matched KDL fixture (modulo `cc` field — `Some(Cc { ... })` for the JSON-translated value, `None` for the hand-authored KDL).
- **Phase(s) producing the test:** Phase 1 (Tasks 4 + 5)

### AC1.3: Unknown fields in both KDL and JSON manifests are silently ignored; parsing succeeds

*(Reinterpreted: AC says "silently ignored"; Phase 1 Task 4 design decision is "preserved structurally for forward-compat" — CC unknowns land in `manifest.cc.fields: BTreeMap<String, serde_json::Value>`; Pattern KDL unknowns land in `manifest.unknown_kdl: BTreeMap<String, KdlNode>`. Both formats round-trip; no field is dropped. Tests assert preservation per the user-clarified design.)*

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/plugin/manifest/cc.rs`; `crates/pattern_runtime/tests/plugin_manifest.rs`
- **Test name(s):** `cc_unknown_fields_preserved_under_cc_fields`, `kdl_unknown_top_level_node_preserved_in_unknown_kdl`
- **Verifies:** CC JSON unknown fields land in `manifest.cc.fields` (structured `BTreeMap`, not a serialized string blob). Pattern KDL unknown top-level nodes land in `manifest.unknown_kdl` (BTreeMap of `KdlNode`). Both round-trip cleanly.
- **Phase(s) producing the test:** Phase 1 (Task 4)

### AC1.4: Manifest missing required `name` field produces `ManifestError::MissingField("name")` with file path

- **Type:** unit
- **File:** `crates/pattern_runtime/src/plugin/manifest.rs` (unit)
- **Test name(s):** `kdl_missing_name_returns_missing_field_error_with_path`
- **Verifies:** `PluginManifest::from_kdl_file("tests/fixtures/plugins/manifest_missing_name.kdl")` returns `ManifestError::MissingField { field: "name", path }` with the path populated.
- **Phase(s) producing the test:** Phase 1 (Task 3)

### AC1.5: Manifest with only `name` and no components parses successfully (empty plugin, valid for testing/scaffolding)

- **Type:** unit
- **File:** `crates/pattern_runtime/src/plugin/manifest.rs` (unit)
- **Test name(s):** `kdl_minimal_manifest_with_only_name_parses`
- **Verifies:** Fixture `manifest_minimal.kdl` (only `name "test-plugin"`) parses; all `Vec<...>` fields empty; `cc` is `None`.
- **Phase(s) producing the test:** Phase 1 (Task 3)

### AC2.1: Plugin install clones to `~/.pattern/plugins/cache/<plugin-id>/`; registry records the installation; persisted KDL config written

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/plugin/registry.rs` (unit); `crates/pattern_runtime/tests/plugin_registry.rs` (integration)
- **Test name(s):** `install_local_path_copies_to_cache_and_records_in_registry`
- **Verifies:** After `PluginRegistry::install(InstallSource::LocalPath(...), Global, &jj)`, cache dir exists at `<base>/plugins/cache/<id>/`, registry KDL contains the entry, `registry.get(id)` returns the plugin with scope Global. Uses `tempfile::TempDir` + `PatternPaths::with_base(...)` for isolation.
- **Phase(s) producing the test:** Phase 1 (Tasks 7 + 8)

### AC2.2: After runtime restart, registry loads from persisted KDL; all previously installed plugins re-registered with their config tunables

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_registry.rs`
- **Test name(s):** `registry_survives_restart_with_config_tunables`
- **Verifies:** Install → drop `PluginRegistry` → `PluginRegistry::load(...)` against same paths → assert plugins re-loaded; `user_config` values intact.
- **Phase(s) producing the test:** Phase 1 (Task 8)

### AC2.3: Plugin uninstall removes from registry and cache; `plugin.uninstall` hook event fires

- **Type:** unit (Phase 1 seam) + integration (Phase 2 wiring)
- **File:** `crates/pattern_runtime/src/plugin/registry.rs` (unit, custom `HookEmitter` capturing emits); `crates/pattern_runtime/tests/hook_lifecycle.rs` (integration once HookBus wired)
- **Test name(s):** `uninstall_removes_plugin_and_emits_hook_event`
- **Verifies:** Custom `HookEmitter` closure captures the `plugin.uninstall` emit on uninstall; cache dir removed; KDL no longer contains entry; `registry.get` returns None.
- **Phase(s) producing the test:** Phase 1 (Task 7) for emit-seam; Phase 2 wires real `HookBus` and integration suite asserts delivery to a subscriber.

### AC2.4: Load precedence: project-scoped plugin overrides global plugin with same ID; warning logged about the override

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_registry.rs`
- **Test name(s):** `project_scope_overrides_global_with_warning_logged`
- **Verifies:** Install id at Global, then at Project; `registry.get(id).scope == Project { .. }`; `tracing-test`'s `traced_test` macro asserts a warn-level log line containing both scope names.
- **Phase(s) producing the test:** Phase 1 (Tasks 7 + 8). Adds `tracing-test = "0.2"` dev-dep if not present.

### AC2.5: Installing a plugin with a collision (same ID at same scope) produces `RegistryError::Collision` with both locations

- **Type:** unit
- **File:** `crates/pattern_runtime/src/plugin/registry.rs` (unit)
- **Test name(s):** `same_scope_collision_returns_registry_error_with_both_paths`
- **Verifies:** Install x globally, install x globally again → `RegistryError::Collision { existing_path, attempted_path, .. }` populated.
- **Phase(s) producing the test:** Phase 1 (Task 7)

### AC2.6: Plugin config tunables editable in persisted KDL between restarts; changes take effect on next load

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/plugin/registry.rs` (unit); `crates/pattern_runtime/tests/plugin_registry.rs` (integration)
- **Test name(s):** `tunables_edited_in_kdl_take_effect_on_reload`
- **Verifies:** Write registry KDL with `threshold 8`, load, assert; rewrite with `threshold 12`, reload, assert change reflected on `LoadedPlugin`.
- **Phase(s) producing the test:** Phase 1 (Tasks 6 + 8)

### AC3.1: CC-format plugin wrapped by `CcPluginAdapter`; adapter implements `PluginExtension`; runtime manages it identically to native plugins

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_cc_adapter.rs`
- **Test name(s):** `cc_plugin_routes_through_pluginextension_uniformly`
- **Verifies:** `LoadedPlugin.extension: Arc<dyn PluginExtension>` (CC adapter dispatched at install when `manifest.cc.is_some()`); `extension.ports()` and `extension.library()` callable through the trait object; lifecycle methods round-trip.
- **Phase(s) producing the test:** Phase 3 (Task 7)

### AC3.2: CC plugin's skills translated to Skill blocks with `trust_tier: PluginInstalled`; visible via `ctx.skills.list()`

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_cc_adapter.rs`
- **Test name(s):** `cc_plugin_install_translates_skills_with_plugin_installed_tier`
- **Verifies:** Install fixture CC plugin; assert two Skill blocks materialized; each carries `trust_tier: PluginInstalled` and `source_plugin_id: Some("cc-adapter-fixture")` (Phase 3) or `source: Some(SkillSource::Plugin { ... })` (after Phase 5 refactor).
- **Phase(s) producing the test:** Phase 3 (Tasks 4 + 7)

### AC3.3: CC plugin's agents translated to spawn configs; invokable via the plugin's declared interface

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/plugin/cc_adapter/agents.rs` (unit); `crates/pattern_runtime/tests/plugin_cc_adapter_full.rs` (integration)
- **Test name(s):** `cc_agent_translates_to_ephemeral_config`, `cc_full_plugin_translates_all_artifact_kinds`
- **Verifies:** `agents/refactorer.md` with `tools: [Read, Edit]` produces `EphemeralConfig` whose capabilities restrict to Memory + File. `persona_mode: draft` triggers draft KDL write at `<drafts_dir>/<plugin-id>--<agent-name>.kdl`.
- **Phase(s) producing the test:** Phase 4 (Tasks 1 + 8)

### AC3.4: CC plugin's monitors translated to Port implementations; subscribable via `ctx.port.subscribe()`

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/plugin/cc_adapter/monitors.rs` (unit); `crates/pattern_runtime/tests/plugin_cc_adapter_full.rs` (integration)
- **Test name(s):** `monitor_port_subscribe_emits_stdout_lines`, `cc_full_plugin_translates_all_artifact_kinds`
- **Verifies:** Fixture `monitors/echo-once.json` registers as `mcp-full-fixture:monitor:echo-once` port; `port.subscribe(...)` first event is `PortEvent::Line { content: "hello" }`. `on_disable` kills the long-running fixture.
- **Phase(s) producing the test:** Phase 4 (Tasks 2 + 8)

### AC3.5: CC plugin's hooks dispatch through `on_event()` with CC event alias mapping (e.g., `PreToolUse` → `tool.before`)

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_cc_adapter.rs`
- **Test name(s):** `cc_plugin_pretooluse_hook_dispatches_with_matcher_filtering`
- **Verifies:** Fixture CC plugin with `PreToolUse` hook on matcher `Write`. Emit `tool.before` event with `tool_name: "Write"` → marker file written. Emit `tool.before` with `tool_name: "Read"` → no marker (matcher rejects).
- **Phase(s) producing the test:** Phase 3 (Tasks 5 + 7)

### AC3.6: CC compatibility Haskell library included in agent prelude; maps CC terminology to pattern terminology

- **Type:** integration (haskell compile)
- **File:** `crates/pattern_runtime/tests/plugin_cc_adapter_full.rs`
- **Test name(s):** `cc_full_plugin_translates_all_artifact_kinds` (Pattern.Cc compile assertion)
- **Verifies:** Open session with one CC plugin enabled; agent program importing `Pattern.Cc` and using `Cc.preToolUse` compiles via tidepool-extract. With no CC plugins, the same import produces a `module not found` compile error (negative path also asserted).
- **Phase(s) producing the test:** Phase 4 (Tasks 7 + 8)

### AC3.7: CC adapter's host-callback surface returns `PluginError::NotDeclared` with clear message explaining CC plugins don't support host callbacks

*(Reinterpreted from literal AC text "PluginHost methods return NotSupported": Phase 3 dropped the PluginHost trait. Equivalent semantics: CC plugins declare zero `requires { ... }` resources, so accessing `ctx.memory()` returns `PluginError::NotDeclared { resource: "memory" }`.)*

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/plugin/cc_adapter.rs` (unit); `crates/pattern_runtime/tests/plugin_cc_adapter.rs` (integration)
- **Test name(s):** `cc_plugin_context_returns_not_declared_for_host_resources`
- **Verifies:** CC plugin's `PluginContext` returned at on_install has no host-callback resources declared. `ctx.memory()` returns `PluginError::NotDeclared { resource: "memory" }`; same shape for other accessors (search, send_message, task_create) — none of which CC plugins declare.
- **Phase(s) producing the test:** Phase 3 (Tasks 3 + 7)

### AC3.8: CC plugin subprocess crashes; `PluginError::ProcessDied` surfaced; plugin marked unhealthy in registry

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_transport.rs`
- **Test name(s):** `out_of_process_plugin_full_cycle` (kill-child branch)
- **Verifies:** Kill the OOP fixture process; next `connection.port_call(...)` returns `PluginError::TransportLost`/`ProcessDied`; `connection.health()` returns `Unhealthy`. CC adapter is in-process; supervised at `ProcessManager` level — assertion via Phase 4's process-spawn hook events.
- **Phase(s) producing the test:** Phase 6 (Task 8)

### AC4.1: `turn.before` hook fires before turn processing begins; hook can return a modification (e.g., prepend content) that affects the turn

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/hook_lifecycle.rs`
- **Test name(s):** `turn_before_hook_modifies_turn_content`
- **Verifies:** Blocking subscriber on `tags::TURN_BEFORE` returning `HookResponse::Modify({"prepend": "[debug] "})`; mock provider drives a turn; first user message contains the prepended content.
- **Phase(s) producing the test:** Phase 2 (Task 8 case 1)

### AC4.2: `tool.before` hook fires before tool dispatch; hook can return `HookResponse::Block` to prevent tool execution

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/hook_lifecycle.rs`
- **Test name(s):** `tool_before_block_response_prevents_dispatch`
- **Verifies:** Blocking subscriber on `tags::TOOL_BEFORE` returning `HookResponse::Block { reason: "denied for testing" }`; tool dispatch returns `EffectError::Handler` containing the block reason; assert no actual tool execution.
- **Phase(s) producing the test:** Phase 2 (Task 8 case 2)

### AC4.3: `memory.write` hook fires after a memory write completes; hook receives block handle and change summary; return value ignored (notification)

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/hook_lifecycle.rs`
- **Test name(s):** `memory_write_notification_delivered_with_payload`
- **Verifies:** Notification subscriber on `tags::MEMORY_WRITE`; `Memory.Put` via SDK handler; subscriber receives one `HookEvent` with `MemoryWritePayload { block_label, scope, write_kind, hashes }`; handler completes without waiting on subscriber.
- **Phase(s) producing the test:** Phase 2 (Task 8 case 3)

### AC4.4: CC alias mapping: hook registered as `PreToolUse` fires on `tool.before` events; hook registered as `SessionStart` fires on `persona.attach`

- **Type:** unit + integration
- **File:** `crates/pattern_core/src/hooks/cc_aliases.rs` (unit); `crates/pattern_runtime/tests/hook_lifecycle.rs` (integration)
- **Test name(s):** `cc_alias_table_round_trip`, `cc_alias_pretooluse_resolves_to_tool_before`
- **Verifies:** Round-trip every entry in the 28-entry alias table; subscribing via `cc_aliases::translate_cc("PreToolUse")` (resolves to `tags::TOOL_BEFORE`) receives a delivery on `tool.before`; same for `SessionStart` → `persona.attached`.
- **Phase(s) producing the test:** Phase 2 (Tasks 3 + 8 case 4)

### AC4.5: Hook execution exceeds timeout; hook treated as returning no response; event proceeds; warning logged

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/hook_lifecycle.rs`
- **Test name(s):** `blocking_hook_timeout_proceeds_with_warning`
- **Verifies:** `HookBus::with_timeout(Duration::from_millis(50))`; subscriber sleeps 200ms; `emit_blocking` returns `HookResponse::Continue` after ~50ms; `tracing-test` asserts warn line.
- **Phase(s) producing the test:** Phase 2 (Tasks 4 + 8 case 5)

### AC4.6: Blocking hook attempts to call an effect not in the runtime's capability set; hook's effect denied (hooks respect capability gates)

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/hook_lifecycle.rs`
- **Test name(s):** `hook_modify_response_does_not_bypass_capability_gate`
- **Verifies:** Subscriber returns `HookResponse::Modify(...)` payload that would bypass the gate in a broken impl; subsequent effect dispatch goes through `policy::evaluate(...)` which returns Deny; `EffectError::Handler` with the policy-deny prefix.
- **Phase(s) producing the test:** Phase 2 (Task 8 case 6)

### AC4.7: Multiple hooks registered for the same event fire in registration order; all complete before event proceeds (blocking) or all fire independently (notification)

- **Type:** unit + integration
- **File:** `crates/pattern_core/src/hooks/bus.rs` (unit); `crates/pattern_runtime/tests/hook_lifecycle.rs` (integration)
- **Test name(s):** `blocking_subscribers_fire_in_registration_order`, `notification_subscribers_fire_in_registration_order`
- **Verifies:** Register subs A, B, C; each pushes its id into a shared Vec; assert `[A, B, C]` order on emit (blocking); same shape for notification path.
- **Phase(s) producing the test:** Phase 2 (Tasks 4 + 8 cases 7 + 8)

### AC5.1: On MCP server load, system reminder pseudo-message injected into segment 2 containing server name + one-line per tool

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/mcp_inverted.rs`
- **Test name(s):** `mcp_inverted_surface_full_cycle` (segment 2 reminder assertion)
- **Verifies:** Load fixture echo MCP server with two tools; next batch's segment 2 contains `<system-reminder>[mcp:server-available] echo` plus tool one-liners.
- **Phase(s) producing the test:** Phase 5 (Tasks 5 + 8)

### AC5.2: On MCP server load, Working-tier blocks created at `mcp/<server>/<tool>.md` with full tool documentation; searchable via `ctx.memory.search`

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/mcp/tool_docs.rs` (unit); `crates/pattern_runtime/tests/mcp_inverted.rs` (integration)
- **Test name(s):** `mcp_tool_docs_materialize_as_skill_blocks`, `mcp_inverted_surface_full_cycle`
- **Verifies:** Load echo server; assert two Skill blocks at `mcp/echo/echo` and equivalent labels with `source: SkillSource::Mcp { server, tool }` and `trust_tier: PluginInstalled`. `MemoryStore::search("input", BlockSchema::Skill)` returns the matching block.
- **Phase(s) producing the test:** Phase 5 (Tasks 6 + 8)

### AC5.3: `ctx.mcp.call(server, method, args)` dispatches to the correct MCP server via rmcp; response returned to agent

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/sdk/handlers/mcp.rs` (unit); `crates/pattern_runtime/tests/mcp_inverted.rs` (integration)
- **Test name(s):** `mcp_call_dispatches_to_server`, `mcp_inverted_surface_full_cycle`
- **Verifies:** `McpReq::Call { server: "echo", method: "echo", args: {value: "hi"} }` dispatched to the fixture stdio server; response is `{"value":"hi"}`.
- **Phase(s) producing the test:** Phase 5 (Tasks 4 + 8)

### AC5.4: `ctx.mcp.introspect(server)` returns structured tool metadata (name, description, input schema summary) for all tools on the server

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/sdk/handlers/mcp.rs` (unit); `crates/pattern_runtime/tests/mcp_inverted.rs` (integration)
- **Test name(s):** `mcp_introspect_returns_tool_metadata`, `mcp_inverted_surface_full_cycle`
- **Verifies:** `McpReq::Introspect { server }` returns `McpIntrospection { tools: [...] }` containing all fixture tools.
- **Phase(s) producing the test:** Phase 5 (Tasks 4 + 8)

### AC5.5: `ctx.mcp.list_servers()` returns all loaded MCP servers with connection status

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/sdk/handlers/mcp.rs` (unit); `crates/pattern_runtime/tests/mcp_inverted.rs` (integration)
- **Test name(s):** `mcp_list_servers_returns_all_with_connection_state`, `mcp_inverted_surface_full_cycle`
- **Verifies:** Two registered servers, both connected; `McpReq::ListServers` returns 2 entries with `connected: true` and `tool_count` matching.
- **Phase(s) producing the test:** Phase 5 (Tasks 4 + 8)

### AC5.6: MCP server unload removes system reminder from subsequent turns and deletes tool doc blocks

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/mcp_inverted.rs`
- **Test name(s):** `mcp_unload_tears_down_reminder_and_blocks`, `mcp_inverted_surface_full_cycle`
- **Verifies:** After `Unload`, next batch's segment 2 does NOT contain the reminder; `MemoryStore::get_block("mcp/echo/echo")` returns None; FTS5 search finds no `mcp/echo/*` results.
- **Phase(s) producing the test:** Phase 5 (Tasks 4 + 5 + 6 + 8)

### AC5.7: `ctx.mcp.call` to a server not in the agent's CapabilitySet returns `CapabilityError::Denied`

- **Type:** unit + integration
- **File:** `crates/pattern_core/src/capability.rs` (unit `has_mcp_server`); `crates/pattern_runtime/tests/mcp_inverted.rs` (integration)
- **Test name(s):** `capability_set_has_mcp_server_allowlist_logic`, `mcp_call_to_disallowed_server_denied`, `mcp_inverted_surface_full_cycle`
- **Verifies:** `CapabilitySet { categories: {Mcp}, resources: {Mcp -> {"allowed-server"}} }` denies call to `denied-server` with `EffectError::Handler` containing `PERMISSION_DENIED_PREFIX`. Edge cases: empty resources entry → full access; missing Mcp category → all denied.
- **Phase(s) producing the test:** Phase 5 (Tasks 7 + 8)

### AC5.8: `ctx.mcp.call` to a disconnected server returns `McpError::ServerUnavailable` with reconnection hint

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/mcp_inverted.rs`
- **Test name(s):** `mcp_call_to_disconnected_server_returns_unavailable`, `mcp_inverted_surface_full_cycle`
- **Verifies:** Kill fixture echo server's process; subsequent `McpReq::Call` returns `EffectError::Handler` mentioning "unavailable" and the reconnect hint.
- **Phase(s) producing the test:** Phase 5 (Tasks 4 + 8)

### AC5.9: MCP server load/unload does not invalidate segment 1 cache (system prompt unchanged; only segment 2 system reminders change)

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/mcp_inverted.rs`
- **Test name(s):** `mcp_load_unload_preserves_segment1_byte_identical`, `mcp_inverted_surface_full_cycle`
- **Verifies:** Compose request before MCP load; capture segment1. Load server. Compose again. Capture segment1 after. `assert_eq!(s1_before, s1_after)` byte-identical.
- **Phase(s) producing the test:** Phase 5 (Tasks 5 + 8)

### AC5.10: MCP server stub deleted from codebase; `cargo check --workspace` passes without `pattern_mcp` in members list

- **Type:** integration (workspace build)
- **File:** CI / `just pre-commit-all` (no dedicated test file — proven by build success after Phase 7 deletes the directory)
- **Test name(s):** N/A (workspace build assertion)
- **Verifies:** `cargo check --workspace` passes after `crates/pattern_mcp/` is removed; no test imports `pattern_mcp`.
- **Phase(s) producing the test:** Phase 5 ensures workspace builds without depending on it; Phase 7 (Task 8) deletes the directory and runs the audit. Build success is the test.

### AC6.1: IRPC-native plugin registers ports via `ports()` over IRPC; pattern records the plugin's declared ports and capabilities

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_transport.rs`
- **Test name(s):** `out_of_process_plugin_full_cycle` (declare_ports assertion)
- **Verifies:** OOP fixture plugin's `connection.declare_ports()` returns expected `WirePortDeclaration`s.
- **Phase(s) producing the test:** Phase 6 (Tasks 5 + 8)

### AC6.2: Agent calls `ctx.port.call(plugin_port, method, payload)`; dispatched to plugin's port implementation over IRPC; response returned

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_transport.rs`
- **Test name(s):** `out_of_process_plugin_full_cycle` (port_call round-trip assertion)
- **Verifies:** OOP fixture plugin's `connection.port_call(port_id, "echo", json!({"value":"hello"}))` round-trips correctly via QUIC IRPC.
- **Phase(s) producing the test:** Phase 6 (Tasks 5 + 8)

### AC6.3: Plugin calls back to Pattern via `PluginContext` accessors — `ctx.memory()` returns `MemoryStore` impl, `ctx.send_message(...)` delivers to target agent, `ctx.task_create(...)` adds to TaskList

*(Reinterpreted from literal AC text "via PluginHost": Phase 3 dropped the PluginHost trait. Equivalent: `PluginContext` accessors round-trip through `PluginProtocol`/`MemorySyncProtocol` wire surfaces.)*

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_transport.rs`
- **Test name(s):** `out_of_process_plugin_full_cycle` (host-callback assertions across multiple resources)
- **Verifies:** Pre-seed `test/canary` block; OOP fixture plugin's `on_install` calls `ctx.memory()?.get_block(...)` and writes content to a marker file; assert marker contains `canary-content`. Plugin also calls `ctx.send_message(target, ...)` (assert mailbox observation) and `ctx.task_create(...)` (assert TaskList block contents). All three host-callback paths exercised in one test.
- **Phase(s) producing the test:** Phase 6 (Tasks 5 + 8)

### AC6.4: Agent calls `ctx.port.subscribe(plugin_port, config)`; events stream from plugin to pattern via IRPC server-stream; delivered as system reminders

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_transport.rs`
- **Test name(s):** `out_of_process_plugin_full_cycle` (subscribe stream assertion)
- **Verifies:** `connection.port_subscribe(port_id, json!({}))` returns a `BoxStream`; first event is `PortEvent::Line { .. }`. Stream events surface as `MessageAttachment::PortEvent` in segment 2 (existing sandbox-io path).
- **Phase(s) producing the test:** Phase 6 (Tasks 5 + 8)

### AC6.5: `McpPluginAdapter` wraps standalone MCP server as `PluginExtension`; MCP tools accessible as port calls; MCP resources as port subscriptions

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_transport.rs`
- **Test name(s):** `mcp_plugin_adapter_wraps_standalone_server`
- **Verifies:** Native plugin manifest declaring `mcp_servers { server "echo" {...} }`; on enable, port `mcp:echo` registered; `port.call("echo", json!({"value":"x"}))` returns `{"value":"x"}`.
- **Phase(s) producing the test:** Phase 6 (Tasks 6 + 8)
- **Gap flag:** MCP resource subscriptions documented as `Err(PortError::NotSupported)` in Phase 6 Task 6 ("MCP resource subscriptions not yet implemented; deferred to follow-up"). The AC text says resources "as port subscriptions" but the implementation explicitly defers. Flag as design-vs-implementation divergence — executor should either implement resource subscribe in this plan or get AC6.5 amended to drop the resources clause.

### AC6.6: Per-plugin cryptographic auth via iroh node identity (local); atproto-backed mutual auth (remote)

- **Type:** integration (split across Phase 6 + Phase 7)
- **File:** `crates/pattern_runtime/tests/plugin_transport.rs` (local); `crates/pattern_runtime/tests/plugin_atproto_auth.rs` (remote)

#### AC6.6 local (Phase 6)

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_transport.rs`
- **Test name(s):** `out_of_process_plugin_full_cycle` (allow-list tamper assertions)
- **Verifies:** Pubkey allow-list contains plugin's pubkey on install; tamper-remove pubkey → re-enable rejected; tamper-restore → enable succeeds.
- **Phase(s) producing the test:** Phase 6 (Tasks 5 + 8)

#### AC6.6 remote (Phase 7)

- **Type:** integration (wiremock-mocked PDS + Constellation)
- **File:** `crates/pattern_runtime/src/plugin/atproto.rs` (unit publish/resolve/verify); new test file `crates/pattern_runtime/tests/plugin_atproto_auth.rs` per Phase 7 Task 6's testing section
- **Test name(s):** `pinned_did_record_verifies_at_connect`, `tampered_sig_rejected`, `node_uri_mismatch_rejected`, `constellation_backlink_discovery_first_match_wins`, `constellation_zero_candidates_rejected`
- **Verifies:** Pinned-DID path verifies signed record at connect; tampered sig → `SignatureMismatch`; tampered nodeUri → `NodeUriMismatch`; Constellation-backlink path resolves and verifies counterpart; zero candidates → reject.
- **Phase(s) producing the test:** Phase 7 (Tasks 5 + 6)

### AC6.7: IRPC connection to plugin drops; plugin marked unhealthy; reconnection attempted; `PluginError::TransportLost` surfaced on next call

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_transport.rs`
- **Test name(s):** `out_of_process_plugin_full_cycle` (kill-child branch)
- **Verifies:** Kill plugin process; next `connection.port_call(...)` returns `PluginError::TransportLost`; `connection.health()` returns `Unhealthy { reason }`.
- **Phase(s) producing the test:** Phase 6 (Tasks 5 + 8)
- **Note on AC text:** AC6.7 says "reconnection attempted." Phase 6 explicitly defers auto-reconnect ("Phase 6 ships fail-and-surface; auto-reconnect is a follow-up"). Flag as design-vs-AC divergence — Phase 6 Notes say this is intentional. Executor should either ship reconnect in this plan or get AC6.7 amended.

### AC6.8: `pattern-plugin-sdk` crate compiles with minimal dependencies; does not pull in `pattern_runtime` or `pattern_memory`

- **Type:** integration (build assertion)
- **File:** `crates/pattern_plugin_sdk/tests/smoke_minimal_plugin.rs`
- **Test name(s):** `minimal_plugin_dep_tree_is_lean`
- **Verifies:** Build fixture `tests/fixtures/minimal_plugin/` against `pattern-plugin-sdk` (default features); `cargo tree` output does NOT contain `loro`, `genai`, `candle-core`, `tokio-tungstenite`, `rusqlite`, `pattern-runtime`, `pattern-memory`.
- **Phase(s) producing the test:** Phase 6 (Tasks 1 + 2 + 7)

### AC6.9: IRPC in-process mode (tokio mpsc) used by CC and MCP adapters verifiably has zero network overhead

*(Reinterpreted: AC says "IRPC in-process mode (tokio mpsc)"; Phase 6 Task 4 implements direct trait dispatch instead — strictly stronger property (no channel hop, no encoding, just a vtable call). Same observable behavior the AC requires — zero network overhead — verified at the trait-dispatch layer.)*

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_transport.rs`
- **Test name(s):** `in_process_plugin_zero_overhead`
- **Verifies:** Phase 6 Task 4 implements `InProcessPluginConnection` as direct `Arc<dyn PluginExtension>` trait dispatch (no channels, no postcard). Test instruments `port_call` to confirm it does NOT hit any IRPC encode/decode codepath (tracing-spans absence assertion).
- **Phase(s) producing the test:** Phase 6 (Tasks 4 + 8)

### AC7.1: Skills from installed plugins receive `trust_tier: PluginInstalled` via the code path reserved in Plan 2

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_cc_adapter.rs` (Phase 3); `crates/pattern_runtime/tests/plugin_smoke.rs` (Phase 7 end-to-end)
- **Test name(s):** `cc_plugin_install_translates_skills_with_plugin_installed_tier`, `plugin_smoke_full_stack` (skill trust tier assertion)
- **Verifies:** Phase 3 fixture plugin's skills → `trust_tier: PluginInstalled`; Phase 7 smoke confirms end-to-end with full stack.
- **Phase(s) producing the test:** Phase 3 (Tasks 4 + 7), verified again in Phase 7 (Task 7)

### AC7.2: Plugin capabilities scoped per manifest declaration; plugin agent cannot use effects beyond what the manifest declares

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_capabilities.rs` (per Phase 7 Task 3 testing section)
- **Test name(s):** `manifest_declared_caps_filter_prelude_at_compile_time`
- **Verifies:** Plugin manifest declares `effects { memory; message }`; agent program calls `Pattern.Shell.Execute`; tidepool-extract compile fails with `Pattern.Shell.Execute not in scope`.
- **Phase(s) producing the test:** Phase 7 (Task 3)

### AC7.3: User override in KDL config can expand or restrict a plugin's declared capabilities; override takes precedence

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_capabilities.rs`
- **Test name(s):** `user_override_narrows_effects_via_intersection`, `user_override_expands_flags_via_union`
- **Verifies:** User KDL `plugin_overrides { plugin "x" { capabilities { effects { message } } } }` narrows; previously-allowed `Memory.Put` now fails to compile. User KDL adds flag `spawn-new-identities` not in manifest → effective caps include it.
- **Phase(s) producing the test:** Phase 7 (Task 3)

### AC7.4: Ad-hoc skill (non-plugin source) triggers body-redact + user-enable flow on first use

- **Type:** unit + integration
- **File:** `crates/pattern_runtime/src/sdk/handlers/skills.rs` (unit body-redact); `crates/pattern_db/src/queries/skill_approvals.rs` (unit DB); `crates/pattern_server/tests/enable_skill.rs` or equivalent (integration RPC)
- **Test name(s):** `adhoc_skill_load_redacts_body_with_enable_hint`, `record_approval_flips_metadata_to_enabled`, `enable_skill_rpc_round_trip`, `slash_skill_enable_dispatch`
- **Verifies:** AdHoc skill load returns redaction marker + `/skill-enable` hint; `record_approval` + metadata update → next load returns body; revoke → redacted again; `EnableSkillRequest`/`Response` round-trip via `DaemonClient`; `/skill-enable` slash command updates metadata via `CommandRegistry`.
- **Phase(s) producing the test:** Phase 7 (Tasks 1 + 2)

### AC7.5: Plugin agent attempts to use an effect not in its manifest-declared or user-overridden capabilities; rejected at prelude filtering (compile-time)

- **Type:** integration (compile-time assertion)
- **File:** `crates/pattern_runtime/tests/plugin_capabilities.rs`
- **Test name(s):** `out_of_caps_effect_rejected_at_prelude_filter` (overlaps AC7.2 fundamentally)
- **Verifies:** Same mechanism as AC7.2 — `build_for(caps)` filters effect decls; agent program referencing a filtered-out effect fails to compile.
- **Phase(s) producing the test:** Phase 7 (Task 3)

### AC7.6: Plugin with no declared capabilities gets an empty CapabilitySet; can only perform pure computation

- **Type:** integration
- **File:** `crates/pattern_runtime/tests/plugin_capabilities.rs`
- **Test name(s):** `empty_capabilities_only_pure_computation_compiles`
- **Verifies:** Manifest with `effects {}` (empty); pure agent program compiles; same program calling any effect fails to compile.
- **Phase(s) producing the test:** Phase 7 (Task 3)

### AC8.1: Smoke test at `crates/pattern_runtime/tests/plugin_smoke.rs` passes: installs CC-format plugin (via CcPluginAdapter), installs native IRPC plugin, wraps MCP server (via McpPluginAdapter), verifies skill trust tiers, hook events fire, MCP inverted surface works, port registration works, capability enforcement active

- **Type:** integration (e2e)
- **File:** `crates/pattern_runtime/tests/plugin_smoke.rs`
- **Test name(s):** `plugin_smoke_full_stack`
- **Verifies:** CC plugin install (cc-full-fixture), OOP plugin install + enable, McpPluginAdapter for echo server, hook events captured (`turn.before`, `port.called`, `turn.after.success`), MCP inverted surface segment 2 reminder, capability enforcement deny on out-of-scope shell, port registration (`http`, `mcp:echo`, OOP-declared ports).
- **Phase(s) producing the test:** Phase 7 (Task 7)

### AC8.2: Mock ProviderClient and mock MCP server (stdio); no live model or network dependency in CI

- **Type:** integration (test harness assertion)
- **File:** `crates/pattern_runtime/tests/plugin_smoke.rs`
- **Test name(s):** `plugin_smoke_full_stack` (test harness setup)
- **Verifies:** `MockProviderClient::with_turns(...)` drives turns. Echo MCP fixture is local stdio script (`tests/fixtures/mcp/echo-server/server.sh`). No reqwest call to a live model. CI passes offline.
- **Phase(s) producing the test:** Phase 7 (Task 7)

### AC8.3: `pattern_mcp` crate fully removed from workspace; all MCP client code lives in `pattern_runtime`

- **Type:** integration (filesystem + workspace assertion)
- **File:** Phase 7 Task 8 audit (no test file; verified by `cargo check --workspace` and `find crates/pattern_mcp` returning nothing)
- **Test name(s):** N/A (audit task)
- **Verifies:** `crates/pattern_mcp/` directory deleted via `git rm -r`; workspace `Cargo.toml` confirmed clean; no `pattern_mcp` import found via `rg pattern_mcp crates/`; `cargo check --workspace` succeeds.
- **Phase(s) producing the test:** Phase 7 (Task 8)

### AC8.4: Any step in the smoke flow failing produces a clear error identifying which step and which assertion

- **Type:** integration (test ergonomics assertion)
- **File:** `crates/pattern_runtime/tests/plugin_smoke.rs`
- **Test name(s):** `plugin_smoke_full_stack` (assertion messages)
- **Verifies:** Each `assert!` and `assert_eq!` in the smoke test carries a descriptive message identifying step + expected. `expect("...")` strings on `unwrap`s point at the failing step. Executor reviews each assertion's message at code-review time per the design's "load-bearing" note.
- **Phase(s) producing the test:** Phase 7 (Task 7) — quality gate during code review.

### AC8.5: Plugin smoke test runs concurrently with other tests without shared-state interference

- **Type:** integration (test isolation assertion)
- **File:** `crates/pattern_runtime/tests/plugin_smoke.rs`
- **Test name(s):** `plugin_smoke_full_stack`
- **Verifies:** Per-test `tempfile::TempDir`-isolated `PatternPaths::with_base(...)`; no global mutable state assumed; `cargo nextest run --workspace` (which parallelizes by default) runs the smoke test alongside others without interference.
- **Phase(s) producing the test:** Phase 7 (Task 7) — verified by green `cargo nextest run --workspace`.

## Human verification

No acceptance criteria are exclusively human-verification. AC8.4 has a code-review quality component (asserting that error messages are *clear*, not just present), but the underlying assertions are all automated. AC5.10 and AC8.3 are workspace-build assertions verified by `cargo check --workspace` rather than dedicated test files — automated, but no per-AC test file.

## Coverage summary

- Total ACs: 50 (AC1: 5, AC2: 6, AC3: 8, AC4: 7, AC5: 10, AC6: 9 with AC6.6 split into local + remote sub-mappings, AC7: 6, AC8: 5)
- Automated: 50
- Human verification: 0
- Build-only (no dedicated test file): 2 — AC5.10, AC8.3 (proven by `cargo check --workspace` after Phase 7 Task 8 deletes `crates/pattern_mcp/`)
- Split mappings: AC6.6 (local Phase 6 / remote Phase 7); AC7.1 (Phase 3 unit + Phase 7 smoke end-to-end); AC4.4 (Phase 2 alias-table unit + Phase 2 Task 8 integration); AC2.3 (Phase 1 emit-seam + Phase 2 hookbus subscriber)

## Flagged gaps and divergences

The executor should resolve these before considering the plan ready to ship:

1. **AC1.3 wording vs. Phase 1 implementation.** AC says "silently ignored"; Phase 1 Task 4 deliberately preserves CC unknown fields under `cc.fields: BTreeMap` and Pattern KDL unknown top-level nodes under `unknown_kdl: BTreeMap`. Resolved: AC1.3 entry above carries the `(Reinterpreted: ...)` marker. Tests assert preservation.

2. **AC6.5 resources-as-port-subscriptions vs. Phase 6 Task 6 deferral.** Phase 6 explicitly returns `PortError::NotSupported` for MCP resource subscribe with comment "deferred to follow-up." AC6.5 claims they're accessible. Action: ship resource subscribe in this plan, or amend AC6.5 to drop the resources clause.

3. **AC6.7 reconnection-attempted vs. Phase 6 fail-and-surface.** AC says "reconnection attempted"; Phase 6 Notes say "Phase 6 ships fail-and-surface; auto-reconnect is a follow-up." Action: implement basic backoff-reconnect in Phase 6, or amend AC6.7 to drop the reconnect clause.

4. **AC6.9 wording — IRPC in-process vs. direct trait dispatch.** AC says "IRPC in-process mode (tokio mpsc)"; Phase 6 Task 4 chose direct trait dispatch (zero overhead, no IRPC encode at all — strictly stronger property than the AC's literal text). Resolved: AC6.9 entry carries the `(Reinterpreted: ...)` marker; Phase 6 Task 4 explicitly commits to direct dispatch.

5. *(Resolved.)* Phase 6 Task 8 now exercises all three host-callback paths (`ctx.memory().get_block`, `ctx.send_message`, `ctx.task_create`) with separate assertions per the AC6.3 reinterpretation.

6. **AC2.3 hook-event firing depends on Phase 2.** Phase 1 only ships an emit-seam (no-op default); the real `HookBus` subscriber assertion needs Phase 2's bus to be wired into the registry. The test for "`plugin.uninstall` hook event fires" is split: Phase 1 exercises the seam with a custom capture closure, Phase 2 wires the bus. The executor should ensure Phase 2 Task 8 includes a `plugin.uninstall` delivery test against a real bus subscriber, not just rely on Phase 1's closure-capture proxy.
