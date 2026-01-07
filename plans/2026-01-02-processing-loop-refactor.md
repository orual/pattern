# Processing loop refactor

## Summary

Refactor the `DatabaseAgent::process()` loop to:
1. Execute tools inline as content blocks are processed (not batched at end)
2. Eliminate duplicated code via extracted helpers
3. Port robust retry logic from old main branch
4. Enable proper event ordering and early exit

## Problem

The current loop in `db_agent.rs` (~800 lines) has several issues:

1. **Tool extraction duplicated** - Same logic appears twice: once for streaming events, again for execution
2. **Error handling repeated** - The `send_event(Error) → set_state(Error) → recovery → set_state(Ready)` pattern appears 5+ times
3. **Force-execution duplicated** - Start constraints and exit requirements have nearly identical ~50-line blocks
4. **Tools execute after full response** - Collects all tool calls, then executes. Prevents early exit and proper ordering
5. **Missing retry robustness** - Old version had Gemini quirk handling, rate limit parsing, exponential backoff

## Solution

### New module structure

```
crates/pattern_core/src/agent/
├── mod.rs
├── db_agent.rs              # Slimmed: struct, builder, Agent trait
├── processing/
│   ├── mod.rs               # Re-exports
│   ├── loop_impl.rs         # run_processing_loop()
│   ├── content.rs           # Content iteration and block processing
│   ├── retry.rs             # complete_with_retry()
│   └── errors.rs            # ProcessingError, handle_processing_error()
```

### Core types

#### Parameter structs (not objects with methods)

```rust
/// Immutable context for the processing loop
pub struct ProcessingContext<'a> {
    pub agent_id: &'a AgentId,
    pub runtime: &'a AgentRuntime,
    pub model: &'a dyn ModelProvider,
    pub response_options: &'a ResponseOptions,
    pub base_instructions: Option<&'a str>,
    pub batch_id: SnowflakePosition,
    pub batch_type: BatchType,
}

/// Mutable state that changes during processing
pub struct ProcessingState {
    pub process_state: ProcessState,
    pub sequence_num: u32,
    pub start_constraint_attempts: u8,
    pub exit_requirement_attempts: u8,
}
```

#### Content iteration (unify without normalizing)

```rust
/// Unified view for iteration - doesn't transform underlying data
pub enum ContentItem<'a> {
    Text(&'a str),
    Thinking(&'a str),
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: &'a serde_json::Value,
    },
    Other,
}

/// Iterator adapter over MessageContent variants
pub fn iter_content_items(content: &[MessageContent]) -> impl Iterator<Item = ContentItem<'_>>
```

Handles both `MessageContent::ToolCalls(vec)` and `MessageContent::Blocks` with `ContentBlock::ToolUse` without transforming storage format.

#### Loop outcome

`ToolAction` (defined in runtime) is used directly in the processing loop - no separate `BlockAction` needed.

```rust
pub enum LoopOutcome {
    Completed { metadata: ResponseMetadata },
    HeartbeatRequested {
        tool_name: String,
        call_id: String,
        next_sequence_num: u32,
    },
    ErrorRecovered,
}
```

### Retry logic (ported from old version)

```rust
pub struct RetryConfig {
    pub max_attempts: u8,        // Default: 10
    pub base_backoff_ms: u64,    // Default: 1000
    pub max_backoff_ms: u64,     // Default: 60_000
    pub jitter_ms: u64,          // Default: 2000
}

pub enum RetryDecision {
    Retry { wait_ms: u64, modify_prompt: Option<PromptModification> },
    Fatal(ProcessingError),
}

pub enum PromptModification {
    /// Gemini empty candidates quirk - append punctuation
    AppendToLastMessage(String),
}

pub async fn complete_with_retry(
    ctx: &ProcessingContext<'_>,
    request: &mut Request,
    config: &RetryConfig,
) -> Result<Response, ProcessingError>
```

