# Pattern v3 Foundation — Phase 6: End-to-end smoke test

**Goal:** Demonstrate the full v3 foundation DoD end-to-end. Ship a minimal CLI binary that drives the smoke flow (create persona → auth → talk to Claude → write memory → simulate restart → read back → edit block → observe cache behavior). Add an API-key-mode integration test for CI and a manually-gated subscription-OAuth test. Assert cache-hit metrics directly from `TurnCacheMetrics` values returned by `Session::step`, verifying segment 1 prefix is preserved across memory edits.

**Architecture:**
- Minimal CLI lives at `crates/pattern_runtime/src/bin/pattern-test-cli.rs` — adds a `[[bin]]` target to `pattern_runtime`. Explicitly minimal, explicitly temporary. The polished CLI is deferred to a post-foundation "CLI/TUI polish" plan (already on the port-list per Phase 1) and will likely be built from scratch with ratatui — this bin is a stepping stone, not the basis.
- Uses `rustyline-async` for line input (workspace dep already; gives line editing, history, Ctrl+C handling) and raw stdout for output. No TUI, no ratatui.
- **The CLI bin + a documented checklist IS the smoke test.** Live-credential integration tests in CI are a foot-gun (credentials rotate and expire, rate-limits trigger spurious failures, per-run API costs accumulate, real failures get lost in noise). Phase 6 therefore **does not create `smoke_e2e.rs` or `smoke_e2e_oauth.rs`** — full-DoD verification is a manual procedure run through the CLI bin, captured as a checklist in `pattern_runtime/CLAUDE.md`. Design AC9.1's "deterministically" is satisfied by a repeatable documented procedure, not by auto-run tests.
- **AC9.5 (specific-error-per-step) stays automated** in `crates/pattern_runtime/tests/error_clarity.rs` — these tests verify error messages are specific and don't need live credentials. They run in CI as normal integration tests.
- `Session::step()` returns `(TurnOutput, TurnCacheMetrics)` per Phase 5. The CLI bin displays the metrics per turn so humans can eyeball segment-1/2/3 cache hit ratios during the manual smoke flow.
- Restart is simulated by exiting the CLI and re-running `pattern-v3 spawn` against the same data_dir. Pattern_db's tempfile persistence preserves the state across bin invocations.

**Tech Stack:** Rust 2024, `clap` derive for arg parsing (matches existing `pattern_cli` convention), `rustyline-async`, `tempfile` for test-dir isolation, `tokio::test` async harness.

**Scope:** Phase 6 of 6. Covers v3-foundation.AC9.*.

**Codebase verified:** 2026-04-16

---

## Acceptance Criteria Coverage

### v3-foundation.AC9: End-to-end foundation demonstration

- **v3-foundation.AC9.1 Success:** API-key smoke test in CI passes deterministically: create persona → auth → send message → receive response → write block → persist → restart → read-back matches → edit block → next turn shows expected cache behavior
- **v3-foundation.AC9.2 Success:** Manual subscription-OAuth smoke test passes on user's machine with a valid `~/.claude/session.json`; procedure documented in test file header
- **v3-foundation.AC9.3 Success:** Minimal CLI entry point drives the full flow from the command line (functional, not polished)
- **v3-foundation.AC9.4 Success:** Cache-hit metric assertions pass: segment 1 hit rate stays high across block edits; segment 3 invalidates as expected
- **v3-foundation.AC9.5 Failure:** Any step failing in the smoke flow (persona creation, auth, message send, memory write, restart, read-back, edit, cache metric) causes the smoke test to fail loudly with a specific error pointing at which step failed

---

## Executor Context

**Repo root:** `/home/orual/Projects/PatternProject/pattern`
**Working bookmark:** `rewrite-v3`
**Pre-phase state after Phase 5:** All foundation machinery in place — `pattern_core` (traits + types + errors + memory storage + base_instructions), `pattern_runtime` (Tidepool + effect handlers + Session/AgentRuntime impl + memory adapter + change_log), `pattern_provider` (three-tier auth + honest shaper + rate limiting + count_tokens + provider impl + three-segment cache composer + break-detection + cache-hit metrics). Rust-genai fork rebased onto upstream with minimal patches.

**Phase 6 adds:** one binary target, two integration test files, optional filter to CI config, documentation updates in `pattern_runtime/CLAUDE.md`.

**Existing test patterns to mirror** (from investigation):
- `crates/pattern_core/tests/config_merge.rs:125-145` — test setup template with `RuntimeContext::builder()`
- `crates/pattern_core/tests/embeddings_test.rs:7-15` — env-gated test pattern (`#[ignore = "..."]` + `std::env::var()` double-gate)
- `crates/pattern_cli/src/main.rs:46-100` — clap derive CLI structure (reference only; pattern-v3 bin is simpler)

