# Pattern Memory Architecture & Multi-Agent Groups

This document describes Pattern's memory hierarchy, native agent coordination via groups, and background processing strategies.

## Memory Hierarchy

Pattern uses a tiered memory system optimized for ADHD cognitive support:

### 1. Core Memory Blocks (Always in Context)
Small blocks (~2000 chars each) always present in the agent's context window:
- **persona**: Agent identity and capabilities
- **current_state**: Real-time status (energy, attention, mood)
- **active_context**: Current task and recent important events
- **bond_evolution**: Relationship dynamics and trust building

### 2. Working Memory Blocks (Swappable)
Active context that can be loaded or archived:
- **Pinned blocks**: Always loaded when agent processes messages
- **Ephemeral blocks**: Loaded only when referenced by notifications

DataStream sources create Working blocks for their content:
```rust
// Source creates block for new content
memory.create_block(
    agent_id,
    "discord_thread_12345",
    "Discord thread context",
    BlockType::Working,
    BlockSchema::text(),
    2000,
).await?;

// Block is included via batch_block_ids when processing notification
let request = ContextBuilder::new(&memory, &config)
    .for_agent(agent_id)
    .with_batch_blocks(vec!["discord_thread_12345".to_string()])
    .build()
    .await?;
```

### 3. Archival Memory (Searchable Long-term)
Separate from blocks, searchable via FTS5 + sqlite-vec:
- Agent observations and insights
- Partner behaviour patterns over time
- Accumulated wisdom

```rust
// Store insight
memory.insert_archival(
    agent_id,
    "User works best with time estimates multiplied by 1.5x",
    Some(json!({"category": "time_patterns"})),
).await?;

// Search later
let results = memory.search_archival(agent_id, "time estimates", 10).await?;
```

### 4. Message History
Conversation history with Snowflake ordering:
- Complete message history with batching
- Recursive summarization for context window management
- FTS5 search across all messages

## Memory Permissions

Permissions control who can modify memory content:

| Permission | Read | Append | Overwrite | Delete |
|------------|------|--------|-----------|--------|
| ReadOnly   | Yes  | No     | No        | No     |
| Partner    | Yes  | Request| Request   | No     |
| Human      | Yes  | Request| Request   | No     |
| Append     | Yes  | Yes    | No        | No     |
| ReadWrite  | Yes  | Yes    | Yes       | No     |
| Admin      | Yes  | Yes    | Yes       | Yes    |

- `Partner` and `Human` permissions require approval via `PermissionBroker`
- Requests appear as Discord/CLI notifications for humans to approve

## Block Schemas

Loro CRDT documents support typed schemas:

**Text**: Simple text with optional viewport
```rust
BlockSchema::Text { viewport: None }
```

**Map**: Structured key-value fields
```rust
BlockSchema::Map {
    fields: vec![
        FieldDef { name: "energy".into(), field_type: FieldType::Number, ..Default::default() },
        FieldDef { name: "mood".into(), field_type: FieldType::Text, ..Default::default() },
    ],
}
```

**List**: Ordered collections (tasks, items)
```rust
BlockSchema::List { item_schema: None, max_items: Some(20) }
```

**Log**: Append-only with display limit
```rust
BlockSchema::Log {
    display_limit: 10,
    entry_schema: LogEntrySchema { timestamp: true, agent_id: true, fields: vec![] },
}
```

**Composite**: Multiple sections
```rust
BlockSchema::Composite {
    sections: vec![
        CompositeSection { name: "notes".into(), schema: Box::new(BlockSchema::text()), read_only: false },
        CompositeSection { name: "diagnostics".into(), schema: Box::new(BlockSchema::text()), read_only: true },
    ],
}
```

## Block Sharing

Agents can share blocks with each other:
```rust
// Share a block with another agent (ReadOnly permission)
db.share_block(owner_id, block_id, recipient_id, MemoryPermission::ReadOnly).await?;

// Recipient sees it in their shared blocks
let shared = memory.list_shared_blocks(recipient_id).await?;

// Access shared content
let doc = memory.get_shared_block(recipient_id, owner_id, "shared_notes").await?;
```

Shared blocks appear in context with attribution:
```xml
<block:shared_notes permission="ReadOnly" shared_from="Archive">
Cross-agent insights about user patterns
</block:shared_notes>
```

## Native Agent Groups

Pattern's native coordination replaces external dependencies like Letta.

### Constellations

