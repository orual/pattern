# Spawn & Fork Improvements

**Last updated:** 2026-05-09

**Status:** working doc — tackling in order

## What we're improving

Three issues with the current spawn/fork machinery, plus a small precursor batch surfaced while setting up our own task tracking. Ordered by dependency:

**Precursor (do first, small warmup):**

- **A. Tasks.create auto-creates TaskList block when missing.** Single entry point for task management — agents shouldn't have to bootstrap blocks separately. `pattern_runtime/src/sdk/handlers/tasks.rs` change.
- **B. Memory.create undeletes soft-deleted blocks instead of erroring on UNIQUE conflict.** Currently a `Memory.delete` followed by `Memory.create` with the same label fails; the row is reserved but unreadable.

These are small, immediately useful (we tripped over both trying to set up our own task tracker), and after they land we can use `Tasks.create` directly for the rest of this work. Surfaced 2026-05-09.

**Main batch:**


1. **TUI attribution + ephemeral mailboxes (do first).** Spawn output isn't attributed at the wire level, so the TUI can't route ephemeral progress to a sidebar. Ephemerals also don't get mailboxes — once spawned they run to completion with no way to interact. Adding both at once gives us the diagnostic substrate for issue (2): we can DM a stuck ephemeral and get information back.

2. **SDK inconsistency in spawned instances.** Things like `File.read` sometimes work in ephemerals and sometimes don't. The smoking gun: with multiple parallel spawns, often *one* fails while the *other* works. That smells like shared mutable state or race conditions, not a uniform configuration bug. Easier to debug after (1) lands because we can probe live.

3. **Forks need to actually run.** Currently `ForkHandle` carries isolated memory state and resolution operations (merge_back / discard / promote) but no agent loop runs in the fork. We want fork sessions to be peer-like: own mailbox, parent and fork can exchange messages, `merge_back` stops the fork (parent and fork become one again). Most of the substrate exists from ephemeral spawn; this is adapting it for a longer-lived peer-style child.

Tackle (1) → (2) → (3) so each lands on top of the diagnostic affordances of the previous.

---

## Issue 1: TUI attribution + ephemeral mailboxes

### What's broken

**Attribution:** `WireTurnEvent`s flow from the runtime through `pattern_server`'s actor and out to the TUI. There's no field on the wire shape distinguishing 'this came from the main session' vs 'this came from spawn X'. The TUI can't route to a sidebar because it can't tell.

Same problem applies to **history loading**: when reconstructing past turns, ephemeral output gets interleaved with main flow because nothing tagged it at write time.

**Mailboxes:** Siblings register in `AgentRegistry` and get peer-like message routing. Ephemerals do *not* — `run_ephemeral` calls `drive_step` directly with no mailbox, so they're fire-and-forget until termination. This makes them hard to manage:
- Can't ask a stuck ephemeral what it's working on
- Can't redirect or cancel via message
- Can't stream observations back through the message channel

Related: ephemeral progress log blocks (`create_progress_log_block` / `build_progress_log_observer` in `spawn::ephemeral`) currently don't seem to work — writes don't surface, or the block isn't readable from the parent. Needs investigation, but bundling the fix here makes sense since we're touching the ephemeral lifecycle anyway.

### Plan (where clear)

**Attribution:**
- Add a `source: SpawnSource` (or similar) field to `WireTurnEvent` (or wrap it). `SpawnSource = MainSession | Ephemeral(SpawnId) | Sibling(PersonaId) | Fork(SpawnId)`.
- Tag events at the point they're emitted in the child's drive loop. The child `SessionContext` already knows its identity; thread it into the event sink.
- `pattern_server` actor passes the tag through unchanged.
- TUI: route by tag — main flow renders main, ephemerals/forks render to sidebar(s) by SpawnId.
- History persistence: persist the tag with each event so reload reconstructs the same routing.

**Ephemeral mailboxes:**
- Register ephemerals in `AgentRegistry` with a spawn-scoped identity (probably `SpawnId`-keyed alongside the existing `PersonaId` map, or treat the ephemeral as a transient persona).
- `run_ephemeral` becomes mailbox-driven: between `drive_step` calls, drain the mailbox and inject messages as user-turn input.
- Parent can `send recipient body` where recipient is the ephemeral's id.
- On termination, unregister and let the existing `SpawnResult` flow happen.

**Log block fix:** Investigate first — could be drain timing, label collision, or missing read-side wiring. Don't pre-design the fix until we know.

### Open questions

- What's the right identity for an ephemeral in the registry? Reuse `PersonaId` (synthesised) or extend the registry to key on `SpawnId`?
- Should sibling spawns also get attribution tags? (Probably yes — consistency.)
- For history: do we backfill old events with `MainSession` tag at read time, or migrate the persistence schema?

---

## Issue 2: SDK inconsistency in spawned instances

### What's broken