**Conventions:**
- Commit prefix: `[pattern-runtime]` for the bin + tests; `[meta]` for CI config / docs updates.
- CI command: `cargo nextest run` (default) excludes `#[ignore]`-ed tests. Manual OAuth test runs via `PATTERN_V3_MANUAL_OAUTH=1 cargo nextest run --test smoke_e2e_oauth -- --ignored`.
- Test runner: `cargo nextest run` workspace-wide for the final phase-close.

**Design reference:** `docs/design-plans/2026-04-16-v3-foundation.md` Phase 6 (lines 404-423).

**Persona / agent terminology:** The codebase uses `AgentConfig` for what the design calls "persona." Phase 6 tests use `AgentConfig` without renaming — terminology migration (persona ↔ agent) is not foundation scope. If Phase 2's new type `PersonaSnapshot` is the canonical v3 persona handle, Phase 6 uses that where trait boundaries demand it and `AgentConfig` elsewhere.

---

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->
<!-- START_TASK_1 -->
### Task 1: Add `pattern-v3` bin target to pattern_runtime

**Verifies:** AC9.3 infrastructure.

**Files:**
- Modify: `crates/pattern_runtime/Cargo.toml` — add `[[bin]]` section + bin-only deps
- Create: `crates/pattern_runtime/src/bin/pattern-v3.rs`

**Step 1: Cargo.toml**

```toml
[[bin]]
name = "pattern-v3"
path = "src/bin/pattern-v3.rs"

[dependencies]
# ... existing deps ...
# Bin-target-only: clap (arg parsing), rustyline-async (line input).
clap = { workspace = true, features = ["derive"] }
rustyline-async = { workspace = true }
```

If `clap` and `rustyline-async` aren't in `[workspace.dependencies]` yet, add them (they're used by pattern_cli so may already be there).

**Step 2: bin/pattern-v3.rs skeleton**

```rust
//! Minimal smoke-test CLI for Pattern v3 foundation.
//!
//! EXPLICITLY NOT POLISHED. This binary exists to drive the Phase 6 smoke
//! test manually and to demonstrate AC9.3. The polished CLI is deferred to
//! a post-foundation "CLI/TUI polish" plan — likely built from scratch with
//! ratatui. Do not extend this binary beyond what the smoke test needs.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "pattern-v3", about = "Pattern v3 foundation smoke-test CLI")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open a session against a persona config and chat interactively.
    /// Drives the full DoD flow for manual verification (AC9.3).
    Spawn {
        /// Path to an AgentConfig / PersonaSnapshot TOML file.
        persona: std::path::PathBuf,

        /// Optional override of database directory for this session.
        /// Defaults to $XDG_DATA_HOME/pattern/v3/ or ~/.local/share/pattern/v3/.
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,

        /// Force a specific auth tier instead of resolving automatically.
        /// Useful for smoke-test reproducibility.
        #[arg(long, value_enum)]
        auth: Option<AuthTierCli>,
    },
}

#[derive(Clone, Debug, clap::ValueEnum)]
enum AuthTierCli {
    SessionPickup,
    Pkce,
    ApiKey,
}

#[tokio::main]
async fn main() -> miette::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Spawn { persona, data_dir, auth } => spawn(persona, data_dir, auth).await,
    }
}

async fn spawn(
    persona_path: std::path::PathBuf,
    data_dir: Option<std::path::PathBuf>,
    auth_override: Option<AuthTierCli>,
) -> miette::Result<()> {
    // 1. Load persona from TOML.
    let persona = load_persona(&persona_path)?;
    // 2. Resolve data directory.
    let data_dir = data_dir.unwrap_or_else(default_data_dir);
    // 3. Initialize pattern_db in data_dir.
    let dbs = pattern_db::ConstellationDatabases::open(&data_dir).await?;
    // 4. Build AgentRuntime (TidepoolRuntime from Phase 3).
    let runtime = pattern_runtime::TidepoolRuntime::with_default_sdk();
    // 5. Construct ProviderClient with the chosen auth override (Phase 4).
    let provider = build_provider(auth_override).await?;
    // 6. Open session (Phase 3+5).
    let mut session = runtime.open_session(persona.into_snapshot()).await?;

    // 7. Obtain the session's DisplayHandler so the REPL can register the
    //    CLI subscriber before driving turns. The handle is a cheap Arc clone;
    //    the same subscriber list is shared with MessageHandler's clone inside
    //    the bundle.
    let display = session.display();

    // 8. Interactive loop using rustyline-async.
    run_repl(&mut session, display).await
}

// ... helpers: load_persona, default_data_dir, build_provider, run_repl ...
```

**Step 3: REPL loop with live chunk streaming**

The CLI registers a `DisplaySubscriber` on the session's DisplayHandler (Phase 3 Task 10). Stream chunks from the provider forward live to stdout; the final assembled content and cache metrics print when the stream ends.