A constellation is a collection of agents for a specific user:
```rust
pub struct Constellation {
    pub id: ConstellationId,
    pub owner_id: UserId,
    pub name: String,
    pub agents: Vec<(AgentModel, ConstellationMembership)>,
    pub groups: Vec<GroupId>,
}
```

### Agent Groups

Groups define how agents coordinate:
```rust
pub struct AgentGroup {
    pub id: GroupId,
    pub name: String,
    pub description: String,
    pub coordination_pattern: CoordinationPattern,
    pub state: GroupState,
    pub members: Vec<(AgentModel, GroupMembership)>,
}
```

### Coordination Patterns

Six native coordination patterns:

#### 1. Supervisor
One agent leads, delegates to others:
```rust
CoordinationPattern::Supervisor {
    leader_id: pattern_id,
    delegation_rules: DelegationRules {
        max_delegations_per_agent: Some(3),
        delegation_strategy: DelegationStrategy::Capability,
        fallback_behavior: FallbackBehavior::HandleSelf,
    },
}
```

#### 2. Round Robin
Agents take turns in order:
```rust
CoordinationPattern::RoundRobin {
    current_index: 0,
    skip_unavailable: true,
}
```

#### 3. Voting
Agents vote on decisions:
```rust
CoordinationPattern::Voting {
    quorum: 3,
    voting_rules: VotingRules {
        voting_timeout: Duration::from_secs(30),
        tie_breaker: TieBreaker::Random,
        weight_by_expertise: true,
    },
}
```

#### 4. Pipeline
Sequential processing through stages:
```rust
CoordinationPattern::Pipeline {
    stages: vec![
        PipelineStage {
            name: "analysis".into(),
            agent_ids: vec![entropy_id],
            timeout: Duration::from_secs(60),
            on_failure: StageFailureAction::Skip,
        },
        PipelineStage {
            name: "scheduling".into(),
            agent_ids: vec![flux_id],
            timeout: Duration::from_secs(60),
            on_failure: StageFailureAction::Retry { max_attempts: 2 },
        },
    ],
    parallel_stages: false,
}
```

#### 5. Dynamic
Context-based agent selection:
```rust
CoordinationPattern::Dynamic {
    selector_name: "capability".into(),
    selector_config: HashMap::from([
        ("preferred_domain".into(), "task_management".into()),
    ]),
}
```

Available selectors:
- `random`: Random selection
- `capability`: Match message content to agent capabilities
- `load_balancing`: Select least recently used agent
- `supervisor`: LLM-based selection by supervisor agent

#### 6. Sleeptime
Background monitoring with intervention triggers:
```rust
CoordinationPattern::Sleeptime {
    check_interval: Duration::from_secs(20 * 60), // 20 minutes
    triggers: vec![
        SleeptimeTrigger {
            name: "hyperfocus_check".into(),
            condition: TriggerCondition::TimeElapsed {
                duration: Duration::from_secs(90 * 60),
            },
            priority: TriggerPriority::High,
        },
        SleeptimeTrigger {
            name: "constellation_sync".into(),
            condition: TriggerCondition::ConstellationActivity {
                message_threshold: 20,
                time_threshold: Duration::from_secs(60 * 60),
            },
            priority: TriggerPriority::Medium,
        },
    ],
    intervention_agent_id: Some(pattern_id),
}
```

### Group Member Roles

```rust
pub enum GroupMemberRole {
    Regular,
    Supervisor,
    Observer,  // Receives messages but doesn't respond
    Specialist { domain: String },
}
```

### Group Response Streaming

Groups emit events during processing:
```rust
pub enum GroupResponseEvent {
    Started { group_id, pattern, agent_count },
    AgentStarted { agent_id, agent_name, role },
    TextChunk { agent_id, text, is_final },
    ToolCallStarted { agent_id, call_id, fn_name, args },
    ToolCallCompleted { agent_id, call_id, result },
    AgentCompleted { agent_id, agent_name, message_id },
    Complete { group_id, pattern, execution_time, agent_responses, state_changes },
    Error { agent_id, message, recoverable },
}
```

## Example Group Configurations

### Main Conversational Group
```rust
AgentGroup {
    name: "Main Support".into(),
    coordination_pattern: CoordinationPattern::Dynamic {
        selector_name: "capability".into(),
        selector_config: HashMap::new(),
    },
    members: vec![pattern, entropy, flux, momentum, anchor, archive],
}
```

