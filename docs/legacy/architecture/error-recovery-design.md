# Error Recovery & Batch Management Design

## Problem Statement

Currently when `process_message_stream()` encounters an error, the agent remains in `Processing` state until timeout. This can cause:
- Agents stuck in unusable states
- Duplicate message sends if naively restarted
- Lost progress on partially completed batches
- No visibility into what succeeded vs failed

## Design Goals

1. **Never duplicate external side effects** (messages, posts, API calls)
2. **Preserve completed work** within a batch
3. **Enable safe recovery** from transient failures
4. **Provide visibility** into error states and recovery options
5. **Support both automatic and manual recovery** paths

## Proposed Solution

### 1. Enhanced Agent State

Add an Error state variant that captures batch context:

```rust
pub enum AgentState {
    Ready,
    Processing,
    Error { 
        reason: String,
        batch_id: Option<SnowflakePosition>,
        occurred_at: DateTime<Utc>,
        recovery_hint: RecoveryStrategy,
    },
    // ... other states
}
```

### 2. Action Tracking System

Track completed actions at the batch level to prevent duplicates:

```rust
pub enum ActionStatus {
    Completed {
        action: String,  // e.g., "discord_reply", "bluesky_post"
        tool_call_id: String,
        result: serde_json::Value,
        timestamp: DateTime<Utc>,
    },
    Failed {
        action: String,
        tool_call_id: String,
        error: String,
        retryable: bool,
    },
}

pub struct BatchProgress {
    pub batch_id: SnowflakePosition,
    pub completed_actions: Vec<ActionStatus>,
    pub pending_tool_calls: Vec<String>,
    pub external_effects: Vec<String>, // IDs of sent messages, posts, etc
}
```

### 3. Side Effect Classification

Tools will be classified by their side effects:

**Safe to Retry (Idempotent):**
- Memory operations (search, recall, context) *
- Read-only operations
- Internal state queries

*Note: `append` operations need duplicate detection (see section 5.4)

**Never Auto-Retry (External Effects):**
- `send_message` (any endpoint)
- Social media posts
- External API writes
- Payment/transaction operations

**Conditionally Retryable:**
- LLM generation (check for partial responses)
- Computations with deterministic outputs
- Cache-backed operations

### 4. Recovery Strategies

```rust
pub enum RecoveryStrategy {
    /// Safe to restart entire batch (no external effects completed)
    RestartBatch,
    
    /// Restart from last external effect
    RestartFromLastExternal { 
        skip_until: SnowflakePosition 
    },
    
    /// Selective tool execution (skip completed external effects)
    Selective {
        completed_tools: Vec<String>,
        skip_tools: Vec<String>,
        resume_from_tool: Option<String>,
    },
    
    /// Manual intervention required
    ManualOnly { 
        reason: String 
    },
    
    /// Clear batch and reset to ready
    Abandon,
}
```

### 5. Implementation Points

#### 5.1 Error Detection

In `process_message_stream()`:

```rust
let result = async {
    // existing processing logic
}.await;

if let Err(e) = &result {
    let batch_progress = self.get_current_batch_progress().await?;
    let recovery_strategy = self.determine_recovery_strategy(&e, &batch_progress).await?;
    
    self.set_state(AgentState::Error {
        reason: format!("{:?}", e),
        batch_id: current_batch_id,
        occurred_at: Utc::now(),
        recovery_hint: recovery_strategy,
    }).await?;
    
    // Store batch progress for recovery
    self.persist_batch_progress(batch_progress).await?;
}
```

#### 5.2 Tool Execution Tracking

Mark completion BEFORE returning results:

```rust
// In tool execution wrapper
let result = tool.execute(params).await?;

// Mark as completed immediately (before response processing that might fail)
self.mark_action_completed(ActionStatus::Completed {
    action: tool.name().to_string(),
    tool_call_id: call_id,
    result: serde_json::to_value(&result)?,
    timestamp: Utc::now(),
}).await?;

// Store external effect IDs if applicable
if let Some(external_id) = extract_external_id(&result) {
    self.record_external_effect(batch_id, external_id).await?;
}

result
```

#### 5.3 Duplicate Detection for Memory Operations

For memory append operations specifically:

```rust
impl MemoryBlock {
    /// Append with duplicate detection
    pub async fn append_safe(&mut self, content: &str) -> Result<AppendResult> {
        // Simple tail check - does the block already end with this content?
        let current_value = self.value.trim_end();
        let content_trimmed = content.trim();
        
        if current_value.ends_with(content_trimmed) {
            // Already appended, skip duplicate
            return Ok(AppendResult::AlreadyPresent {
                message: "Content already exists at end of block".into()
            });
        }
        
        // For partial overlap detection (optional)
        // Check if last N chars match start of new content (interrupted append)
        if content_trimmed.len() > 20 {
            let check_len = content_trimmed.len().min(100);
            let tail = current_value.chars().rev().take(check_len).collect::<String>();
            let tail_rev = tail.chars().rev().collect::<String>();
            
            if content_trimmed.starts_with(&tail_rev) {
                // Partial append detected, continue from where it left off
                let remaining = &content_trimmed[tail_rev.len()..];
                self.append_internal(remaining).await?;
                return Ok(AppendResult::ResumedPartial {
                    skipped_bytes: tail_rev.len()
                });
            }
        }
        
        // Not a duplicate, proceed with normal append
        self.append_internal(content).await?;
        Ok(AppendResult::Success)
    }
}

pub enum AppendResult {
    Success,
    AlreadyPresent { message: String },
    ResumedPartial { skipped_bytes: usize },
}
```

This approach is much simpler than hashing:
- No need to track append history
- Handles partial appends from interrupted operations
- Zero additional storage overhead
- Natural idempotency for recovery scenarios

#### 5.4 Recovery Functions

```rust
impl DatabaseAgent {
    /// Attempt automatic recovery based on error state
    pub async fn auto_recover(&self) -> Result<()> {
        match self.state {
            AgentState::Error { batch_id, recovery_hint, .. } => {
                match recovery_hint {
                    RecoveryStrategy::RestartBatch => {
                        self.restart_batch(batch_id).await
                    },
                    RecoveryStrategy::Selective { .. } => {
                        self.selective_recovery(batch_id).await
                    },
                    RecoveryStrategy::Abandon => {
                        self.clear_batch_and_reset(batch_id).await
                    },
                    _ => Err("Manual recovery required".into())
                }
            },
            _ => Ok(())
        }
    }
    
    /// Safe batch restart with completed action awareness
    async fn restart_batch(&self, batch_id: SnowflakePosition) -> Result<()> {
        let progress = self.get_batch_progress(batch_id).await?;
        
        // Inject synthetic context about completed actions
        if !progress.completed_actions.is_empty() {
            let status_msg = self.build_recovery_context(&progress);
            self.inject_system_message(status_msg).await?;
        }
        
        // Resume processing
        self.set_state(AgentState::Processing).await?;
        self.continue_batch_processing(batch_id, progress).await
    }
    
    /// Clear incomplete batch and reset
    async fn clear_batch_and_reset(&self, batch_id: Option<SnowflakePosition>) -> Result<()> {
        if let Some(batch) = batch_id {
            // Archive failed batch for debugging
            self.archive_failed_batch(batch).await?;
            
            // Clear incomplete messages (keeping user message)
            self.clear_incomplete_messages(batch).await?;
        }
        
        self.set_state(AgentState::Ready).await
    }
}
```

### 6. Safety Mechanisms

1. **Never Delete User Messages** - Only clear agent responses and tool calls
2. **Archive Before Deletion** - Move failed batches to archive table
3. **Audit Log** - Record all recovery attempts and outcomes
4. **Rate Limiting** - Prevent rapid retry loops
5. **Manual Override** - CLI commands for forced recovery

### 7. CLI Integration

```bash
# View error state and recovery options
pattern agent status <agent_id>

# Attempt automatic recovery
pattern agent recover <agent_id>

# Manual recovery options
pattern agent recover <agent_id> --strategy abandon
pattern agent recover <agent_id> --strategy restart
pattern agent recover <agent_id> --rollback-to <batch_id>

# View batch progress
pattern agent batch-status <agent_id> <batch_id>
```

### 8. Monitoring & Alerting

- Track error rates per agent
- Alert on repeated failures
- Log recovery attempts and outcomes
- Measure time in error state

## Implementation Phases

### Phase 1: Basic Error State
- Add Error variant to AgentState
- Implement error detection in process_message_stream
- Add basic recovery (clear and reset)

### Phase 2: Action Tracking
- Implement BatchProgress tracking
- Add ActionStatus recording
- Mark tool completions immediately

### Phase 3: Smart Recovery
- Implement recovery strategies
- Add selective recovery logic
- Prevent duplicate external effects

### Phase 4: CLI & Monitoring
- Add CLI commands
- Implement monitoring
- Add alerting for stuck agents

## Testing Strategy

1. **Unit Tests**: Recovery strategy determination
2. **Integration Tests**: Full recovery flows with mock tools
3. **Failure Injection**: Test various error scenarios
4. **Idempotency Tests**: Verify no duplicate side effects

## Open Questions

1. How long to retain failed batch archives?
2. Should we implement exponential backoff for auto-recovery?
3. How to handle recovery of distributed transactions (multi-agent operations)?
4. Should certain errors trigger immediate alerts vs waiting for timeout?

## Related Work

- Current PR: Message batching implementation (prerequisite)
- Future: Transaction-like semantics for multi-agent operations
- Future: Checkpoint/restore for long-running operations