Reports of `File.read` / `File.write` (and possibly other handlers) sometimes failing inside ephemerals. **Inconsistent**: same code in same persona sometimes works. **Parallel-spawn-correlated**: when two ephemerals run concurrently, often one is fine and the other isn't.

### Hypotheses to verify (once we have ephemeral mailboxes for live debugging)

**Top suspect: shared evaluator pool.** Each ephemeral compiles and runs Haskell via tidepool. The eval workers are OS threads from a bounded pool (per AGENTS.md, around the 'eval worker is a plain OS thread spawned via std::thread::spawn' section). If ephemerals share the parent's eval worker pool — and the workers carry any per-eval state that doesn't reset cleanly between evaluations (cached compilation artifacts, interner state, thread-locals, anything not destructed at eval boundaries) — parallel ephemerals queueing onto the same workers would observe each other's residue. The 'one of two parallel spawns fails' pattern matches exactly: outcome depends on which worker each spawn lands on and what that worker carried over from the previous tenant. **First thing to check before chasing other hypotheses.**

Other candidates:

1. **Shared mutable state in the file handler.**
 The mount info / policy gate may use process-global or session-global state that gets clobbered when two children mutate it concurrently. Suspect: anything in `pattern_runtime/src/sdk/handlers/file.rs` or wherever the policy is evaluated.

2. **TempDir collision in `synthesize_program_lib`.** Each ephemeral writes `lib/Pattern/SpawnHelpers.hs` to a `tempfile::TempDir`. If something downstream caches by path, two parallel spawns could race. (Probably fine since `TempDir` randomises, but worth checking the include-path resolution.)

3. **tidepool-runtime concurrency hazard.** `compile_haskell` shells out to the GHC plugin binary; if it shares cache paths, parallel spawns could trample. AGENTS.md already mentions 'concurrent tidepool-extract spawns contending on shared cache paths' as a known concern (around line 732).

4. **Capability/policy state not properly scoped to child.** `compute_child_caps` does intersection via `restrict_to`, but if the file handler reads from a different source than the child's restricted set, a child could see capabilities that get revoked under it.

5. **Memory cache concurrent writes.** Children inherit the parent's memory cache. If two children write the same block in parallel, what happens? Loro should handle CRDT merges, but a path that bypasses the cache to do raw I/O could race.

### Plan

Don't pre-commit to a fix. After (1) ships, write a reproducer that spawns N ephemerals each doing the same `File.read`, observe failure pattern, then narrow with the live-debugging mailbox affordance. Once root cause is identified, write a focused fix + regression test.

---

## Issue 3: Forks need to actually run

### What's broken

`ForkHandle` (in `spawn::fork`) carries:
- Isolated memory state (`ForkIsolationState::Lightweight` via `LoroDoc::fork()`, or `Persistent` via jj workspace + bookmark)
- A `cancel_state` and `cancel_watcher` JoinHandle
- Resolution methods: `merge_back_lightweight`, `merge_back_persistent`, `discard`, `promote`

What it *doesn't* have: an agent loop driving turns. The fork is inert. You can isolate state and merge it back, but the fork can't do work in that state.

### Plan (peer-like fork sessions)

**Decision (confirmed with orual):** Forks get mailboxes and run as peers. Parent and fork can message each other. `merge_back` stops the fork's loop and merges its state back into the parent's cache.

**Build on the ephemeral mailbox infrastructure from issue 1:**
- A fork is essentially 'an ephemeral-style runner with isolated state and persistent lifetime'.
- Build a `fork_for_session` (analogous to `fork_for_ephemeral`) that constructs a child `SessionContext` from the `ForkHandle`'s isolated cache.
- Spin up an agent loop on top of that context (mailbox-driven, no MAX_TURNS cap).
- Register in `AgentRegistry` so `send` / message routing works in both directions.

**Lifecycle:**
- Fork session runs until: parent calls `merge_back` (state merges, loop stops, registry unregisters); parent calls `discard` (cancel + drop); fork session terminates itself; or parent session drops (cascade cancel).
- `merge_back` semantics: import fork's Loro snapshot into parent cache (already implemented), then signal the fork's `cancel_state` to stop the loop, then unregister.
- The 'parent and fork become one' framing means: after merge_back, the fork agent identity dissolves; subsequent messages to the fork's id should error or be redirected.

### Open questions

- Persistent forks (jj workspace) live across daemon restarts. How does the agent loop reattach on restart? Probably needs a session-resume mechanism, similar to how main sessions resume.
- Promote (currently consumes the fork into a new draft persona): should promote also stop the fork's agent loop? Probably yes — the seed cache becomes the new persona's starting state, and the fork itself ceases.
- What capabilities does a fork inherit? Same as parent? Restricted? (Currently no capability machinery for forks.)

---

## Tracking

Update this doc as we learn things. Keep the 'open questions' sections honest. When we close one, move it to a 'decisions' section with the resolution.