### Crisis Response Group
```rust
AgentGroup {
    name: "Crisis Response".into(),
    coordination_pattern: CoordinationPattern::RoundRobin {
        current_index: 0,
        skip_unavailable: true,
    },
    members: vec![pattern, momentum, anchor],
}
```

### Planning Session Group
```rust
AgentGroup {
    name: "Planning".into(),
    coordination_pattern: CoordinationPattern::Supervisor {
        leader_id: entropy_id,
        delegation_rules: DelegationRules::default(),
    },
    members: vec![entropy, flux, pattern],
}
```

### Sleeptime Processing Group
```rust
AgentGroup {
    name: "Memory Consolidation".into(),
    coordination_pattern: CoordinationPattern::Sleeptime {
        check_interval: Duration::from_secs(20 * 60),
        triggers: vec![/* ... */],
        intervention_agent_id: Some(archive_id),
    },
    members: vec![pattern, archive],
}
```

## Overlapping Groups

The same agent can belong to multiple groups with different coordination styles:

```
Pattern Agent
├── Main Support (Dynamic)
├── Crisis Response (RoundRobin)
├── Planning (Supervisor - as member)
└── Sleeptime (Sleeptime - as intervener)

Entropy Agent
├── Main Support (Dynamic)
└── Planning (Supervisor - as leader)

Archive Agent
├── Main Support (Dynamic)
└── Sleeptime (Sleeptime - as leader)
```

Benefits:
- Context-specific coordination styles
- Cost optimization (simpler models for sleeptime)
- Focused agent participation per context
- Easy to experiment with different configurations

## Memory Processing Strategy

### Agent-Level Insights
Each agent maintains domain-specific observations:
```rust
// Entropy observes task patterns
memory.insert_archival(
    entropy_id,
    "Tasks labeled 'quick fix' average 3.2x estimated time",
    Some(json!({"domain": "task_patterns"})),
).await?;
```

### Sleeptime Consolidation
Archive agent processes accumulated insights during sleeptime:
```rust
// Sleeptime trigger fires
// Archive reads recent observations from all agents
let all_observations = memory.search_all("observations last 24h", options).await?;

// Consolidates into meta-patterns
memory.insert_archival(
    archive_id,
    "Cross-agent pattern: User productivity drops 40% after 2pm meetings",
    Some(json!({"domain": "meta_patterns", "confidence": 0.85})),
).await?;

// Updates shared blocks with key insights
memory.update_block_text(
    pattern_id,
    "active_context",
    "Note: Schedule creative work before meetings today",
).await?;
```

### Memory Flow

```
Partner interaction
    ↓
Core memory updated (immediate context)
    ↓
Agent observes and processes
    ↓
Insights stored to archival memory
    ↓
Sleeptime trigger fires
    ↓
Archive consolidates patterns
    ↓
Key insights → shared blocks
    ↓
All agents see updated context
```

## Cost Optimization

### Two-Tier Model Strategy
- **Routine monitoring**: Lightweight checks (rules-based or small model)
- **Intervention**: Full model for complex interactions

### Sleeptime Triggers
Sleeptime patterns only invoke expensive models when triggers fire:
```rust
TriggerCondition::ThresholdExceeded {
    metric: "stress_level".into(),
    threshold: 8.0,
}
```

### Batch Processing
Queue processing accumulates messages between expensive model calls:
```rust
let processor = QueueProcessor::new(config, runtime);
processor.start(); // Polls queue, batches notifications
```

## CLI Commands

```bash
# List groups
pattern group list

# Create group (interactive TUI builder)
pattern group create

# Add member
pattern group add member "Planning" Flux --role specialist

# View group status
pattern group status "Planning"

# Export group configuration
pattern group export "Planning" -o planning.toml
```

## Integration Points

### Discord
```rust
// Route message to appropriate group
let group_id = select_group(&message, &user_state);
let events = group_manager.route_message(&group, &agents, message).await?;

// Stream responses back to Discord
while let Some(event) = events.next().await {
    match event {
        GroupResponseEvent::TextChunk { agent_id, text, .. } => {
            discord.send_typing(&channel_id).await?;
            discord.send_message(&channel_id, &text).await?;
        }
        // ...
    }
}
```

### MCP Server
Groups can be exposed as MCP tools:
```json
{
  "name": "route_to_group",
  "description": "Route a message through an agent group",
  "inputSchema": {
    "type": "object",
    "properties": {
      "group_id": { "type": "string" },
      "message": { "type": "string" }
    }
  }
}
```