```rust
use pattern_runtime::sdk::handlers::{DisplayEvent, DisplaySubscriber};
use std::sync::{Arc, Mutex};

struct CliDisplaySubscriber {
    stdout: Arc<Mutex<rustyline_async::SharedWriter>>,
}

impl DisplaySubscriber for CliDisplaySubscriber {
    fn on_event(&self, event: &DisplayEvent) {
        use std::io::Write;
        let mut out = self.stdout.lock().unwrap();
        match event {
            // Typewriter effect: write chunks as they arrive, no newline.
            DisplayEvent::Chunk(s) => { let _ = write!(out, "{s}"); let _ = out.flush(); }
            // Final event: ensure we end the current line before the prompt returns.
            DisplayEvent::Final(_) => { let _ = writeln!(out); }
            // Notes render dimmed, on their own line, without interrupting the stream too much.
            DisplayEvent::Note(s) => { let _ = writeln!(out, "  (·) {s}"); }
        }
    }
}

async fn run_repl(
    session: &mut impl pattern_core::traits::Session,
    display: pattern_runtime::sdk::handlers::DisplayHandler,
) -> miette::Result<()> {
    use rustyline_async::{Readline, ReadlineEvent};
    let (mut rl, stdout) = Readline::new("pattern> ".into())?;
    let stdout = Arc::new(Mutex::new(stdout));

    // Register CLI as a display subscriber. Chunks from the provider flow
    // through DisplayHandler → CliDisplaySubscriber → stdout.
    display.subscribe(Arc::new(CliDisplaySubscriber { stdout: stdout.clone() }));

    loop {
        match rl.readline().await? {
            ReadlineEvent::Line(line) => {
                let line = line.trim();
                if line.is_empty() { continue; }
                if line == ":q" || line == ":quit" { break; }

                let input = TurnInput::user_message(line.to_string());
                match session.step(input).await {
                    Ok((_output, metrics)) => {
                        // Output content was already streamed via DisplaySubscriber.
                        // Just print the cache-metrics summary after the turn.
                        let mut out = stdout.lock().unwrap();
                        writeln!(out, "  [cache: seg1={:.0}% seg2={:.0}% seg3={:.0}%]",
                            metrics.segment_1_hit_ratio() * 100.0,
                            metrics.segment_2_hit_ratio() * 100.0,
                            metrics.segment_3_hit_ratio() * 100.0,
                        )?;
                    }
                    Err(e) => {
                        let mut out = stdout.lock().unwrap();
                        writeln!(out, "error: {}", e)?
                    }
                }
            }
            ReadlineEvent::Eof | ReadlineEvent::Interrupted => break,
        }
    }
    Ok(())
}
```

Not polished — typewriter-style streaming to stdout, cache percentages after every turn as a debug aid. After `TidepoolSession::open` returns, the CLI calls `session.display()` to obtain a cheap clone of the session's DisplayHandler (its subscriber list is Arc-shared with the MessageHandler's clone inside the bundle), then subscribes `CliDisplaySubscriber` onto it. Bundle-side chunks emitted during `session.step()` fan out to every registered subscriber.

**Step 4: Verify**

```bash
cargo build -p pattern_runtime --bin pattern-v3
./target/debug/pattern-v3 --help
```

Help output shows the `spawn` command. No runtime calls yet.

**Commit:**

```bash
jj describe -m "[pattern-runtime] pattern-v3 bin target for phase 6 smoke test (AC9.3 infrastructure)"
jj new
```
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Persona TOML loader + minimal example fixture

**Verifies:** contributes to AC9.3.

**Files:**
- Create: `crates/pattern_runtime/src/bin/persona_loader.rs` (small module the bin uses)
- Create: `crates/pattern_runtime/tests/fixtures/smoke_persona.toml` (example persona for smoke tests + manual CLI use)

**Step 1: Persona loader**

```rust
// crates/pattern_runtime/src/bin/persona_loader.rs
// Small helper for the pattern-v3 bin. Reads a TOML file matching the
// AgentConfig schema Pattern's ecosystem already uses; converts into
// PersonaSnapshot (Phase 2 type) for AgentRuntime consumption.

use pattern_core::types::PersonaSnapshot;

pub fn load_persona(path: &std::path::Path) -> miette::Result<PersonaSnapshot> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| miette::miette!("reading persona file {}: {e}", path.display()))?;
    let config: pattern_core::config::AgentConfig = toml::from_str(&contents)
        .map_err(|e| miette::miette!("parsing persona TOML: {e}"))?;
    config.into_snapshot()
        .map_err(|e| miette::miette!("converting AgentConfig to PersonaSnapshot: {e}"))
}
```

**Step 2: Smoke fixture**