Error classification handles:
- **Rate limits (429/529)** - Parse `Retry-After`, `x-ratelimit-reset-*`, Anthropic epoch headers
- **Gemini empty candidates** - Retry with prompt modification (append `.`, `?`, `!`)
- **Server errors (5xx)** - Exponential backoff
- **Context too long** - Could trigger compression, or fatal
- **Auth errors (401/403)** - Fatal, no retry

### Centralized error handling

```rust
pub struct ErrorContext<'a> {
    pub event_tx: &'a mpsc::Sender<ResponseEvent>,
    pub runtime: &'a AgentRuntime,
    pub batch_id: Option<SnowflakePosition>,
}

pub async fn handle_processing_error(
    ctx: &ErrorContext<'_>,
    agent: &dyn Agent,
    error: &ProcessingError,
) -> LoopOutcome
```

`run_error_recovery` moves from `DatabaseAgent` impl to free function.

### Force-execution helper

```rust
pub struct ConstraintViolation {
    pub required_tools: Vec<String>,
    pub violation_type: ConstraintType,
}

pub enum ConstraintType {
    StartConstraint,
    ExitRequirement,
}

pub async fn force_execute_required_tools(
    ctx: &ProcessingContext<'_>,
    state: &mut ProcessingState,
    event_tx: &mpsc::Sender<ResponseEvent>,
    violation: &ConstraintViolation,
) -> Vec<ToolResponse>
```

### Main loop flow

```rust
pub async fn run_processing_loop(
    ctx: ProcessingContext<'_>,
    state: &mut ProcessingState,
    event_tx: &mpsc::Sender<ResponseEvent>,
    initial_message: Message,
    heartbeat_sender: &HeartbeatSender,
) -> Result<LoopOutcome, ProcessingError> {
    // 1. Build initial request
    let mut request = ctx.runtime.prepare_request(...).await?;

    loop {
        // 2. Call model with retry
        let response = complete_with_retry(&ctx, &mut request, &config).await?;

        // 3. Store response messages
        store_response_messages(&ctx, state, &response).await?;

        // 4. Process content blocks IN ORDER (inline tool execution)
        let mut tool_responses = Vec::new();
        let mut pending_action = ToolAction::Continue;

        for item in iter_content_items(&response.content) {
            match item {
                ContentItem::Text(text) => emit_text_event(...),
                ContentItem::Thinking(text) => emit_reasoning_event(...),
                ContentItem::ToolUse { id, name, input } => {
                    let outcome = execute_and_emit_tool(...).await?;
                    tool_responses.push(outcome.response);

                    if !matches!(outcome.action, ToolAction::Continue) {
                        pending_action = outcome.action;
                        break;  // Early exit
                    }
                }
                ContentItem::Other => {}
            }
        }

        // 5. Store tool responses
        emit_and_store_tool_responses(...).await?;

        // 6. Handle heartbeat exit
        if let ToolAction::RequestHeartbeat { .. } = pending_action {
            send_heartbeat(...)?;
            return Ok(LoopOutcome::HeartbeatRequested { ... });
        }

        // 7. Check exit conditions and requirements
        let should_exit = matches!(pending_action, ToolAction::ExitLoop)
            || ctx.runtime.should_exit_loop(&state.process_state);
        let needs_continuation = !tool_responses.is_empty();

        if should_exit || !needs_continuation {
            let pending = ctx.runtime.get_pending_exit_requirements(...);
            if !pending.is_empty() {
                // Retry logic for exit requirements
            } else {
                break;  // Clean exit
            }
        }

        // 8. Prepare continuation
        request = ctx.runtime.prepare_request(...).await?;
    }

    ctx.runtime.complete_batch(ctx.batch_id);
    Ok(LoopOutcome::Completed { metadata })
}
```

### Tool execution on runtime

Move action determination into the runtime. The runtime understands tool rules and process state - it should determine what action to take.

