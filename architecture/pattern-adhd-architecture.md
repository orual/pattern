# Pattern Architecture - Multi-Agent Cognitive Support System

Pattern is a multi-agent cognitive support system designed specifically for ADHD brains. It uses a multi-agent architecture with shared memory to provide external executive function through specialized cognitive agents inspired by Brandon Sanderson's Stormlight Archive.

## Agent Constellation

```
Pattern (Sleeptime Orchestrator)
├── Entropy (Task/Complexity Agent)
├── Flux (Time/Scheduling Agent)
├── Archive (Memory/Knowledge Agent)
├── Momentum (Flow/Energy Agent)
└── Anchor (Habits/Structure Agent)
```


## Core Features
- **Native Agent Groups**: Flexible agent coordination with overlapping groups
- **Three-Tier Memory**: Core blocks (immediate), searchable archival, and deep storage
- **Cost-Optimized Sleeptime**: Two-tier monitoring with rules-based checks + AI intervention
- **Passive Knowledge Sharing**: Agents write insights to embedded documents for semantic search
- **ADHD-Specific Design**: Time blindness compensation, task breakdown, energy tracking, interruption awareness
- **Evolving Relationship**: Agents develop understanding of user patterns over time
- **MCP Client/Server**: Consume external tools or expose Pattern capabilities

For detailed architecture, see [Memory and Groups Architecture](./memory-and-groups.md).

## ADHD-Specific Design Principles

Pattern is built on deep understanding of ADHD cognition:

### Core Principles
- **Different, Not Broken**: ADHD brains operate on different physics - time blindness and hyperfocus aren't bugs
- **External Executive Function**: Pattern provides the executive function support that ADHD brains need
- **No Shame Spirals**: Never suggest "try harder" - validate struggles as logical responses
- **Hidden Complexity**: "Simple" tasks are never simple - everything needs breakdown
- **Energy Awareness**: Attention and energy are finite resources that deplete non-linearly

### Key Features for ADHD
- **Time Translation**: Automatic multipliers (1.5x-3x) for all time estimates
- **Proactive Monitoring**: Background checks prevent 3-hour hyperfocus crashes
- **Context Recovery**: External memory for "what was I doing?" moments
- **Task Atomization**: Break overwhelming projects into single next actions
- **Physical Needs**: Track water, food, meds, movement without nagging
- **Flow Protection**: Recognize and protect rare flow states

### Relationship Evolution
Agents evolve from professional assistant to trusted cognitive partner:
- **Early**: Helpful professional who "gets it"
- **Building**: Developing shorthand, recognizing patterns
- **Trusted**: Inside jokes, gentle ribbing, shared language
- **Deep**: Finishing thoughts about user's patterns

## Multi-Agent Architecture

Pattern uses flexible groups for agent coordination:

### Flexible Group Patterns
Groups are created dynamically based on needs:
- Different coordination patterns (supervisor, round-robin, pipeline, dynamic, voting, sleeptime)
- Overlapping membership - same agents in multiple groups
- Context-specific coordination styles
- Experiment and evolve group configurations

### Memory Hierarchy (Loro CRDT + SQLite)
1. **Core Memory Blocks** (Always in context):
   - `current_state`: Real-time status
   - `active_context`: Recent important events
   - `bond_evolution`: Relationship dynamics

2. **Archival Memory** (Searchable via FTS5):
   - Agent observations and insights
   - Pattern detection across time
   - Accumulated wisdom

3. **Vector Store** (sqlite-vec):
   - Semantic search for related memories
   - 384-dimensional embeddings

See [Memory and Groups Architecture](./memory-and-groups.md) for implementation details.

## Shared Memory Blocks

All agents share these memory blocks for coordination without redundancy:

```rust
// Shared state accessible by all agents via MemoryStore
// Real-time energy/attention/mood tracking (200 char limit)
current_state: "energy: 6/10 | attention: fragmenting | last_break: 127min | mood: focused_frustration"

// What they're doing NOW, including blockers (400 char limit)
active_context: "task: integration | start: 10:23 | progress: 40% | friction: api auth unclear"

// Growing understanding of this human (600 char limit)
bond_evolution: "trust: building | humor: dry->comfortable | formality: decreasing"
```

### Agent Communication

Agents coordinate through:
- Shared memory updates (all agents see changes via MemoryStore)
- Message routing via AgentMessageRouter
- Shared tools that any agent can invoke

## Agent Details

### Pattern (Sleeptime Orchestrator)
- Runs background checks every 20-30 minutes
- Monitors hyperfocus duration, physical needs, transitions
- Coordinates other agents based on current needs
- Personality: "friend who slides water onto your desk"

