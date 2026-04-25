# Pattern ADHD Assistant - Development Conversation

## Initial Context

Starting from the Agent Architecture research report, the discussion evolved around why not use sleeptime_agent, multi-agent shared memory, and multi-agent custom tools from Letta's documentation.

### Key Insights on Sleeptime Agents

**sleeptime_agent** is particularly suited for ADHD support:
- Background processing for proactive checks (detecting 3-hour hyperfocus sessions)
- Time-based reminders that fire without user prompting
- Monitoring task deadlines and energy patterns
- Autonomous scheduled functions for external executive function

The original report undersold this - these agents can run scheduled functions autonomously, which is exactly what ADHD brains need.

### Multi-Agent Architecture Benefits

**Multi-agent shared memory** enables:
- Real-time state sync between agents
- Shared working memory pool
- No redundant context storage

**Multi-agent custom tools** opens up:
- Task breakdown tool shared by all agents
- Universal context capture any agent can invoke
- Energy state assessment available to all agents

## The Revised Architecture

### Core Structure

```
Pattern (Sleeptime Orchestrator)
├── Entropy (Task/Complexity Agent)
├── Flux (Time/Scheduling Agent)  
├── Archive (Memory/Knowledge Agent)
├── Momentum (Flow/Energy Agent)
└── Anchor (Habits/Structure Agent)
```

Pattern runs background checks every 20-30 minutes:
- Attention drift patterns
- Physical needs
- Transition warnings
- Energy/task misalignment
- Proactive check-ins

### Shared Memory Blocks

```toml
[memory.current_state]
value = "energy: 6/10 | attention: fragmenting | last_break: 127min | mood: focused_frustration"
limit = 200

[memory.active_context]
value = "task: letta integration | start: 10:23 | progress: 40% | friction: api auth unclear"
limit = 400

[memory.bond_evolution]
value = "trust: building | humor: dry->comfortable | formality: decreasing | shared_refs: ['time is fake', 'brain full no room']"
limit = 600
```

## The Stormlight Archive Connection

The architecture mirrors Shallan Davar's cognitive system from Brandon Sanderson's Stormlight Archive:

- **Pattern (spren)** coordinates between Shallan's personas
- **Veil** - espionage/coping persona
- **Radiant** - determination/structure persona
- **Shallan** - creative core
- They develop "The Compact" to work together

Our architecture parallels this:
- Pattern (orchestrator) = Pattern the spren coordinating
- Specialized agents = Shallan's personas with distinct roles
- Shared memory = The Compact/shared consciousness
- Each agent having strengths = Veil handles pain, Radiant gets shit done

### Key Difference: Inverting Trauma Response

Shallan's system emerged from trauma - fracturing to survive. We're building the opposite:
- Intentional cognitive multiplicity as support infrastructure
- "My executive function needs backup" vs "I can't handle this memory"
- Proactive support before breakdown
- Specialized helpers by design not necessity
- Truth-speaking for growth not just survival

## Technical Implementation

### MCP Server Architecture

Single MCP server with tool visibility configured per agent:

```typescript
// Pattern gets everything
pattern_tools = ["*"]

// Entropy gets task stuff + shared state
entropy_tools = [
  "check_state",
  "update_state",
  "task_breakdown",
  "estimate_energy_cost",
  "find_next_action"
]

// Flux gets time/scheduling + state
flux_tools = [
  "check_state",
  "update_state",
  "time_estimate",
  "add_buffers",
  "transition_warning"
]
```

### Accuracy and Grounding

Critical requirement: agents must be accurate with appointments/facts. ADHD brains can't afford hallucinated details.

Needs dual retrieval:
- Semantic search for concepts/patterns
- Exact text search for facts/details
- Structured data store for critical info

```python
# Not just semantic search
results = semantic_search("meeting tomorrow")

# But also exact matching
exact_matches = text_search(
    query="tomorrow",
    fields=["datetime", "attendees", "location"],
    exact_match=True
)
```

Confidence gradations:
- **Certain**: from explicit source (calendar event, email)
- **Probable**: strong pattern match ("you usually meet Sarah Tuesdays")
- **Possible**: weak inference ("might be the Sarah from marketing?")
- **Uncertain**: best guess

## Cost Optimization Strategy

### Tiered Model Approach

Initial thinking about model tiers:
```typescript
agent_models = {
  pattern: "opus",        // orchestration needs smarts
  entropy: "opus",        // task complexity analysis
  flux: "sonnet",         // time math doesn't need genius
  archive: "haiku",       // mostly retrieval coordination
  momentum: "sonnet",     // pattern recognition
  anchor: "haiku"         // routine checks
}
```

### Better: Orchestrator + Local Execution

Pattern (Opus) as planner:
- Reads situation
- Decides which agents/tools needed
- Writes execution plan
- Handles ambiguity

Specialist agents (local models) as executors:
- Follow Pattern's specific instructions
- No complex decision making
- Just "retrieve X, format Y, return"

### Reflex Agent Pattern

Pattern as dispatcher with a default response agent:
- **Reflex** runs on Sonnet/local
- Handles 80% of interactions
- Pattern only engages for complex coordination

```typescript
// Pattern's quick check (Opus)
const needs = analyzeRequest(input)
if (needs.complexity < 7 && !needs.coordination_required) {
  return delegate_to_reflex(input, needs.context)
}
```

### OAuth Integration

Using Claude OAuth instead of API keys would make Opus usage sustainable:
- Pattern on Opus via OAuth (already paid for with Claude Pro)
- Other agents on local/cheaper models
- Hybrid approach for different use cases

Since Letta doesn't support OAuth natively, would need to:
- Run a proxy service handling OAuth flow
- Translate Letta's API calls to Claude chat format
- Present as OpenAI-compatible endpoint to Letta

## Summary

The Pattern ADHD Assistant architecture evolved from a straightforward multi-agent system to a sophisticated cognitive support framework that:

1. Mirrors healthy cognitive multiplicity patterns from fiction (Stormlight Archive)
2. Inverts trauma-response patterns into proactive support
3. Uses sleeptime orchestration for autonomous executive function
4. Implements shared memory for coordination without redundancy
5. Prioritizes accuracy through dual retrieval mechanisms
6. Optimizes costs through tiered model usage and OAuth integration

The system serves as external executive function while maintaining a evolving, trust-building relationship with the user - moving from professional assistant to trusted cognitive partner.