```toml
# crates/pattern_runtime/tests/fixtures/smoke_persona.toml
name = "pattern-smoke-test"

[model]
provider = "anthropic"
model = "claude-sonnet-4-6"
# Reasoning off by default (Phase 4 default).

[memory.core]
content = "Test persona for Pattern v3 foundation smoke test. I am a minimal agent used to verify the end-to-end flow."
memory_type = "Working"
permission = "ReadWrite"
pinned = true

[memory.scratchpad]
content = ""
memory_type = "Working"
permission = "ReadWrite"
pinned = false
```

Intentionally minimal. Real personas live elsewhere; this is just enough to drive the smoke flow.

**Commit:**

```bash
jj describe -m "[pattern-runtime] persona TOML loader + smoke-test fixture"
jj new
```
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Wire auth tier override + provider construction in the bin

**Verifies:** AC9.3 — bin drives full flow.

**Files:**
- Modify: `crates/pattern_runtime/src/bin/pattern-v3.rs` — fill in `build_provider()` with Phase 4 wiring

**Implementation:**

```rust
async fn build_provider(
    auth_override: Option<AuthTierCli>,
) -> miette::Result<pattern_provider::AnthropicProviderClient> {
    use pattern_provider::{AnthropicProviderClient, AuthResolver, ShaperConfig, ShaperCompatMode};

    // Build auth resolver per Phase 4. Override narrows tiers if requested.
    let resolver = match auth_override {
        None => AuthResolver::default(),
        Some(AuthTierCli::SessionPickup) => AuthResolver::session_pickup_only(),
        Some(AuthTierCli::Pkce) => AuthResolver::pkce_only(),
        Some(AuthTierCli::ApiKey) => AuthResolver::api_key_only(),
    };

    let shaper = ShaperConfig::default()
        .with_compat_mode(ShaperCompatMode::default());
    // ShaperCompatMode::default() is SubscriptionRoutingShape when the
    // `subscription-oauth` feature is enabled (Phase 4 default), HonestPattern
    // when it's disabled.

    AnthropicProviderClient::builder()
        .auth_resolver(resolver)
        .shaper_config(shaper)
        .build()
        .await
        .map_err(|e| miette::miette!("building provider: {e}"))
}
```

**Step 1:** Add the missing `AuthResolver` constructors if Phase 4 didn't ship them (tier-restricted variants for test reproducibility). Small additions: `session_pickup_only()`, `pkce_only()`, `api_key_only()`.

**Step 2:** Verify manually.

```bash
cargo build -p pattern_runtime --bin pattern-v3
ANTHROPIC_API_KEY=... ./target/debug/pattern-v3 spawn \
    crates/pattern_runtime/tests/fixtures/smoke_persona.toml \
    --auth api-key
```

Expected: REPL starts. Type a short message; agent responds. Cache percentages print. Ctrl+D exits cleanly.

**Commit:**

```bash
jj describe -m "[pattern-runtime] pattern-v3 bin: auth tier override + provider construction (AC9.3)"
jj new
```
<!-- END_TASK_3 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-5) -->
<!-- START_TASK_4 -->
### Task 4: Document the smoke-test checklist in `pattern_runtime/CLAUDE.md`

**Verifies:** AC9.1, AC9.2, AC9.3, AC9.4 — all verified manually via the CLI bin following this checklist.

**Files:**
- Modify: `crates/pattern_runtime/CLAUDE.md` — add "Smoke test procedure" section

**Content to add:**

```markdown
## Smoke test procedure

The v3 foundation smoke test is a **manual procedure** driven through the
`pattern-v3` CLI binary. Live-credential tests in CI are a foot-gun
(expiring credentials, rate-limit noise, per-run $$, real failures lost in
noise), so Phase 6 ships a CLI + this checklist rather than an automated
integration test with live Anthropic calls.

The failure-mode tests at `tests/error_clarity.rs` run in CI and verify
errors are specific per step (AC9.5). Everything below is manual.

### Setup (one-time per machine)

1. Ensure `tidepool-extract` is on `$PATH` (see Phase 3 setup notes).
2. Build: `cargo build -p pattern_runtime --bin pattern-v3`.
3. Pick an auth path:
   - **API-key:** set `ANTHROPIC_API_KEY` in the environment.
   - **OAuth (subscription):** either have an active claude-code session at
     `~/.claude/.credentials.json` (session-pickup tier resolves it), or
     run one-time PKCE flow (described at Step 2a below).

### DoD flow — verifies AC9.1 (API-key) / AC9.2 (OAuth) / AC9.3 (CLI drives it) / AC9.4 (cache behavior)

In a scratch directory:

**Step 1: start a fresh session.**

```bash
TMPDIR=$(mktemp -d)
cargo run -p pattern_runtime --bin pattern-v3 -- \
    spawn crates/pattern_runtime/tests/fixtures/smoke_persona.toml \
    --data-dir "$TMPDIR"
# CLI prints: "pattern> "
```

For OAuth verification, add `--auth pkce` (or `--auth session-pickup`).

**Step 2: talk to Claude (AC9.1 step 1-3).**

Type: `hello; what's your role?`
Expected: agent responds with role description consistent with the smoke persona.