### Entropy (Task/Complexity Agent)
- Breaks down overwhelming tasks into atoms
- Recognizes hidden complexity in "simple" tasks
- Validates task paralysis as logical response
- Finds the ONE next action when everything feels impossible

### Flux (Time/Scheduling Agent)
- Translates between ADHD time and clock time
- Automatically adds buffers (1.5x-3x multipliers)
- Recognizes time blindness patterns
- Creates temporal anchors for transitions

### Archive (Memory/Knowledge Agent)
- External memory bank for dumped thoughts
- Surfaces relevant context without prompting
- Finds patterns across scattered data points
- Answers "what was I doing?" with actual context

### Momentum (Flow/Energy Agent)
- Distinguishes hyperfocus from burnout
- Maps energy patterns (Thursday 3pm crash, 2am clarity)
- Suggests task pivots based on current capacity
- Protects flow states when they emerge

### Anchor (Habits/Structure Agent)
- Tracks basics: meds, water, food, sleep
- Builds loose structure that actually works
- Celebrates basic self-care as real achievements
- Adapts routines to current capacity

## Work & Social Support Features

Pattern's agents provide specific support for contract work and social challenges:

### Contract/Client Management
- **Time Tracking**: Flux automatically tracks billable hours with "what was I doing?" recovery
- **Invoice Reminders**: Pattern notices unpaid invoices aging past 30/60/90 days
- **Follow-up Prompts**: "Hey, you haven't heard from ClientX in 3 weeks, might be time to check in"
- **Meeting Prep**: Archive surfaces relevant context before client calls
- **Project Switching**: Momentum helps context-switch between clients without losing state

### Social Memory & Support
- **Birthday/Anniversary/Medication Tracking**: Anchor maintains important dates with lead-time warnings
- **Conversation Threading**: Archive remembers "they mentioned their dog was sick last time"
- **Follow-up Suggestions**: "Sarah mentioned her big presentation was today, maybe check how it went?"
- **Energy-Aware Social Planning**: Momentum prevents scheduling social stuff when depleted
- **Masking Support**: Pattern tracks social energy drain and suggests recovery time

Example interactions:
```
Pattern: "heads up - invoice for ClientCorp is at 45 days unpaid. want me to draft a friendly follow-up?"

Archive: "before your 2pm with Alex - last meeting you promised to review their API docs (you didn't)
and they mentioned considering migrating to Leptos"

Anchor: "Mom's birthday is next Tuesday. you usually panic-buy a gift Monday night.
maybe handle it this weekend while you have energy?"

Momentum: "you've got 3 social things scheduled this week. based on last month's pattern,
that's gonna wreck you. which one can we move?"
```

## Data Architecture

### Embedded Storage (SQLite + Loro)

All data stored locally using embedded databases:

```rust
// pattern_db provides SQLite storage
use pattern_db::ConstellationDb;

// Memory uses Loro CRDT for versioning
use pattern_core::memory::{MemoryCache, MemoryStore};

// Combined databases
let dbs = ConstellationDatabases::open("./constellation.db", "./auth.db").await?;
let memory = MemoryCache::new(dbs.clone());
```

**SQLite** handles:
- Agent configs and state
- Messages and conversations
- Memory block persistence
- Full-text search via FTS5

**Loro CRDT** provides:
- Versioned memory blocks
- Conflict-free merging
- Time-travel for rollback

**sqlite-vec** enables:
- 384-dimensional vector search
- Semantic memory search
- No external vector DB needed

### Memory Tiers
- **Core**: Current context (configurable limit) - in Loro
- **Working**: Short-term memory (configurable limit) - in Loro, loadable temporarily or pinnable in context
- **Archival**: FTS5 + sqlite-vec for unlimited searchable storage
- **Message**: Conversation history with summaries
- **Embeddings**: Stored as blobs, indexed via sqlite-vec

### Built-in Agent Tools

Pattern agents come with built-in tools via BuiltinTools:

```rust
// Built-in tools are registered via the runtime
let builtin = BuiltinTools::new(runtime.clone());
builtin.register_all(&tool_registry);
```

**Standard Tools**:
- `block`: Memory block operations (append, replace, archive, load, swap)
- `recall`: Archival memory operations (insert, read, delete)
- `search`: Unified search across memory and conversations
- `send_message`: Route messages to users, agents, groups, or channels

**Customization**:
For a custom memory backend, implement `MemoryStore` and provide it to RuntimeContext:

```rust
let ctx = RuntimeContext::builder()
    .dbs_owned(dbs)
    .memory(Arc::new(CustomMemoryStore::new()))
    .build()
    .await?;
```

**AgentRuntime Architecture**:
- Per-agent runtime with memory, tools, messages, routing
- Tools access services through `ToolContext` trait
- MemoryStore abstracts over storage implementation
- Thread-safe with DashMap for concurrent access
