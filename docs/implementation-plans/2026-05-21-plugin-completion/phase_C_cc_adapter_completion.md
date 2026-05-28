# Phase C — CC Adapter Completion

**Plan:** docs/implementation-plans/2026-05-21-plugin-completion/phase_C_cc_adapter_completion.md
**Status:** drafted 2026-05-21. Not started.
**Depends on:** Phase B (PermissionRequest/Decision plumbing for PreToolUse/PermissionRequest hook events).
**Unblocks:** real use of Claude Code plugins as pattern extensions.

## Motivation

The CC adapter exists so we can reuse existing Claude Code plugins without rewriting them. **CC plugins get a deliberate subset of Pattern features** — not parity with OOP plugins. The adapter translates CC's native concepts (hooks with shell/http/agent/prompt/mcp_tool handlers, slash commands, subagents, skills, mcp_config) onto pattern's primitives where they map.

Current state:
- Skills load (works, phase 3)
- Hooks declared in plugin.json or hooks/hooks.json are parsed + subscribed (works)
- Hook *handlers*: Command branch runs the subprocess but **DROPS the output** (only logs success); Http branch logs "not yet implemented"; anything else logs "not yet supported"
- Commands: not parsed, not wired
- Subagents: not parsed, not wired
- mcp_config auth-from-headers: TODO marker (only stub flagged with TODO label)