**Step 2a (one-time PKCE flow if using `--auth pkce`):** CLI prints an auth
URL, opens browser, paste back the `code#state` string. Token gets stored
in keyring (or JSON fallback at `~/.config/pattern/creds/anthropic.json`).

**Step 3: write a memory block (AC9.1 step 4).**

Type: `please remember in your scratchpad: favorite color is teal.`
Expected:
- Agent confirms.
- After the turn completes, CLI prints a cache-metrics line including `seg1`, `seg2`, `seg3` percentages.
- Agent invoked `memory.write` (verify by checking the CLI's change-log debug output if enabled, or by step 5 below).

**Step 4: exit + re-spawn against the same data dir (AC9.1 step 5).**

Type `:q` or Ctrl+D. CLI exits.
Run the same `spawn` command with the same `--data-dir` to reopen.

**Step 5: recall the stored value (AC9.1 step 6).**

Type: `what's my favorite color?`
Expected: agent responds with "teal" (or a clear recall of the stored value). Persistence worked.

**Step 6: capture pre-edit cache metrics (AC9.1 step 7).**

Type: `ok, thanks`
Observe and note the `seg1`, `seg3` percentages printed.

**Step 7: edit the block externally, then next turn (AC9.1 step 8, AC9.4).**

Leave the CLI running. From another shell:

```bash
pattern-v3 edit-block --data-dir "$TMPDIR" scratchpad "favorite color is actually indigo"
```

(Or use whatever `edit-block` subcommand the CLI exposes — if it doesn't
have one, add a `:edit-block <label> <content>` REPL command during this
task, since direct block edit is central to AC9.4.)

Back in the original CLI, type: `confirm update`.
Expected cache behavior:
- `seg1` ratio within 5% of pre-edit (AC8.1 / AC9.4: system prefix preserved).
- `seg3` ratio substantially lower than pre-edit (AC8.2 / AC9.4: memory pseudo-turn invalidated).

Record the numbers. If segment 1 drops more than 5%, that's a real failure — segment 1 shouldn't be affected by a memory block edit.

### When things fail

- If any step surfaces an unclear error, that's an AC9.5 regression — the error-clarity tests should have caught it; add a new test case at `tests/error_clarity.rs` before debugging further.
- If segment 1 invalidates unexpectedly, check break-detection output via `tracing::warn` logs — Phase 5 Task 11 wires this.
```

**Step 1:** Write the checklist section into `pattern_runtime/CLAUDE.md`.

**Step 2:** If the CLI doesn't already expose a way to edit blocks externally, add a REPL command during this task (e.g., `:edit-block <label> <content>` processed inside the `run_repl` loop from Task 3). This is a small addition and belongs with the smoke-test verification path since AC9.4 depends on it.

**Step 3:** Run the checklist end-to-end manually. Capture any rough edges (unclear CLI output, missing info in cache metrics, etc.) and fix before committing. The checklist should produce a consistent, boring "it works" experience when followed.

**Commit:**

```bash
jj describe -m "[pattern-runtime] smoke test procedure checklist (AC9.1, AC9.2, AC9.3, AC9.4)"
jj new
```
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `error_clarity.rs` — automated per-step failure tests

**Verifies:** AC9.5.

**Files:**
- Create: `crates/pattern_runtime/tests/error_clarity.rs`

**Implementation:**

These tests run in CI as normal integration tests. They verify that every failure mode in the Phase 6 flow produces a specific error message pointing at which step failed — so when the smoke-test checklist hits an error, the CLI output tells the human exactly what broke.

```rust
//! Automated tests for AC9.5: every step of the smoke flow produces a
//! specific error message when it fails. No live credentials needed.

use pattern_provider::{AnthropicProviderClient, AuthResolver};

#[tokio::test]
async fn ac9_5_persona_parse_failure_is_specific() {
    let bad_toml = "not valid toml at all";
    let result = pattern_runtime::bin_support::parse_persona_str(bad_toml);
    let err = result.expect_err("bad toml should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("parsing") || msg.contains("persona"),
        "error should point at parse step: {msg}",
    );
}

#[tokio::test]
async fn ac9_5_auth_no_tier_available_is_specific() {
    // Clear ANTHROPIC_API_KEY; point at api-key-only tier; expect specific error.
    let orig = std::env::var("ANTHROPIC_API_KEY").ok();
    std::env::remove_var("ANTHROPIC_API_KEY");

    let result = AnthropicProviderClient::builder()
        .auth_resolver(AuthResolver::api_key_only())
        .build()
        .await;
    let err = result.expect_err("no credentials should fail");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("no auth") || msg.contains("api_key") || msg.contains("credential"),
        "error should point at auth step: {msg}",
    );

    if let Some(key) = orig { std::env::set_var("ANTHROPIC_API_KEY", key); }
}

