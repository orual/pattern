# Phase B — Permissions Plumbing

**Plan:** docs/implementation-plans/2026-05-21-plugin-completion/phase_B_permissions_plumbing.md
**Status:** drafted 2026-05-21. Not started.
**Depends on:** Phase A (uses PluginHostProtocol/PluginGuestProtocol substrate, but new variants are additive).
**Unblocks:** Phase C (CC adapter's PreToolUse/PermissionRequest/PermissionDenied hooks consume this).

## Motivation

Pattern has a real permission substrate already: `pattern_core::permission::PermissionBroker` (823 lines, per-session, scope-cache, broadcast/oneshot decision channel) and `pattern_runtime::permission::PermissionBridge` (sync-to-async bridge for the eval-worker thread). What's missing is **consumers**:

1. **No UI surface receives permission requests.** The broker broadcasts to `broker.subscribe()` — currently nothing in the TUI subscribes. Every request times out → silently denied. Orual: "i can't approve or deny something from the TUI."
2. **Plugins have no permission protocol.** PluginGuestProtocol can't deliver `PermissionRequest` events to plugins, and PluginHostProtocol can't receive `PermissionDecision` responses from plugins acting as deciders (matters for CC adapter's PreToolUse mapping).
3. **Effects-side gating is partial.** Only `pattern_runtime::file_manager::manager` (File handler shape-guard for KDL config writes) currently calls `PermissionBridge::request_sync`. Shell.execute, Web.fetch, port calls to network-side ports — none gate through the broker today.

## Current state (as of 2026-05-21)

### What exists (substrate complete)

- `pattern_core::permission`:
  - `PermissionBroker::request(agent_id, tool_name, scope, origin, reason, metadata, timeout)` → async, returns `Option<PermissionGrant>`
  - `PermissionBroker::respond(request_id, decision)` for external deciders
  - `PermissionBroker::subscribe()` → `broadcast::Receiver<PermissionRequest>` for external subscribers
  - Scope cache for `ApproveForScope` + `ApproveForDuration` short-circuit
  - `MessageOrigin::bypasses_permission_gate` for Partner-direct bypass
  - Session-lifetime grants only by current implementation (the load-bearing-invariant framing in the doc-comment is being relaxed per orual 2026-05-21; persistence is fine except for FileWriteConfig)
  - `PermissionScope` enum: MemoryEdit, MemoryBatch, ToolExecution, DataSourceAction, FileWrite, FileWriteConfig
  - `PermissionDecisionKind`: Deny, ApproveOnce, ApproveForDuration, ApproveForScope
- `pattern_runtime::permission`:
  - `PermissionBridge::spawn(broker)` → bridge handle the eval-worker thread can use
  - `PermissionBridge::request_sync(...)` blocks the eval-worker thread until broker responds
  - Mirrors `RouterBridge` shape for consistency

### What's missing

- TUI client subscribing to the broker's broadcast + rendering modal + sending decision via `broker.respond(...)`
- Wire protocol for TUI ↔ daemon permission flow (the existing IRPC channel needs new variants)
- PluginGuestProtocol variant: runtime → plugin permission notifications (so plugins can observe / log / pre-veto)
- PluginHostProtocol variant: plugin → runtime permission request (so plugins can initiate gating from their port handlers)
- Effects-side gating audit: which handlers should gate but don't?

## Target state

1. TUI shows a permission dialog when an agent requests a gated operation. User picks Deny / Once / ForDuration(N) / ForScope. Decision flows back through the broker.
2. Discord plugin (and future plugins) can request permission via PluginHostProtocol::HostPermissionRequest — same broker, same decision flow.
3. CC adapter (phase C) can map CC's PreToolUse/PermissionRequest decisions onto PermissionBroker responses.
4. All effects with side-effects beyond the local mount (Shell, Web, port calls touching network, MCP calls) gate through the bridge with sensible default scopes.

## Tasks

### B.1 — TUI permission subscriber + dialog

- TUI client subscribes to `permission_broker.subscribe()` over the existing daemon ↔ TUI IRPC channel (the SessionRoutingProtocolHandler / route table).
- New wire variants:
  - `SessionEvent::PermissionRequested(PermissionRequest)` — daemon → TUI fanout
  - `SessionCommand::PermissionDecision { request_id, decision: PermissionDecisionKind }` — TUI → daemon response
- TUI renderer: modal/popup showing tool_name + scope + reason. Options: Deny / Approve Once / Approve for 1h / Approve for Session-Scope.
- Decision posts back via `broker.respond(request_id, decision)`.
- Timeout behavior: if user doesn't decide in N seconds, TUI shows a countdown; default deny on the broker side handles the silent path.
- Test: send a `broker.request(...)` from a test session, assert the TUI subscriber receives it and can post back a decision that satisfies the request.

### B.2 — Plugin protocol permission variants

Add to `PluginGuestProtocol` (runtime → plugin):
- `OnPermissionRequest(WirePermissionRequest)` — fire-and-forget notification when a permission request involves this plugin's operations (so plugins can observe / log)

Add to `PluginHostProtocol` (plugin → runtime):
- `HostPermissionRequest(WirePermissionRequest)` → `Result<WirePermissionGrant, WirePluginError>` — plugins initiating gated ops

Wire-format types (`WirePermissionRequest`, `WirePermissionGrant`, `WirePermissionScope`, `WirePermissionDecision`) mirror the pattern-core types but with serde-stable layouts.

Test: a fixture plugin calls `HostPermissionRequest`, runtime gates via broker, TUI subscriber approves, plugin gets back `WirePermissionGrant`.

### B.3 — Effects-side gating audit + wiring

Audit which handlers should call `PermissionBridge::request_sync` but don't. Likely candidates:

- `Shell.execute` / `Shell.spawn` — gate with `PermissionScope::ToolExecution { tool: "shell", args_digest }`
- `Web.fetch` / `Web.search` — gate with new `PermissionScope::NetworkAccess { host }` variant
- `Mcp.call` — gate with `PermissionScope::ToolExecution { tool: "mcp:<server>:<method>", ... }`
- Port calls that reach network-side resources (currently no gate — needs design)
- `Recall.insert` / `Memory.put` for protected blocks (configurable via .pattern.kdl rules)

For each handler: add `request_sync` call before the side-effect; on `None` (denied/timeout), return a structured error rather than executing.

Audit deliverable: a markdown table in `docs/implementation-plans/2026-05-21-plugin-completion/phase_B_audit.md` listing each effect handler, whether it currently gates, the scope shape it should use, and any per-mount KDL rule that should pre-approve.

Land gating per-handler in separate commits with regression tests.

### B.4 — Scope additions to PermissionScope enum

Add new variants the audit will need (likely):
- `NetworkAccess { host: String }` — for Web.fetch / network port calls
- `McpInvoke { server: String, method: String }` — for Mcp.call gating
- `SpawnChild { kind: SpawnKind }` — for Spawn.ephemeral / Spawn.fork / Spawn.sibling (if we want to gate child creation)

Each new variant is additive on the wire (PermissionScope is `#[non_exhaustive]`), so adding doesn't break compat.

### B.5 — Grant persistence (with FileWriteConfig exception)

- Add a persistence backend for `PermissionGrant`: SQLite (likely in the existing pattern_db) keyed on (mount_id, agent_id, scope) with expires_at.
- On TidepoolSession::open, load matching grants into the broker's scope cache.
- On `respond` with `ApproveForDuration` / `ApproveForScope`, persist the grant (unless scope matches the exclusion list).
- **Exclusion list:** `FileWriteConfig { .. }` (and possibly FileWrite for protected paths — confirm during implementation). These scopes get session-lifetime grants only.
- Update the doc-comment in `pattern_core/src/permission.rs` to reflect the new threat model: persistence is fine except for the explicit exclusion list. Don't leave the old "don't add persistence" comment in place — that's a stale load-bearing claim.

### B.6 — `.pattern.kdl` rules surface for declarative pre-approval


Per the broker's load-bearing invariant: grants are session-lifetime only, **but rules can pre-approve a scope at session-open time**. Add a `permissions { ... }` block to `.pattern.kdl`:

```kdl
permissions {
    // pre-approve shell commands matching a prefix
    rule scope="tool:shell" args-prefix="git " decision="approve-for-session"
    // gate all network access unless allowlisted
    rule scope="network" host="github.com" decision="approve-for-duration" duration="1h"
    rule scope="network" decision="ask"
}
```

Rules loaded at TidepoolSession::open populate the broker's scope cache before the first request.

## Acceptance criteria

- TUI shows a dialog on permission request + decision posts back to broker
- Plugin protocol carries PermissionRequest/Grant in both directions (tested via fixture plugin)
- At minimum Shell.execute, Web.fetch, Mcp.call gate through the broker (audit may add more)
- `.pattern.kdl` permissions block parses + populates broker scope cache at session open
- Existing File handler shape-guard for KDL config writes still works (regression)
- All workspace tests pass

## Gotchas

- **Grants CAN persist (per orual 2026-05-21) — EXCEPT FileWriteConfig.** The original `pattern_core/permission.rs` doc-comment says "don't persist grants without rethinking threat model." The threat model has been rethought: most scopes are fine to persist (UX win, less prompt fatigue). The exception is **FileWriteConfig** — the shape-guard for Pattern's own KDL config writes. A persisted "approve forever" there defeats the gate by definition. Implementation note: when wiring persistence, exclude FileWriteConfig (and possibly the related shape-detected variants) by scope-variant match. Update the load-bearing doc-comment in `pattern_core/src/permission.rs` accordingly.
- **Partner bypass via MessageOrigin**: `bypasses_permission_gate` short-circuits requests when the immediate dispatcher is a Partner (direct-execution, not autonomous agent). Don't accidentally route plugin-initiated requests through a Partner-shaped origin.
- **Eval-worker thread can't `block_on`**: any new effect handler that gates must use `PermissionBridge::request_sync`, not raw broker calls.
- **Timeout default**: the broker times out → `None` → handler must treat as deny. Don't silently fall through to "approve" on timeout.
- **Wire types must mirror but not alias**: WirePermissionRequest is the postcard-stable form, pattern_core::PermissionRequest is the runtime form. Convert at the wire boundary; don't share serde tags.
- **Wake up policy**: if TUI is disconnected when a request fires, broker timeout fires → deny. Document this in the TUI auth phase D notes — connected TUI is part of the trust model.

## Out of scope (deferred)

- Multi-decider quorum (e.g. "both partner and admin must approve") — not needed for v1, would need scope-cache rework
- Remote-plugin permission requests (those land with phase E once atproto auth resolves identity)
- (resolved 2026-05-21) **Ephemerals inherit permissions from parent.** Per orual: otherwise the noise of re-prompting for every ephemeral spawn is unworkable. Implementation: spawned children share the parent's permission broker scope-cache rather than getting a fresh one. Forks/siblings likely same shape since they're peer-like, but confirm at implementation time.

## Verification before declaring done

1. End-to-end manual: start daemon + TUI, run an agent that tries to Shell.execute, see the modal, approve, command runs.
2. Same flow but Deny — command should fail with structured error.
3. Same flow but ApproveForScope — second matching invocation in same session short-circuits without re-prompting.
4. Fixture plugin: makes HostPermissionRequest, gets WirePermissionGrant back, proceeds with operation.
5. KDL rules: `.pattern.kdl` with a `permissions { rule scope="tool:shell" args-prefix="git " decision="approve-for-session" }` block lets `git status` through without a modal.
