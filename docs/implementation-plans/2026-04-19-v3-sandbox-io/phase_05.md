# Phase 5: Runtime-provided ports + integration

**Goal:** Ship `HttpPort` as the first concrete `Port` impl (registered at runtime startup); unify the three system-reminder pipelines (FileEdits from Phase 2, ShellOutput from Phase 3, PortEvents from Phase 4) so they share splice/render machinery instead of three near-identical code paths; write the end-to-end smoke test that exercises shell + file + port surfaces deterministically; finalize cleanup so only Spawn (Plan 3) and Mcp (Plan 4) handler stubs remain.

**Architecture:** `HttpPort` lives in `crates/pattern_runtime/src/ports/http.rs` and uses `reqwest` (already a workspace dep, used by `pattern_core` and `pattern_mcp`). Methods: `configure` (set base URL / default headers / timeout), `get`, `post`, `put`, `delete`, `head`. No `subscribe` — `HttpPort::capabilities()` returns `subscribable: false`. The `library()` returns a Haskell `Pattern.Http` module with typed wrappers around the JSON payload format. System reminder unification: all three turn-composition splice points (Phase 2 task 8, Phase 3 task 7, Phase 4 task 5) currently write `MessageAttachment::FileEdits` / `ShellOutput` / `PortEvents` separately. This phase introduces a single `MessageAttachment::SystemReminders(Vec<SystemReminder>)` variant where `SystemReminder` is an enum over the three sources — the splice happens once, render is one `Segment2Pass` arm, drain calls happen in sequence inside one helper. Smoke test at `crates/pattern_runtime/tests/sandbox_io_smoke.rs` runs the full `TidepoolSession` lifecycle exercising all three subsystems; uses a mock provider (no live model dependency).

**Tech Stack:** Rust async, `reqwest = "0.12"` (workspace), `wiremock` (test-only — already used by pattern_provider for HTTP-mocked tests, verify at execution time).

**Scope:** Phase 5 of 5 — final phase. Depends on Phases 1-4. Prerequisites for execution: Plan 3 (v3-multi-agent) Phase 1 must have landed for `CapabilitySet` (file/shell/port effect category methods). Same parking note as Phases 2-4.

**Codebase verified:** 2026-04-24. Evidence:
- `reqwest` is a workspace dep at `Cargo.toml`. `pattern_core/Cargo.toml:38` and `pattern_mcp/Cargo.toml:35` consume it.
- Test layout at `crates/pattern_runtime/tests/`: 16 existing integration tests; new `sandbox_io_smoke.rs` slots in.
- `wiremock`: investigator did not confirm; verify with `grep wiremock crates/*/Cargo.toml` at execution time. If absent, **ask orual** before adding.
- `MessageAttachment` plumbing (introduced by Phase 2): `compose_request_for_turn` in `crates/pattern_runtime/src/agent_loop.rs` already does the splice-on-first-user-message dance for `BatchOpeningSnapshot`. Phases 2-4 each add their own `drain_*` + splice block; Phase 5 collapses these into one helper.
- Stubs remaining after Phases 1-4: `SpawnHandler` (Plan 3 — v3-multi-agent owns it) and `McpHandler` (Plan 4 — v3-extensibility owns it). Both stay as stubs.

---

## Acceptance Criteria Coverage

### v3-sandbox-io.AC5: Integration and cleanup
- **v3-sandbox-io.AC5.1 Success:** `HttpPort` registered as runtime-provided port; `Port.Call("http", "get", {url})` performs HTTP request and returns response
- **v3-sandbox-io.AC5.2 Success:** System reminders from file watches, shell spawn output, and port subscriptions all appear in segment 2 of the agent's next turn
- **v3-sandbox-io.AC5.3 Success:** Smoke test at `crates/pattern_runtime/tests/sandbox_io_smoke.rs` passes deterministically: exercises shell execute, file open+write+external-edit+merge, port call+subscribe
- **v3-sandbox-io.AC5.4 Success:** Sources handler stub and Rpc handler stub deleted; only Spawn (Plan 3) and Mcp (Plan 4) stubs remain
- **v3-sandbox-io.AC5.5 Success:** `canonical_effect_decls()` updated for Shell, File, Port effects; removed Sources and Rpc declarations
- **v3-sandbox-io.AC5.6 Failure:** Any step in smoke test failing produces a clear error identifying which step and which assertion
- **v3-sandbox-io.AC5.7 Edge:** Smoke test runs concurrently with other tests without shared-state interference