#[tokio::test]
async fn ac9_5_session_open_with_invalid_data_dir_is_specific() {
    // Point TidepoolRuntime at a path that can't be created (e.g., under /proc).
    // Expect error mentioning "data dir" or similar.
    ...
}

#[tokio::test]
async fn ac9_5_memory_write_to_unknown_handle_is_specific() {
    // Build an in-memory memory store; attempt write to a nonexistent BlockHandle;
    // expect MemoryError::BlockNotFound { handle, available } with populated `available` list.
    ...
}

// Similar tests for: provider build failure, restart with corrupt db, token
// count failure surfacing, rate-limit exhaustion messaging. One test per
// failure mode in the smoke flow.
```

**Step 1:** Write the tests. Each one deliberately triggers a failure mode and asserts the error message is specific + actionable.

**Step 2:** `cargo nextest run -p pattern_runtime --test error_clarity` passes in CI without any credentials.

**Step 3:** If any existing error message is too generic to satisfy the assertion, fix the error variant or its Display impl to be more specific. Error clarity is the deliverable; tests just verify.

**Commit:**

```bash
jj describe -m "[pattern-runtime] error_clarity.rs: per-step failure-mode tests (AC9.5)"
jj new
```
<!-- END_TASK_5 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (task 6) -->
<!-- START_TASK_6 -->
### Task 6: Phase 6 close — zero warnings + audit + full workspace test

**Verifies:** cleanliness gates + cross-phase verification.

**Step 1: Compile check.**

```bash
cargo check --workspace 2>&1 | tee /tmp/phase6-check.log
cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tee /tmp/phase6-clippy.log
cargo doc --workspace --no-deps 2>&1 | tee /tmp/phase6-doc.log
```

All three zero-warning.

**Step 2: Full workspace test suite.**

```bash
cargo nextest run --workspace 2>&1 | tail -50
cargo test --doc --workspace
```

Expected: all tests pass. Smoke tests skip if credentials unavailable (with clear skip messages).

**Step 3: CI smoke run.**

If CI config lives at `.github/workflows/`, verify the new `smoke_e2e` test is included (it will be by default since `cargo nextest run --workspace` picks up integration tests automatically). If CI needs `ANTHROPIC_API_KEY` set as a secret, document this in the commit message as a required CI config step.

**Step 4: Audit script.**

```bash
bash scripts/audit-rewrite-state.sh
```

Must pass.

**Step 5: Full DoD checklist manual walk.**

Walk through the design's Definition of Done (lines 9-54 of design plan) item by item, assert each is satisfied:

- ✅ `rewrite-v3` branch/bookmark exists, cut from pre-rewrite-v3 marker
- ✅ Workspace narrowed to active crates; port-list doc tracks exclusions
- ✅ pattern_core trait definitions landed
- ✅ Tidepool embedded via FFI with external CPU/wall timeout wrapping
- ✅ Minimal agent loop works (program → LLM → response → next turn)
- ✅ freer-simple SDK handlers for memory/message/shell/file/sources/mcp/time/ipc/log (stubs where appropriate)
- ✅ Turn-level checkpoint + restore
- ✅ pattern_provider with three-tier auth resolution
- ✅ Request shaping with honest identification
- ✅ Per-provider token-bucket rate limiting
- ✅ Provider-session UUID rotation
- ✅ Provider-reported token counting replaces heuristic
- ✅ pattern_memory storage preserved
- ✅ Rendering layer revised (three-segment cache with pre-turn pseudo-turn)
- ✅ Three-segment cache layout with TTL variants
- ✅ DEFAULT_BASE_INSTRUCTIONS preserved
- ✅ Smoke test documented + exercised manually via CLI checklist (AC9.1/9.2/9.3/9.4); AC9.5 failure-mode tests in CI via `error_clarity.rs`. Design AC9.1's "deterministically" is satisfied by the repeatable documented procedure rather than an auto-run live-credential test (deliberate divergence from design's literal "on CI" text — see Phase 6 Architecture section, confirmed intentional by user during planning).

Document any DoD item that needs follow-up in the commit message.

**Commit:**

```bash
jj describe -m "[meta] phase 6 complete: v3 foundation end-to-end demonstration passes

Phase 6 summary:
- pattern-v3 minimal CLI bin added to pattern_runtime (rustyline-async REPL, persona TOML loader, auth tier override)
- Smoke-test procedure documented as manual checklist in pattern_runtime/CLAUDE.md
  (AC9.1, AC9.2, AC9.3, AC9.4 all verified via CLI + checklist; no live-credential
  tests in CI by deliberate design choice — see phase-6 Architecture section)
- error_clarity.rs: automated per-step failure-mode tests covering AC9.5 in CI

Cross-phase verification:
- cargo check --workspace: zero warnings
- cargo clippy --all-features --all-targets -- -D warnings: clean
- cargo doc --workspace --no-deps: clean
- cargo nextest run --workspace: all tests pass (smoke tests skip gracefully without credentials)
- scripts/audit-rewrite-state.sh: clean
- just pre-commit-all: passes