Reference: [Claude Code hooks documentation](https://code.claude.com/docs/en/hooks.md)

## Current state surface map

### `cc_adapter/hooks.rs` (371 lines)

- `HookHandler` enum has variants: `Command { command, env }`, `Http { url, method }`, `Skipped { original_type, reason }`.
- Command branch runs `run_command_hook` and binds `Ok(output)` but **does not use it** — `output` is discarded; only debug-logs success/failure.
- Http branch unconditionally logs "http hooks not yet implemented".
- Skipped catches anything CC supports that pattern doesn't yet: `prompt`, `agent`, `mcp_tool`.

### `cc_adapter/mcp_config.rs` (169 lines)

- One TODO: `auth: AuthConfig::None, // TODO: parse auth from headers` — line 86.

### `cc_adapter.rs` itself

- `PluginExtension::on_enable` wires hook subscriptions + loads skills. Does not wire commands or subagents.
- `PluginManifest` has `commands` + `agents` fields but `cc_adapter` doesn't consume them.

## Target state

CC plugins can be installed via `pattern plugin install <cc-plugin-dir>` and:

- Their command hooks execute and **their JSON output is consumed** — decisions block events, additionalContext is attached to the next agent message as `MessageAttachment::Custom(String)`, permission decisions feed the broker (phase B).
- Their http hooks fire real HTTP requests + consume the response body the same way.
- Their slash commands are dispatchable from TUI / discord plugin.
- Their subagents are spawnable via Spawn.sibling with the subagent's prompt + matcher.
- mcp_config carries auth correctly when the CC plugin's mcp config declares auth headers.

## Tasks

### C.1 — Consume command hook output

Per CC's [hook docs](https://code.claude.com/docs/en/hooks.md):

**Exit codes:**
- 0 = success; stdout parsed as JSON if present
- 2 = blocking error; stderr fed to Claude (for us: feed to the event-emitting agent's next message)
- other = non-blocking error (warn + continue)

**JSON output fields** (exit 0 + stdout):
- `decision: "block" | "approve"` + `reason: String` — top-level for Stop/SubagentStop/UserPromptSubmit/PostToolUse
- `continue: false` + `stopReason: String` — stop the agent entirely (maps onto Pattern's agent-stop)
- `hookSpecificOutput.additionalContext: String` — maps to `MessageAttachment::Custom(String)` on the next agent message (per orual)
- `hookSpecificOutput.permissionDecision: "allow"|"deny"|"ask"|"defer"` + `permissionDecisionReason` for PreToolUse — maps to PermissionBroker::respond (phase B)
- `hookSpecificOutput.decision.behavior: "allow"|"deny"` for PermissionRequest — also phase B
- `systemMessage: String` — surface to user via Display.note
- `terminalSequence: String` — desktop notification (defer / map to plugin-channel notify later)

Implementation:
- New module `cc_adapter/hook_response.rs`: `HookResponseJson` struct (serde) + `apply_to_pattern_response` fn
- Modify `run_command_hook` to return `(ExitStatus, String stdout, String stderr)` instead of `()`
- The Command branch parses stdout-as-JSON (best-effort: malformed JSON → warn + treat as plain text per CC's docs)
- Map fields onto `pattern_core::hooks::HookResponse`:
  - `decision: "block"` → `HookResponse::Block { reason }`
  - `continue: false` → `HookResponse::StopAgent { reason }`
  - `additionalContext` → attach to event's outgoing message as `MessageAttachment::Custom(...)`
  - `systemMessage` → emit via `Display.note` channel for the partner
  - permission fields → call `PermissionBroker::respond` (requires phase B + the event needs to carry a permission request_id for the broker to know which one to respond to)
- 10,000 char cap per CC docs — handle overflow gracefully (truncate + tail-note)

### C.2 — Http hook handler

- Implement the Http branch using existing `pattern::http` port or `reqwest` directly.
- Per CC docs: 2xx body parses as JSON via same schema as command hooks; non-2xx is non-blocking error (warn + continue).
- Use `method` from handler config; default POST if None.
- POST body = the JSON hook event (same shape command hooks receive on stdin).
- Timeout: 30s default (CC docs default, configurable per-hook).

### C.3 — Expand HookHandler coverage

Currently `Skipped` catches: `prompt`, `agent`, `mcp_tool`.

- **`prompt`**: CC injects a fresh LLM call with the prompt + event context. Map onto a Pattern ephemeral spawn with that prompt? Or skip for v1 since it doubles LLM cost. **Decision: skip for v1, keep Skipped, document.**
- **`agent`**: invokes a CC subagent — see C.5.
- **`mcp_tool`**: invokes a registered MCP tool — map onto `Mcp.call`.

### C.4 — Wire CC slash commands

- Parse `manifest.commands` (already in manifest struct, unused by adapter)
- Each command is a `.md` file in `commands/` with frontmatter (name, description) + body (prompt template)
- Register with Pattern's command-dispatch (the same surface TUI / discord plugin use for `/command`)
- Invocation: command body becomes a UserPromptSubmit-style event with the command's prompt as the message body

### C.5 — Wire CC subagents

Per orual: CC subagent → `Spawn.sibling` with `system_prompt = subagent.prompt`. Subagents don't get history (Pattern's sibling gives fresh context; ephemeral inherits context — sibling is the right map). Ephemeral works too with different behavior, but sibling matches CC's no-history semantics.

- Parse `manifest.agents` — each is a `.md` file in `agents/` with frontmatter (name, description, optional matcher) + body (system prompt)
- Register subagent definitions with the spawn registry under `cc:<plugin_id>:<agent_name>`
- When invoked (via `/agents <name>`, or via CC's TaskCreate hook): construct a `SiblingConfig { system_prompt: agent_body, name: agent_name, ... }` and call `Spawn.sibling`
- TaskCreate/TaskCompleted/SubagentStart/SubagentStop hook events map to Pattern's task-graph events on the sibling

### C.6 — mcp_config auth-from-headers

- Line 86 in `cc_adapter/mcp_config.rs`: parse the `headers` field on CC mcp config → populate `AuthConfig` variant
- Support: Bearer token, Basic auth, custom header
- Map onto pattern's existing `AuthConfig` types

### C.7 — Integration test: real CC plugin

Pick a small real CC plugin (or write a minimal fixture) and verify end-to-end:
- Install via `pattern plugin install`
- Hook fires on relevant event, output consumed correctly
- additionalContext attaches to next message
- decision: block prevents the event
- Slash command dispatchable from TUI
- Subagent spawnable via /agents

## Acceptance criteria

- CC command hook output is parsed + applied (no more discarded `Ok(output)`)
- Http hook handler works end-to-end against a test server
- mcp_tool handler dispatches via Mcp.call
- Slash commands from CC plugin are dispatchable
- Subagents from CC plugin spawn as siblings with correct system prompt
- mcp_config carries auth headers
- A real CC plugin (or fixture) runs end-to-end exercising at least: command hook with decision block + slash command + subagent
- Workspace tests pass

## Gotchas

- **CC plugins are an external ecosystem we adapt, not first-class Pattern citizens.** Their model is `command-hook subprocess + stdin/stdout JSON`. They never get host-callbacks to Pattern's SDK — they don't know about Pattern at all. Don't try to give them more than CC's own runtime gives them.
- **10,000 char output cap**: CC docs say overflow gets saved to file + preview substituted. Mirror this behavior so plugins behaving sanely against CC's docs also behave sanely against us.
- **`additionalContext` is replayed on resume per CC docs**: timestamps/SHAs in additionalContext become stale on session resume. **Resolved (orual 2026-05-21): SessionStart hooks fire again on session resume** (with `source: "resume"` per CC's docs), so they can refresh stale context. Pattern's session-resume path needs to re-fire SessionStart hooks on its own session-resume code path too (not just CC plugins — applies to Pattern's own session lifecycle generally).
- **Hook output stdout MUST be only the JSON** — if shell profiles emit text on startup, it breaks JSON parsing. CC has a [JSON validation failed](https://code.claude.com/docs/en/hooks-guide#json-validation-failed) troubleshooting section we should link to in our error messages.
- **Subagent prompt + matcher**: CC's subagent frontmatter can declare a matcher (e.g. matches certain tool names). Honor it when deciding whether to even spawn.
- **Hook decision races**: multiple hooks can fire for the same event with conflicting decisions. CC's resolution rules apply (most-restrictive-wins for block-shape). Document the resolution in `hook_response.rs`.
- **Permission decisions from CC require phase B's plumbing** — if phase B isn't done when we get to PreToolUse, this task partial-lands with the permission-mapping deferred.

## Out of scope (deferred)

- CC `prompt`-type hooks (extra LLM call) — keep as Skipped
- CC's `Setup` event (one-shot init scripts) — Pattern doesn't have a directly analogous lifecycle moment. Map onto on_install or skip.
- CC's `PreCompact`/`PostCompact` events — Pattern's compaction is different shape; map onto compose/compression lifecycle if needed, else skip
- CC's `WorktreeCreate`/`WorktreeRemove`/`CwdChanged`/`FileChanged` — these are CC-internal events with no Pattern analog. Skip.

## Verification before declaring done

1. Install a real CC plugin (or curated fixture) via `pattern plugin install`
2. Trigger a hook event that the plugin handles with a Command hook returning JSON `{decision: "block", reason: "don't do that"}` — verify the event is blocked + reason surfaces
3. Trigger an event with a hook that returns `additionalContext: "<some fact>"` — verify the next message to the agent carries that as `MessageAttachment::Custom`
4. Trigger a slash command — verify it dispatches
5. Trigger `/agents <name>` — verify sibling spawn fires with the right system prompt
6. CC plugin returns `permissionDecision: "allow"` for a PreToolUse — verify the broker (phase B) receives the decision (requires phase B done)