---

## Subcomponent layout

- **A (tasks 1-2): `HttpPort` impl + runtime registration.**
- **B (tasks 3-4): System reminder unification — single `SystemReminders` variant + one splice helper + Segment-2 render arm.**
- **C (tasks 5-6): End-to-end smoke test + final cleanup verification.**

---

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: `HttpPort` impl

**Files:**
- Create: `crates/pattern_runtime/src/ports/mod.rs` — module root.
- Create: `crates/pattern_runtime/src/ports/http.rs` — `HttpPort` impl.
- Create: `crates/pattern_runtime/haskell/Pattern/Http.hs` — Haskell library module returned by `HttpPort::library()`.
- Modify: `crates/pattern_runtime/src/lib.rs` — `pub mod ports;`.

**Implementation:**

`HttpPort` wraps a `reqwest::Client` (already constructed with sensible defaults — gzip, brotli, redirect policy). Methods are dispatched by the string `method` argument:

- `"configure"` — sets base URL / default headers / timeout. Optional; HttpPort works without configuration (just no defaults).
- `"get"`, `"post"`, `"put"`, `"delete"`, `"head"` — HTTP verb. Payload shape: `{ url, headers?: Map, body?: String, query?: Map }`. Response shape: `{ status, headers, body }`.

Configuration state is held in `Mutex<HttpConfig>` because `Port::call` takes `&self` (no `&mut`), and reqwest doesn't allow header-mutation on a constructed client without rebuilding.

