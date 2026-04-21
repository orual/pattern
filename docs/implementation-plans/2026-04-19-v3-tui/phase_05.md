# v3 TUI — Phase 5: Concurrent batches

**Goal:** Users can send new messages while an agent is still responding. Both responses stream simultaneously in their correct positions in the conversation — the "shadow clone jutsu."

**Architecture:** Each `send_message` call returns a `BatchId`. The daemon tags every `TurnEvent` with that `BatchId` via the `TurnSinkBridge`. The TUI routes incoming `TaggedTurnEvent`s to the correct `RenderBatch` by looking up the `batch_id` in `ConversationState.batches`. Multiple batches can be in `streaming = true` state simultaneously. The input area is never blocked — submit always works, creating a new batch immediately.

**Tech Stack:** No new dependencies. Uses existing BatchId (Phase 1), RenderBatch (Phase 2), input handling (Phase 3).

**Scope:** Phase 5 of 6 from the v3-tui design plan.

**Codebase verified:** 2026-04-20

---

## Acceptance criteria coverage

This phase implements and tests:

### v3-tui.AC5: Concurrent batches
- **v3-tui.AC5.1 Success:** User sends message while agent is mid-response; input area accepts the new message immediately
- **v3-tui.AC5.2 Success:** Both batch A (previous) and batch B (new) responses stream simultaneously; A continues above the new user message, B appears below
- **v3-tui.AC5.3 Success:** Scrolling up during concurrent streaming shows batch A still receiving text
- **v3-tui.AC5.4 Success:** `TurnEvent`s route to correct `RenderBatch` by `BatchId` — no cross-contamination
- **v3-tui.AC5.5 Failure:** Cancelling batch A (`cancel_batch`) stops its events; batch B continues unaffected
- **v3-tui.AC5.6 Edge:** Three concurrent batches (user sends three times rapidly) all render in correct positions

---

<!-- START_TASK_1 -->
### Task 1: Batch-aware event routing

**Verifies:** v3-tui.AC5.4

**Files:**
- Modify: `crates/pattern_cli/src/tui/app.rs`

**Implementation:**

Currently, the app's event handler likely pushes all incoming events to the last batch. This needs to change to route by `batch_id`.

Replace the event handling in `App::handle_turn_event`:

```rust
fn handle_turn_event(&mut self, event: TaggedTurnEvent) {
    let batch_id = &event.batch_id;

    // Find the batch this event belongs to.
    let batch = self.conversation.batches.iter_mut()
        .find(|b| b.batch_id == *batch_id);

    match batch {
        Some(batch) => {
            // Route display events to panel/toast (Phase 4 logic).
            match &event.event {
                TurnEvent::Display { kind, text } => {
                    self.route_display_event(*kind, text.clone());
                    return;
                }
                _ => {}
            }
            batch.push_event(&event.event);
        }
        None => {
            // Event for unknown batch — create a new batch.
            // This can happen if subscribe started after send_message.
            let mut new_batch = RenderBatch::new(batch_id.clone());
            match &event.event {
                TurnEvent::Display { kind, text } => {
                    self.route_display_event(*kind, text.clone());
                }
                _ => {
                    new_batch.push_event(&event.event);
                }
            }
            self.conversation.batches.push(new_batch);
        }
    }
}
```

The key invariant: `batch_id` on the `TaggedTurnEvent` determines which `RenderBatch` receives the event. Events never cross batches.

**Testing:**

- `events_route_to_correct_batch` — create two batches with different IDs, send events tagged for each, verify content lands in the right batch
- `unknown_batch_creates_new` — send event for non-existent batch_id, verify a new batch is created
- `no_cross_contamination` — interleave events for two batches, verify each batch has only its own events

**Verification:**

Run: `cargo nextest run -p pattern-cli app::batch_routing`
Expected: all tests pass

**Commit:** `[pattern-cli] route TurnEvents to correct RenderBatch by BatchId`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Concurrent submit — non-blocking input during streaming

**Verifies:** v3-tui.AC5.1, v3-tui.AC5.2

**Files:**
- Modify: `crates/pattern_cli/src/tui/app.rs`

**Implementation:**

The submit flow (from Phase 3) currently creates a batch and sends to the daemon. For concurrent batches, the key change is: **submitting a new message does NOT wait for the previous batch to finish**. The previous batch continues streaming in its position while the new batch starts below the new user message.

Updated submit flow:
```rust
InputAction::Submit(parts) => {
    // Client mints the batch_id (snowflake — safe for distributed minting).
    let batch_id = new_snowflake_id();

    // Create the user message batch immediately — visible right away.
    let mut batch = RenderBatch::new(batch_id.clone());
    batch.user_message = Some(text_from_parts(&parts));
    self.conversation.batches.push(batch);

    // Send to daemon asynchronously. The daemon uses our batch_id to tag
    // all TurnEvents for this exchange, so events route to the correct
    // RenderBatch with no reconciliation needed.
    if let Some(client) = &self.client {
        let client = client.clone();
        let agent_id = self.current_agent.clone();
        tokio::spawn(async move {
            if let Err(e) = client.send_message(batch_id, agent_id, parts).await {
                tracing::error!("send failed: {e}");
            }
        });
    }

    // Auto-scroll to show the new batch.
    self.conversation.auto_scroll = true;
}
```

The client-minted batch_id is sent as part of `AgentMessage`. The daemon uses it directly when constructing the `TurnSinkBridge`, so all events arrive tagged with the same ID the TUI already used for its `RenderBatch`. No reconciliation, no placeholder IDs, no race conditions. Snowflake IDs are designed for exactly this — distributed minting with monotonic ordering and no coordination.

**Testing:**