```rust
// In pattern_core/src/runtime/mod.rs (or tool execution module)

pub enum ToolAction {
    Continue,
    ExitLoop,
    RequestHeartbeat { tool_name: String, call_id: String },
}

pub struct ToolExecutionOutcome {
    pub response: ToolResponse,
    pub action: ToolAction,
}

impl AgentRuntime {
    /// Execute tool and determine resulting action based on rules and process state
    pub async fn execute_tool_checked(
        &self,
        call: &ToolCall,
        batch_id: SnowflakePosition,
        process_state: &mut ProcessState,
        meta: &ExecutionMeta,
    ) -> Result<ToolExecutionOutcome, ToolExecutionError> {
        let result = self.execute_tool(call, batch_id, process_state, meta).await?;

        let action = if meta.request_heartbeat && !result.has_continue_rule {
            ToolAction::RequestHeartbeat {
                tool_name: call.fn_name.clone(),
                call_id: call.call_id.clone(),
            }
        } else if self.should_exit_loop(process_state) {
            ToolAction::ExitLoop
        } else {
            ToolAction::Continue
        };

        Ok(ToolExecutionOutcome {
            response: result.response,
            action,
        })
    }
}
```

Processing loop just emits events and uses the action:

```rust
ContentItem::ToolUse { id, name, input } => {
    emit_event(event_tx, ResponseEvent::ToolCallStarted { ... }).await;

    let meta = build_execution_meta(id, input);
    let call = build_tool_call(id, name, input);

    match ctx.runtime.execute_tool_checked(&call, ctx.batch_id, &mut state.process_state, &meta).await {
        Ok(outcome) => {
            emit_event(event_tx, ResponseEvent::ToolCallCompleted {
                call_id: id.to_string(),
                result: Ok(outcome.response.content.clone()),
            }).await;

            tool_responses.push(outcome.response);

            if !matches!(outcome.action, ToolAction::Continue) {
                pending_action = outcome.action;
                break;
            }
        }
        Err(e) => { /* constraint violation handling */ }
    }
}
```

### Slimmed db_agent.rs

```rust
async fn process(
    self: Arc<Self>,
    message: Message,
) -> Result<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>, CoreError> {
    // Setup batch, state
    self.set_state(AgentState::Processing { ... }).await?;

    let (tx, rx) = mpsc::channel(100);

    tokio::spawn(async move {
        let ctx = ProcessingContext { ... };
        let mut state = ProcessingState { ... };

        let outcome = run_processing_loop(ctx, &mut state, &tx, message, &heartbeat).await;

        // Emit Complete event
        // Reset to Ready state
    });

    Ok(Box::new(ReceiverStream::new(rx)))
}
```

~50 lines instead of ~800.

## Implementation steps

1. **Runtime changes** - Add `ToolAction`, `ToolExecutionOutcome`, `execute_tool_checked()` to `AgentRuntime`
2. **Create module structure** - `agent/processing/` with `mod.rs`, `content.rs`, `errors.rs`, `retry.rs`, `loop_impl.rs`
3. **Implement `content.rs`** - `ContentItem` enum and `iter_content_items()`
4. **Implement `errors.rs`** - `ProcessingError`, `handle_processing_error()`, move `run_error_recovery()` from db_agent
5. **Implement `retry.rs`** - `RetryConfig`, `complete_with_retry()`, error classification with rate limit parsing
6. **Implement `loop_impl.rs`** - helper functions:
   - `emit_event()`
   - `store_response_messages()`
   - `emit_and_store_tool_responses()`
   - `handle_start_constraint_violation()`
   - `force_execute_required_tools()`
   - `add_exit_reminder()` / `add_start_reminder()`
   - `send_heartbeat()`
7. **Implement `run_processing_loop()`** - main orchestration function
8. **Refactor `db_agent.rs`** - slim down to struct, builder, `Agent` trait impl calling `run_processing_loop()`
9. **Remove dead code** - old loop logic, moved functions
10. **Update tests** - ensure existing tests pass, add new unit tests

## Testing

- Existing tests should continue to pass (behaviour preserved)
- Add unit tests for:
  - `iter_content_items()` with both content formats
  - `complete_with_retry()` with various error scenarios
  - `force_execute_required_tools()`
- Integration tests for inline execution ordering

## Migration notes

- No API changes to `Agent` trait
- No changes to `ResponseEvent` variants
- Internal refactor only - callers unaffected