All v3 foundation DoD items verified against design plan §Definition of Done.

Foundation ready for post-foundation plans: memory-fs redesign, subagent
primitives, plugin system, MCP integration, social plugin migration, v2→v3
migrator, CLI/TUI polish (ratatui), compaction enhancements."
jj new
```
<!-- END_TASK_6 -->
<!-- END_SUBCOMPONENT_C -->

---

## Phase 6 "Done when" checklist

- [ ] Retire Phase-3-era static-program session machinery: remove the `SessionMachine` + `SdkBundle` fields kept on `TidepoolSession` under Phase 5 Task 20 for test-fixture compatibility. With the agent-loop production path fully exercised and the smoke test passing, any remaining tests that depend on the pre-compiled-agent-program path get rewritten against the agent-loop entry points, and the dead fields + `InnerState` scaffolding they supported come out. See `crates/pattern_runtime/src/session.rs` for the fields to remove; update call sites accordingly.
- [ ] **Env setup for Haskell eval worker: `TIDEPOOL_PRELUDE_DIR`** (follow-up from Phase 5 Task 20). The agent-loop eval worker compiles `code`-tool snippets via `tidepool-extract`. The preamble (adapted from tidepool-mcp) imports `Tidepool.Prelude`, `Tidepool.Aeson`, `Tidepool.Aeson.KeyMap`, etc. — modules that live in the tidepool haskell `lib/` source tree, NOT bundled with the `tidepool-extract` binary itself. For agent programs to compile, the lib/ directory must be on GHC's include path at eval time.
    1. Nix devshell (`nix/modules/devshell.nix`): expose `TIDEPOOL_PRELUDE_DIR = "${inputs.tidepool}/haskell/lib"` so interactive shells + CI get the path for free. Requires the tidepool flake input's source tree to be accessible as `inputs.tidepool` (already is — we take the whole flake, not just the `tidepool-extract` package).
    2. `pattern_runtime::preflight::check()`: add a warning (not an error) when `TIDEPOOL_PRELUDE_DIR` isn't set, pointing at the same setup docs as the `tidepool-extract` check.
    3. `TidepoolSession::open` (or wherever the eval worker is wired — Task 20 part 5e): read `TIDEPOOL_PRELUDE_DIR` and pass `[sdk_dir, prelude_dir]` to `EvalWorker::spawn_with_includes`. Fall back to `sdk_dir` only with a tracing warning when unset (agents that only use `Pattern.*` effects still work, just anything that pulls in Tidepool utilities will fail with "Could not find module Tidepool.X").
    4. Tidepool fork (if needed): consider adding a `tidepool-haskell-lib` flake output that packages `haskell/lib/` as a derivation with the right store-path stability. Not strictly required for local use since `${inputs.tidepool}/haskell/lib` works directly, but might be needed if tidepool's flake restructures.
    5. Test gating: the existing `dispatch_evaluates_trivial_haskell_snippet_end_to_end` in `agent_loop/eval_worker.rs` already skips cleanly when `TIDEPOOL_PRELUDE_DIR` is unset; once the devshell sets it, the test starts running automatically.
- [ ] **Evaluate migrating pattern_db from sqlx to rusqlite** (follow-up from Phase 5 Task 20 design discussion). The agent-loop eval worker currently spawns a multi-thread tokio runtime (2 worker threads + default blocking pool) solely so handlers can drive async sqlx calls from inside the synchronous `compile_and_run` body. With sync rusqlite:
    1. `MemoryStore` trait becomes sync. No `async_trait`, no `block_on` at handler boundaries.
    2. Eval worker can drop the tokio runtime entirely — just `std::thread` + `std::sync::mpsc`. Simpler, fewer threads per session, smaller footprint.
    3. SQLite doesn't benefit from async anyway; its operations are blocking CPU/IO. sqlx's async wrapper is pure overhead.
    4. Trade-off: migration churn touches every memory-store callsite + pattern_db's query surface. Not trivial. Other async callers (pattern_server HTTP, pattern_discord) still want async, so they'd block_on the sync store at their boundaries.
    5. Scope: evaluate whether the simplification is worth the migration. If yes, plan it as a separate implementation plan distinct from phase 6. If no, leave sqlx + document the multi-thread runtime in the eval worker as permanent.
- [ ] **genai fork patch: wire Anthropic thinking preservation on the outbound path** (follow-up from Phase 5 Task 20). The fork's `rust-genai` already models thinking-block preservation via `ContentPart::{ThoughtSignature, ReasoningContent}`, `ToolCall.thought_signatures`, `StreamEnd::into_assistant_message_for_tool_use()`, and `ChatMessage::assistant_tool_calls_with_thoughts()` — but the Anthropic adapter never wires these for reals:
    1. `crates/rust-genai/src/adapter/adapters/anthropic/streamer.rs` — the streamer emits `ThoughtSignatureChunk` events during streaming but its `InterStreamEnd` construction sets `captured_thought_signatures: None`. Track signatures per `InProgressBlock::Thinking` during the stream (or accumulate into a `Vec<String>` on the captured-data struct) and populate `InterStreamEnd.captured_thought_signatures` at stream end.
    2. `crates/rust-genai/src/adapter/adapters/anthropic/adapter_impl.rs` — lines 681-682 and 725-726 currently ignore `ContentPart::ThoughtSignature` and `ContentPart::ReasoningContent` on outbound message serialization. Emit them as proper Anthropic wire blocks: `{"type": "thinking", "thinking": "<text>", "signature": "<sig>"}`. Pair a reasoning-content part with its adjacent signature part (they belong together in one signed block); multiple thinking blocks per response (interleaved thinking mode) each get their own pair. A previous Pattern branch had a version of this patch that offloaded pairing onto the library user — don't port that approach; handle pairing inside the adapter. Scope is small (~50 lines across the two files) now that we know the shape.
    3. Pattern side: no code changes needed once the fork patch lands. Task 20 part 5c's agent loop passes the assistant `ChatMessage` through to the composer unchanged; when the adapter starts serialising thinking parts correctly, Extended Thinking with tool_use starts working end-to-end automatically. Add a phase-6 regression test (via wiremock) that asserts a thinking block captured on turn N appears verbatim as a `{"type":"thinking",...}` content block in the request payload for turn N+1.
    4. Rationale for deferring: Phase 5's user-visible functionality (single-turn + tool-use cycles without thinking) works without this patch; Extended Thinking merely degrades (UI still sees `TurnEvent::Thinking` chunks mid-stream, but the model can't continue its reasoning chain across tool cycles because the follow-up request strips thinking). Fixing this properly requires a clean adapter patch, not a Pattern-side workaround — so it belongs here, not shoehorned into Task 20.
- [ ] `pattern-v3` bin target added to `pattern_runtime` with clap + rustyline-async input
- [ ] `spawn <persona>` subcommand loads persona, opens session, drives REPL, prints cache metrics per turn
- [ ] CLI exposes a way to directly edit a memory block (REPL command or separate subcommand) — required by the smoke-test checklist step 7
- [ ] `smoke_persona.toml` fixture exists with minimal viable persona
- [ ] Smoke-test procedure checklist documented in `pattern_runtime/CLAUDE.md` covers AC9.1 / AC9.2 / AC9.3 / AC9.4 (manual, run through the CLI bin)
- [ ] `error_clarity.rs` automated tests cover AC9.5 for parse / auth / provider-build / session-open / memory-write / restart paths without live credentials
- [ ] Cache-hit metrics display in CLI per turn; documented what "good" looks like in the checklist
- [ ] Smoke-test checklist executed end-to-end by a human before phase close; any rough edges fixed
- [ ] `cargo check --workspace`, `clippy`, `doc` all zero-warning
- [ ] `cargo nextest run --workspace` all tests pass (no live-credential tests exist)
- [ ] `scripts/audit-rewrite-state.sh` passes
- [ ] `just pre-commit-all` passes
- [ ] DoD checklist from design plan §Definition of Done walked and verified

## What this phase deliberately does NOT do

- **Does not create automated live-credential smoke tests.** No `smoke_e2e.rs` or `smoke_e2e_oauth.rs` integration tests exist. Live-credential tests in CI are a foot-gun (credentials rotate + expire, rate-limit noise, per-run API cost, real failures lost in noise). The CLI bin + manual checklist is the smoke-test vehicle. Phase 6 captures the verification procedure in a repeatable form; automating it (via e.g. a nightly manual-trigger workflow that pipes canned inputs through the bin) is a future plan if we decide it's worth the operational cost.
- Does not build a polished CLI UX. `pattern-v3` bin is minimal; proper CLI lives in the post-foundation CLI/TUI polish plan (likely rebuilt from scratch with ratatui).
- Does not retire `pattern_cli` from the port-list doc — it stays as "deferred to CLI/TUI polish plan." The polished CLI will likely be a new crate, not a resurrection of `pattern_cli`.
- Does not implement non-Anthropic provider smoke paths (OpenAI, Gemini, Ollama). Pattern's foundation target is Anthropic; other providers require separate design plans.
- Does not implement multi-agent constellation smoke paths (coordination patterns, subagent spawning). Foundation is single-agent; multi-agent is the subagent-primitives plan.
- Does not exercise data-sources (Bluesky, Discord, shell-as-source). Those are future plugin-migration scope.
- Does not verify against Vertex / Bedrock / Foundry providers. First-party Anthropic API only.
- Does not benchmark throughput / latency. Observability beyond cache-hit metrics is a future plan.