```rust
use std::sync::Mutex;
use std::time::Duration;
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use pattern_core::traits::Port;
use pattern_core::types::port::{
    PortCapabilities, PortError, PortEvent, PortId, PortMetadata,
};

#[derive(Debug, Default, Clone)]
struct HttpConfig {
    base_url: Option<String>,
    default_headers: std::collections::BTreeMap<String, String>,
    timeout: Option<Duration>,
}

#[derive(Debug)]
pub struct HttpPort {
    id: PortId,
    client: reqwest::Client,
    config: Mutex<HttpConfig>,
}

impl HttpPort {
    pub fn new() -> Self {
        Self {
            id: PortId::new("http"),
            client: reqwest::Client::builder()
                .gzip(true)
                .brotli(true)
                .build()
                .expect("HTTP client builder cannot fail with default config"),
            config: Mutex::new(HttpConfig::default()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RequestPayload {
    url: String,
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    query: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct ResponsePayload {
    status: u16,
    headers: std::collections::BTreeMap<String, String>,
    body: String,
}

#[async_trait]
impl Port for HttpPort {
    fn id(&self) -> &PortId { &self.id }

    fn metadata(&self) -> PortMetadata {
        PortMetadata {
            id: self.id.clone(),
            description: "HTTP/HTTPS request port (one-shot)".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            methods: vec![
                "configure", "get", "post", "put", "delete", "head",
            ].into_iter().map(String::from).collect(),
        }
    }

    fn capabilities(&self) -> PortCapabilities {
        PortCapabilities { subscribable: false, callable: true, requires_configuration: false }
    }

    async fn subscribe(&self, _config: serde_json::Value)
        -> Result<BoxStream<'static, PortEvent>, PortError>
    {
        Err(PortError::NotSubscribable(self.id.clone()))
    }

    async fn call(&self, method: &str, payload: serde_json::Value)
        -> Result<serde_json::Value, PortError>
    {
        match method {
            "configure" => {
                let cfg: HttpConfig = serde_json::from_value(payload)
                    .map_err(|e| PortError::BadPayload {
                        port: self.id.clone(),
                        method: method.to_string(),
                        message: e.to_string(),
                    })?;
                *self.config.lock().unwrap() = cfg;
                Ok(serde_json::json!({}))
            }
            "get" | "post" | "put" | "delete" | "head" => {
                self.do_request(method, payload).await
            }
            other => Err(PortError::UnsupportedMethod {
                port: self.id.clone(),
                method: other.to_string(),
            }),
        }
    }

    fn library(&self) -> Option<&'static str> {
        Some(include_str!("../../haskell/Pattern/Http.hs"))
    }

    fn as_any(&self) -> &dyn std::any::Any { self }
}

impl HttpPort {
    async fn do_request(&self, method: &str, payload: serde_json::Value)
        -> Result<serde_json::Value, PortError>
    {
        let req: RequestPayload = serde_json::from_value(payload)
            .map_err(|e| PortError::BadPayload {
                port: self.id.clone(),
                method: method.to_string(),
                message: e.to_string(),
            })?;
        let cfg = self.config.lock().unwrap().clone();
        let url = if let Some(base) = &cfg.base_url {
            format!("{}{}", base.trim_end_matches('/'), req.url)
        } else {
            req.url.clone()
        };
        let verb = match method {
            "get" => reqwest::Method::GET,
            "post" => reqwest::Method::POST,
            "put" => reqwest::Method::PUT,
            "delete" => reqwest::Method::DELETE,
            "head" => reqwest::Method::HEAD,
            _ => unreachable!(),
        };
        let mut builder = self.client.request(verb, &url);
        // Default headers.
        for (k, v) in &cfg.default_headers {
            builder = builder.header(k, v);
        }
        // Per-request headers.
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }
        // Query.
        if !req.query.is_empty() {
            builder = builder.query(&req.query.iter().collect::<Vec<_>>());
        }
        // Body.
        if let Some(body) = &req.body {
            builder = builder.body(body.clone());
        }
        // Timeout.
        if let Some(t) = cfg.timeout { builder = builder.timeout(t); }

        let response = builder.send().await
            .map_err(|e| PortError::CallFailed(self.id.clone(), e.to_string()))?;
        let status = response.status().as_u16();
        let headers: std::collections::BTreeMap<String, String> = response.headers().iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
            .collect();
        let body = response.text().await
            .map_err(|e| PortError::CallFailed(self.id.clone(), e.to_string()))?;
        let resp = ResponsePayload { status, headers, body };
        serde_json::to_value(&resp)
            .map_err(|e| PortError::CallFailed(self.id.clone(), e.to_string()))
    }
}
```

**Haskell library** (`crates/pattern_runtime/haskell/Pattern/Http.hs`):

```haskell
{-# LANGUAGE OverloadedStrings, FlexibleContexts #-}
module Pattern.Http where

import Pattern.Port (Port, call)
import Pattern.Eff (Eff, Member)
import Data.Text (Text)
import qualified Data.Aeson as A
import qualified Data.Text.Lazy as TL
import qualified Data.Text.Lazy.Encoding as TLE

-- Typed wrappers over Pattern.Port.Call("http", method, payload).
-- Returns the response body; status/headers available via getRaw.

httpGet :: Member Port effs => Text -> Eff effs Text
httpGet url = call "http" "get" (encode (A.object ["url" A..= url]))

httpPost :: Member Port effs => Text -> Text -> Eff effs Text
httpPost url body = call "http" "post" (encode (A.object ["url" A..= url, "body" A..= body]))

httpDelete :: Member Port effs => Text -> Eff effs Text
httpDelete url = call "http" "delete" (encode (A.object ["url" A..= url]))

-- Returns the raw response JSON (status, headers, body) instead of just body.
-- Use when you need to inspect status code or headers.
httpGetRaw :: Member Port effs => Text -> Eff effs Text
httpGetRaw = httpGet  -- httpGet already returns the raw response; alias for clarity

encode :: A.ToJSON a => a -> Text
encode = TL.toStrict . TLE.decodeUtf8 . A.encode
```