- `submit_during_streaming_creates_new_batch` — batch A streaming, submit new message, two batches exist
- `input_not_blocked_during_streaming` — verify input handler accepts keystrokes while events are arriving
- `new_batch_appears_after_previous` — batch B position is after batch A + user message B

**Verification:**

Run: `cargo nextest run -p pattern-cli app::concurrent`
Expected: all tests pass

**Commit:** `[pattern-cli] concurrent submit with non-blocking input`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Scroll behaviour during concurrent streaming

**Verifies:** v3-tui.AC5.2, v3-tui.AC5.3

**Files:**
- Modify: `crates/pattern_cli/src/tui/scroll.rs`
- Modify: `crates/pattern_cli/src/tui/conversation.rs`

**Implementation:**

When multiple batches are streaming simultaneously, scroll behaviour needs refinement:

1. **Auto-scroll follows the newest batch.** If `auto_scroll = true`, viewport tracks the bottom of the last batch (the most recent response). The previous batch continues growing above but the viewport doesn't jump to it.

2. **Scrolling up shows previous batch still streaming.** When user scrolls up during concurrent streaming (AC5.3), they can see batch A receiving text. The viewport stays where the user put it — `auto_scroll = false`.

3. **Re-engaging auto-scroll.** Scrolling to the bottom (End key or scroll to end) re-engages `auto_scroll`, jumping to the newest content.

4. **Height invalidation during concurrent streaming.** Multiple batches may have their heights changing simultaneously. The virtual scrolling in `ConversationView::render` already handles this because it recomputes visible batches each frame. Batches with `streaming = true` always have their height cache invalidated before rendering.

Changes to `ConversationView::render`:
```rust
// Before computing visible range, invalidate heights on streaming batches.
for batch in state.batches.iter_mut() {
    if batch.streaming {
        batch.cached_total_height = None;
        // Also invalidate the last section's height (it's still growing).
        if let Some(section) = batch.sections.last_mut() {
            section.cached_height = None;
        }
    }
}
```

**Testing:**

- `auto_scroll_follows_newest_batch` — two streaming batches, viewport at bottom shows batch B content
- `scroll_up_shows_batch_a_streaming` — scroll up, batch A text visible and growing
- `scroll_to_bottom_reengages` — scroll up then End → viewport jumps to bottom, auto_scroll true
- `streaming_batches_invalidate_height` — streaming batch height changes between frames

**Verification:**

Run: `cargo nextest run -p pattern-cli scroll`
Expected: all tests pass

**Commit:** `[pattern-cli] scroll behaviour for concurrent streaming batches`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Batch cancellation

**Verifies:** v3-tui.AC5.5

**Files:**
- Modify: `crates/pattern_cli/src/tui/app.rs`
- Modify: `crates/pattern_cli/src/tui/commands.rs`

**Implementation:**

Add a `/cancel` slash command that cancels the most recent streaming batch (or a specific one by ID).

```rust
// In command registry:
CommandDef {
    name: "cancel",
    description: "Cancel the current response",
    target: CommandTarget::Runtime,
    arg_hint: ArgHint::None,
}
```

Dispatch:
```rust
"cancel" => {
    // Find the most recent streaming batch.
    if let Some(batch) = self.conversation.batches.iter().rev()
        .find(|b| b.streaming)
    {
        let batch_id = batch.batch_id.clone();
        if let Some(client) = &self.client {
            let client = client.clone();
            tokio::spawn(async move {
                let _ = client.cancel_batch(batch_id).await;
            });
        }
    }
}
```

On the daemon side (already stubbed in Phase 1), `cancel_batch` signals the session's `CancelState`. Events stop arriving for that batch. The TUI marks the batch as `streaming = false` when a `Stop` event with an appropriate reason arrives, or after a timeout.

When batch A is cancelled, batch B (if any) continues unaffected — they're independent subscriptions tagged with different batch IDs.

**Testing:**

- `cancel_stops_batch_a` — mock: cancel batch A, verify it stops receiving events
- `cancel_doesnt_affect_batch_b` — cancel A, B still receives events
- `cancel_with_no_streaming_batch` — `/cancel` with nothing streaming → graceful no-op

**Verification:**

Run: `cargo nextest run -p pattern-cli app::cancel`
Expected: all tests pass

**Commit:** `[pattern-cli] batch cancellation via /cancel command`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Integration tests for concurrent batches

**Verifies:** v3-tui.AC5.2, v3-tui.AC5.4, v3-tui.AC5.6

**Files:**
- Create: `crates/pattern_cli/tests/concurrent_batches.rs`

**Implementation:**

End-to-end tests using the daemon's local mode (in-process channels) and the app's event handling.

Test scenarios:

1. **Two concurrent batches** (AC5.2):
   - Send message A, subscribe
   - Send message B before A completes
   - Verify both batches receive events
   - Verify batch order: A's user message, A's response, B's user message, B's response
   - Verify no cross-contamination (AC5.4)

2. **Three concurrent batches** (AC5.6):
   - Send A, B, C rapidly
   - Verify all three batches render in correct positions
   - Verify all three receive their tagged events

3. **Concurrent with scrolling** (AC5.3):
   - Send A, start receiving events
   - Send B
   - Simulate scroll-up
   - Verify A's content still growing in the scrolled viewport

These tests use the daemon's echo mode (Phase 1) — each send_message produces synthetic Text + Stop events tagged with the correct batch_id. For testing concurrency, the echo handler can be made to delay responses so both batches overlap.

**Verification:**

Run: `cargo nextest run -p pattern-cli --test concurrent_batches`
Expected: all tests pass

**Commit:** `[pattern-cli] concurrent batch integration tests`
<!-- END_TASK_5 -->