**Verifies:** AC5.1 mechanism.

**Verification:**
- `cargo check -p pattern-runtime`.
- Unit test in `ports/http.rs`:
    - `metadata_advertises_methods` — `metadata().methods` contains `get`, `post`, `put`, `delete`, `head`, `configure`.
    - `subscribe_returns_not_subscribable` — `subscribe(json!({}))` → `PortError::NotSubscribable`.
    - `unknown_method_returns_unsupported` — `call("invalid", json!({}))` → `PortError::UnsupportedMethod`.
    - `configure_persists` — `call("configure", json!({"base_url": "..."}))`; subsequent `call("get", json!({"url": "/x"}))` uses base_url.
    - HTTP-against-wiremock test (if wiremock available — see open question Q1): `get` returns 200 with body `"hello"`, asserts response shape.

**Commit:** `[pattern-runtime] HttpPort — first runtime-provided Port impl`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Register `HttpPort` at runtime startup

**Files:**
- Modify: `crates/pattern_runtime/src/runtime.rs` — in `TidepoolRuntime::new`, after the `port_registry` is constructed, register `HttpPort`:
    ```rust
    let _ = handle.block_on(port_registry.register(Arc::new(HttpPort::new())));
    ```
    (`handle` is the runtime's tokio Handle established for ProcessManager in Phase 3 Task 5.)

**Note on capability gate:** registration happens unconditionally at runtime startup. Per-agent visibility is enforced at handler dispatch time via `cap.has_port(&port_id)` (Phase 4 Task 6). An agent without HTTP in its CapabilitySet sees the port absent from `Port.List` and gets `CapabilityDenied` on `Port.Call`.

**Verifies:** AC5.1.

**Verification:**
- `cargo check -p pattern-runtime`.
- Unit test in `runtime.rs`: construct a `TidepoolRuntime`; `runtime.port_registry().get(&PortId::new("http"))` returns Some.

**Commit:** `[pattern-runtime] register HttpPort at TidepoolRuntime startup`
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

---

<!-- START_SUBCOMPONENT_B (tasks 3-4) -->

<!-- START_TASK_3 -->
### Task 3: Unify the three system-reminder pipelines

**Files:**
- Modify: `crates/pattern_core/src/types/message.rs` — replace the three independent variants (`FileEdits`, `ShellOutput`, `PortEvents` from Phases 2/3/4) with a single `SystemReminders(Vec<SystemReminder>)` variant where `SystemReminder` is the enum.
- Modify: `crates/pattern_runtime/src/agent_loop.rs` — `compose_request_for_turn` calls one helper `drain_and_splice_system_reminders(ctx, partial)` instead of three separate splice blocks.
- Modify: `pattern_provider::compose::passes::Segment2Pass` — one render arm for `SystemReminders` instead of three.
- Modify: `crates/pattern_runtime/src/file_manager/manager.rs`, `crates/pattern_runtime/src/process_manager/manager.rs`, `crates/pattern_runtime/src/session.rs` — the three `drain_*` accessors stay (they're now consumed inside the unified helper, not splice-side).

**Implementation:**

```rust
// pattern_core/src/types/message.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemReminder {
    FileEdit {
        path: PathBuf,
        kind: FileEditKind,   // Open | Watch
        at: jiff::Timestamp,
        diff: Option<String>,
    },
    ShellOutput {
        task_id: String,
        kind: ShellOutputKind,  // Output(String) | Exit { code, duration_ms }
        at: jiff::Timestamp,
    },
    PortEvent {
        port_id: String,
        payload: serde_json::Value,
        at: jiff::Timestamp,
    },
}

// MessageAttachment gains:
SystemReminders(Vec<SystemReminder>),
// And loses the three Phase-introduced variants (FileEdits, ShellOutput, PortEvents).
```

Helper in `agent_loop.rs`:

```rust
fn drain_and_splice_system_reminders(
    ctx: &SessionContext,
    partial: &mut PartialRequest,
) {
    let mut reminders: Vec<SystemReminder> = Vec::new();

    for evt in ctx.file_manager().drain_pending_edits() {
        reminders.push(SystemReminder::FileEdit {
            path: evt.path,
            kind: evt.kind.into(),
            at: evt.at,
            diff: evt.diff,
        });
    }
    for evt in ctx.drain_pending_shell_output() {
        reminders.push(SystemReminder::ShellOutput {
            task_id: evt.task_id.to_string(),
            kind: evt.kind.into(),
            at: evt.at,
        });
    }
    for evt in ctx.drain_pending_port_events() {
        reminders.push(SystemReminder::PortEvent {
            port_id: evt.port_id.to_string(),
            payload: evt.payload,
            at: evt.at,
        });
    }

    if !reminders.is_empty() {
        // Sort by timestamp so the agent sees events in temporal order
        // even when they came from different subsystems.
        reminders.sort_by_key(|r| match r {
            SystemReminder::FileEdit { at, .. } => *at,
            SystemReminder::ShellOutput { at, .. } => *at,
            SystemReminder::PortEvent { at, .. } => *at,
        });
        if let Some(first) = partial.messages.iter_mut().find(|m| matches!(m.role, ChatRole::User)) {
            first.attachments.push(MessageAttachment::SystemReminders(reminders));
        }
    }
}
```

Segment-2 render emits one `<system-reminder>` block per attachment, with sub-bullets per source:

```
<system-reminder>
External events since your last turn:
- 17:42:00.123 file /project/src/lib.rs (you had open) changed:
  [diff or before/after]
- 17:42:01.456 shell task abc12345:
  $ output line one
  [exited 0 in 1234ms]
- 17:42:02.789 port http event:
  { "status": 200, ... }
</system-reminder>
```

**Verifies:** AC5.2.

**Verification:**
- `cargo check --workspace`.
- Unit test for `drain_and_splice_system_reminders`:
    - `mixes_three_sources_in_temporal_order` — push events with interleaved timestamps from file/shell/port; assert the rendered order matches timestamp order, not source order.
    - `no_attachment_when_all_sources_empty` — drain returns empty; no attachment spliced.
- Update Phase 2 Task 8, Phase 3 Task 7, Phase 4 Task 5 tests where they assert on the per-source `MessageAttachment::FileEdits/ShellOutput/PortEvents` variants — those variants no longer exist; assertions move to `MessageAttachment::SystemReminders` matching on the inner enum.

**Commit:** `[pattern-runtime] [pattern-core] unify file/shell/port system reminders`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Cleanup — `canonical_effect_decls`, preamble, stub audit

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs:88-107` — `canonical_effect_decls()` test asserts the final count: 15 (16 original — Sources — Rpc + Port = 15). Verify the SdkBundle HList declaration matches.
- Modify: `crates/pattern_runtime/src/sdk/preamble.rs:6,11-13` — drop the literal "16" count (or update to 15); cleanest is to drop the count and say "the SDK effect modules" so future row changes don't require source edits to comments.
- Modify: `crates/pattern_runtime/CLAUDE.md` "Authoring agent programs" section — the "canonical 16-effect row" reference list is now 15 entries with `Port` in place of `Sources`/`Rpc`. Update the list explicitly:
    ```
    Memory, Search, Recall, Message, Display, Time, Log, Shell, File,
    Port, Mcp, Spawn, Diagnostics, Skills, Tasks
    ```
    (Skills and Tasks were added by v3-task-skill-blocks Phase 4-5 — verify the CLAUDE.md list is current at execution time; the canonical-row test in `bundle.rs` is the source of truth.)
- Audit pass: `grep -rn "is not implemented" crates/pattern_runtime/src/sdk/handlers/` should return only `mcp.rs` and `spawn.rs` after Phase 5. Anything else is a stub-leak; surface and fix.

**Verifies:** AC5.4, AC5.5.

**Verification:**
- `cargo check --workspace`.
- `cargo nextest run -p pattern-runtime --lib sdk::bundle::tests` — 15-entry assert passes.
- `grep -rn "is not implemented" crates/pattern_runtime/src/sdk/handlers/ | grep -v 'mcp\|spawn'` returns no matches.

**Commit:** `[pattern-runtime] cleanup — canonical_effect_decls=15, preamble + CLAUDE.md current`
<!-- END_TASK_4 -->

<!-- END_SUBCOMPONENT_B -->

---

<!-- START_SUBCOMPONENT_C (tasks 5-6) -->

<!-- START_TASK_5 -->
### Task 5: End-to-end smoke test

**Files:**
- Create: `crates/pattern_runtime/tests/sandbox_io_smoke.rs`.

**Test scope:** one async tokio test that constructs a real `TidepoolRuntime`, opens a real `TidepoolSession` against a mock provider, and exercises the full sandbox I/O surface in one continuous flow. Uses tempdirs for everything (file paths, mount, cache); uses `MockPort` (from Phase 4 testing module) registered alongside HttpPort; uses a mock provider that returns scripted responses driving the agent through scripted effect calls.

**Sequence:**

1. **Setup** — tempdir for project, tempdir for cache, `.pattern.kdl` with file-policy allowing the tempdir, a mock persona with full CapabilitySet (File + Shell + Port + http + mock), a scripted mock provider that emits a sequence of `assistant` messages each containing a single `code` tool call.
2. **Session open** — `runtime.open_session(persona).await?`; assert HttpPort registered, mock port registered.
3. **Step 1 — shell execute** — agent code calls `Shell.execute "echo hello"`; assert `ExecuteResult` with output `"hello\n"` + exit 0.
4. **Step 2 — file open + write** — agent opens a file in the tempdir, writes content, asserts read back matches.
5. **Step 3 — external edit** — test harness writes to the same file via `std::fs::write` from outside the agent. Wait for the SyncedDoc merge (condition-based, 5s deadline).
6. **Step 4 — next turn shows file edit reminder** — agent's next turn input contains `MessageAttachment::SystemReminders` with a `SystemReminder::FileEdit` for the path.
7. **Step 5 — shell spawn + output reminder** — agent calls `Shell.spawn "for i in 1 2 3; do echo line$i; sleep 0.05; done"`. Wait one turn boundary; assert `SystemReminder::ShellOutput` chunks contain "line1", "line2", "line3", and an `Exit` chunk.
8. **Step 6 — port call** — agent calls `Port.call "mock" "ping" "{}"`. MockPort returns scripted response. Assert response shape.
9. **Step 7 — port subscribe + event reminder** — agent calls `Port.subscribe "mock" "{}"`. Test harness pushes an event into MockPort. Wait one turn; assert `SystemReminder::PortEvent` with the right port_id.
10. **Step 8 — capability denial** — agent (with `CapabilitySet` constructed without HTTP) calls `Port.call "http" "get" {url:"http://example.com"}`. Assert error contains "CapabilityDenied".
11. **Step 9 — file policy denial** — agent writes to a path outside the policy allow list. Assert error contains "PermissionDenied" + names the rule.
12. **Cleanup** — drop session; assert no leaked threads (verify cancel cascade works) by checking thread count delta is 0 after a brief wait.

**Each step uses a labeled assertion** so AC5.6 ("error identifies which step and which assertion") is satisfied:

```rust
.with_context(|| format!("step 4: file edit reminder — expected at least one SystemReminder::FileEdit"))
```

**Concurrency** (AC5.7): all paths use tempdirs; no shared `/tmp` paths; no shared globals. Run with `cargo nextest run --test-threads=4` to verify.

**Mock provider:** if `tests/fixtures/` already has a mock-provider helper (the investigator pointed to existing tests like `session_lifecycle.rs`), reuse it; otherwise add a minimal `ScriptedMockProvider { steps: Vec<MockStep> }` that returns each step's response in order on each `complete()` call.

**Verifies:** AC5.3, AC5.6, AC5.7.

**Verification:**
- `cargo nextest run -p pattern-runtime --test sandbox_io_smoke`.
- Run 5x in a row to confirm non-flake behavior.
- Run with `--test-threads=4` alongside other integration tests.

**Commit:** `[pattern-runtime] end-to-end sandbox_io smoke test`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Workspace-wide regression sweep + final stub audit

**Files:** no code changes; verification gate.

**Verification:**

```
just pre-commit-all
```

Plus:

1. **Stub leak audit:** `grep -rn "is not implemented in v3 foundation\|is not yet implemented" crates/pattern_runtime/src/sdk/handlers/`. Expected matches: only `mcp.rs` (Plan 4 owns it) and `spawn.rs` (Plan 3 owns it). Anything else: surface as a regression and fix.
2. **Dead code audit:** `cargo clippy --all-features --all-targets -- -W dead_code` for `pattern_runtime` and `pattern_core`. Old Sources/Rpc references should be gone; any remaining `dead_code` warnings are real and need fixing.
3. **Doc-test pass:** `cargo test --doc --workspace`. The Port doctest from Phase 4 Task 1 must pass. The DataStream / SourceManager doctests from `pattern_core/src/traits/` are gone; no stale references.
4. **Test count:** record final `cargo nextest run --workspace` count. Plan 1 baseline was 646. Phase 1-5 add roughly: 8 (AC1) + 13 (AC2) + 10 (AC3) + 9 (AC4) + 1 smoke (AC5) + ~30 unit tests across phases = ~70 new. Final count should be in the 700-720 range. Numbers diverging dramatically signal silently-skipped tests; investigate.
5. **Documentation refresh:**
    - `crates/pattern_runtime/CLAUDE.md` — update the "canonical row" section + remove any Sources/Rpc references.
    - `crates/pattern_core/CLAUDE.md` — remove the data_source/source_manager mentions; add Port trait note.
    - `crates/pattern_memory/CLAUDE.md` — note the new `loro_sync` module and SyncedDoc/DirWatcher primitives if not already documented.

**Verifies:** AC5.4, AC5.5, AC5.7.

**Commit:** Only if incidental fixes needed — `[pattern-runtime] [pattern-core] [pattern-memory] sandbox-io cleanup pass`.
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_C -->

---

## Open questions for human review (foreground at end of plan-write)

**Q1: `wiremock` for HttpPort tests.** Investigator did not confirm presence in workspace. If absent, **ask orual** before adding. Alternative: skip the over-the-wire test and rely on `wiremock`-equivalent unit tests for the `do_request` payload shape (deserialize the constructed `reqwest::Request` rather than sending it). Defaulted to ask first.

**Q2: System-reminder unification scope.** Phase 5 Task 3 retroactively replaces three `MessageAttachment` variants with one. This means Phase 2/3/4 ship temporary three-variant code that gets unified here. Alternative: do the unified design from Phase 2 onward (introduce `SystemReminders` in Phase 2, all three phases write to it). Defaulted to retroactive unify because (a) Phase 2/3/4 can each be reviewed and merged independently, (b) the unification is cheap to do once all three sources exist. Flag if reviewer prefers the up-front design.

**Q3: HttpPort response body as `String` vs `Vec<u8>`.** Plan returns `body` as `String`. Binary responses (images, PDFs) get garbled or lossy-decoded. Defaults are sensible for typical agent use (text APIs); a `body_b64` alternative variant could be added later. Flag if reviewer wants the binary-safe variant in scope.

**Q4: HttpPort + auth.** Plan ships no built-in auth helpers. Bearer tokens, basic auth, etc. land via the agent passing headers manually. Defaulted to "agents handle auth via headers" — keeps the port simple. Future port impls (e.g., `OAuthPort` wrapping HttpPort) can layer auth. Flag if reviewer wants OAuth helpers in scope.

**Q5: Final test count target.** Plan estimates +70 tests (646 → ~716). Reviewer may want a tighter estimate or a stricter assert. Defaulted to a range guidance; flag if a precise post-phase number is required.
