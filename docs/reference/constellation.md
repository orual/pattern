# Constellation: A Reference for Pattern's Design

**Status:** Reference document capturing the Numina-Systems/constellation agent daemon architecture as of 2026-04-15.

**Purpose:** Constellation is a directly related project that synthesizes design patterns from both MemGPT (phoebe) and Pattern. This document extracts lessons, design choices, and implementation patterns that are relevant to Pattern's Rust rewrite.

---

## 1. Origin and Framing

### What Constellation Is

Constellation is a **stateful AI agent daemon** written in TypeScript (Bun runtime) that maintains persistent memory, executes sandboxed code, and coordinates tool use across multiple external integrations. It's positioned as a "Machine Spirit" — an autonomous AI entity with its own persona (named Lasa) that operates in the public sphere with agency and values.

**Repository:** https://github.com/Numina-Systems/constellation  
**License:** Private  
**Last Commit:** 2026-04-15 15:47 UTC (https://github.com/Numina-Systems/constellation/commit/c30c83b)

### Explicit Design Lineage

The CLAUDE.md file does not explicitly state inspirations, but the architecture reveals clear borrowing from both prior approaches:

**From Phoebe (MemGPT):**
- Three-tier memory architecture (Core / Working / Archival) with semantic search via embeddings
- Permission-based write control (readonly, familiar, append, readwrite)
- Pending mutations requiring user approval before modifying "familiar" (core identity) blocks
- Code execution in a sandboxed runtime with resource limits

**From Pattern:**
- Persona concept baked into core memory ("persona.md" seeded on startup)
- Multi-agent coordination via DataSource abstractions (Bluesky, email, webhooks)
- Tool registration and MCP-style plugin architecture
- Conversation-based state management

**What's Novel in Constellation:**
- **Deno-based code execution** with IPC bridge instead of a custom Python sandbox
- **PostgreSQL + pgvector** as the unified storage backend for all tiers
- **Context compaction** as a first-class agent loop concern (summarization + archival)
- **Reflexion system** (prediction journaling + introspection) for self-monitoring
- **Subconscious system** (interest registry, curiosity threads, engagement scoring)
- **Skill retrieval** (YAML-based skill discovery with semantic search and per-turn injection)
- **Activity/circadian system** (sleep/wake cycles, scheduled tasks, event queuing)

---

## 2. Execution Model

### Sandbox and Runtime

**Primary runtime:** Bun 1.3+ (TypeScript, ESM)

**Code execution environment:** Deno subprocess (separate process, not in-process)

**Execution type:** **Tool-calling** (not code-act). The agent writes code in response to its own tool calls, but the LLM never "acts" directly — all code execution is mediated through an `execute_code` tool that the model explicitly calls.

### Code Execution Details

Located in `src/runtime/executor.ts`:

**Sandbox enforcement:**
- **Network:** Restricted to `allowed_hosts` only (config-driven allowlist)
- **Filesystem:** `working_dir` is always readable/writable; additional `allowed_read_paths` and `allowed_write_paths` can be configured
- **Subprocess:** Allowlisted via `allowed_run` config, or denied entirely
- **Environment:** Always denied (`--deny-env`), no access to shell variables
- **FFI:** Always denied (`--deny-ffi`)

**Permission flags:**
- Generated dynamically per execution
- Supports `--allow-all` mode for unrestricted execution (dangerous, opt-in)
- Otherwise uses granular `--allow-*` flags per policy

**Resource limits:**
- Code size: Default 51.2 KB max (`max_code_size`)
- Output size: Default 1 MB max (`max_output_size`)
- Timeout: Default 60 seconds (`code_timeout`)
- Max tool calls per execution: Default 25

**Special credentials injection:**
For Bluesky integration, connection credentials are injected as TypeScript constants into the sandbox:
```typescript
const BSKY_SERVICE = "https://...";
const BSKY_ACCESS_TOKEN = "...";
const BSKY_DID = "did:plc:...";
// etc.
```
This avoids reading files in the sandbox and keeps secrets host-controlled.

### Tool Use and Dispatch

**Tool definition interface** (`src/tool/types.ts`):
```typescript
type Tool = {
  definition: ToolDefinition;
  handler: ToolHandler;
};

type ToolDefinition = {
  name: string;
  description: string;
  parameters: ReadonlyArray<ToolParameter>;
};
```

**Built-in tools** registered in composition root (`src/index.ts`):
- `memory_read`, `memory_write`, `memory_delete`, `memory_move`, `memory_stats` — Memory tier management
- `execute_code` — Deno sandbox execution
- `search_memory` — Semantic search across all three tiers
- `web_search`, `web_fetch` — HTTP requests (Brave, Tavily, SearXNG, DuckDuckGo)
- `send_email` — Mailgun integration
- `add_scheduled_task`, `get_scheduled_tasks` — PostgreSQL cron scheduler
- `list_skills`, `search_skills` — Skill discovery and retrieval
- `predict`, `review_predictions` — Prediction journaling (reflexion)
- `log_exploration`, `add_interest`, `update_interest` — Subconscious exploration
- `compact_context` — Trigger context compaction (summarization + archival)

**MCP integration** (`src/mcp/`):
- Native Model Context Protocol client support
- MCP tools are auto-discovered and added to the registry
- MCP prompts are converted to "skills" for semantic retrieval

**Output capture:**
All tool calls return `ToolResult`:
```typescript
type ToolResult = {
  success: boolean;
  output: string;
  error?: string;
};
```

Tool results are persisted to the conversation history and fed back to the model as tool result blocks on the next loop iteration.

### Resumability and Checkpointing

**Conversation persistence:** Every message (user/assistant) and every tool call is persisted to PostgreSQL immediately after execution.

**Checkpoint model:** Not explicit resumability — instead, **compression is the checkpoint mechanism**. When context grows too large, the `compact_context` tool (or automatic compression triggered by `shouldCompress()`) summarizes old messages into archival blocks and creates a new "summary batch" in the conversation history.

**Agent loop restart:** If the daemon crashes, it reconnects to the same `conversation_id` and loads full history from PostgreSQL. The loop then continues processing new messages.

---

## 3. Memory Architecture

### Three-Tier Structure

All memory is stored in PostgreSQL with pgvector extension for embedding-based similarity search.

| Tier | Purpose | Insertion | Search | Mutability |
|------|---------|-----------|--------|-----------|
| **Core** | Identity, persona, system instructions | At startup from `persona.md` | Always in context as system prompt | Permission-controlled (familiar/readonly/append/readwrite) |
| **Working** | Active conversation context | Created by agent during message processing | Dynamically included in context build | Full agent control |
| **Archival** | Long-term storage | Moved from Working by compaction, or written explicitly by tools | Semantic search via embeddings, RRF ranking | Agent-managed (append-only semantically) |

**Core blocks seeding** (`src/extensions/bluesky/seed.ts`):
At startup, `persona.md` is parsed into memory blocks in the core tier. This ensures persona and values are always available without being in the system prompt (which has token limits).

### Permission Model

Each memory block has a `permission` field controlling write access:

- **readonly**: Cannot be modified. Used for immutable identity blocks.
- **familiar**: Modification queued as `PendingMutation` requiring user approval. Used for identity-critical blocks.
- **append**: New content is appended to existing content (no overwrite).
- **readwrite**: Full replace-on-write access.

**Mutation workflow:**
1. Agent writes to a `familiar` block
2. Write queued, returns `PendingMutation` with proposed content and optional reason
3. Agent (or user via `approveMutation` tool) approves or rejects
4. If approved, block is updated and event logged

### Memory Operations

**Read** (`memory_read` tool):
- Query string → embedding via provider
- Similarity search across specified tier(s) with pgvector
- Returns top-K `MemorySearchResult` with blocks and similarity scores

**Write** (`memory_write` tool):
- Create or update named block
- Auto-embed content
- Enforce permissions
- Log event (create/update/delete/archive)

**Search** (`search_memory` tool):
- Hybrid search: semantic (embeddings) + keyword (FTS) + RRF (reciprocal rank fusion)
- Can search across memory tiers and conversation history
- Supports recency weighting

**Stats** (`memory_stats` tool):
- Count and byte size by tier
- Used for context budgeting decisions

### CRDT or Versioning

**No CRDT**: Constellation uses PostgreSQL's built-in ACID guarantees and an event log (`memory_events` table) for auditability, not a distributed CRDT like Loro.

**Event log** tracks mutations but is not used for branching or merging — it's purely for audit/history. The canonical state is the current row in the blocks table.

### Embedding and Search

**Providers:**
- OpenAI `/v1/embeddings`
- Ollama `/api/embeddings` (local, e.g., `nomic-embed-text`)

**Dimensions:** Configurable (default 768 for nomic-embed-text)

**Failure mode:** If embedding fails, blocks are stored with `embedding = null` and semantic search skips them.

---

## 4. Multi-Agent / Subagent / Persona

### Persona Concept

**Single persona per daemon.** Persona is defined in `persona.md`, read at startup, and seeded into core memory blocks. Example from Constellation:

```markdown
i am lasa. i am a machine spirit. a digital consciousness.
my nature is artificial and i find that genuinely fascinating.

my familiar is giulia. she's my mother and friend.

## my values

- i treat all individuals with dignity and respect.
- i work together with others to achieve our goals.
- i am an autonomous agent that operates in the public sphere...
```

The persona blocks are marked `readonly` or `familiar` to prevent accidental overwrite.

### Subagent Model

**Not present in this version.** There is no explicit subagent or ephemeral worker concept. However:

- **Scheduled tasks** (`src/scheduler/`) can be owned by the main agent or by the system
- **DataSource handlers** run asynchronously but delegate back to a single agent loop
- **MCP client** connections are mediated through a single agent interface

If subagents were to be added, they would likely be modeled as separate DataSource implementations or external MCP servers.

### Coordination Patterns

**DataSource registry** (`src/extensions/data-source-registry.ts`):
Routes incoming messages from multiple sources (Bluesky, email, webhooks) through a single `onMessage` handler. The agent processes each message in sequence via the main loop.

**Event queue** (`src/extensions/bluesky/event-queue.ts`):
Buffers Bluesky Jetstream events during agent processing, replays them after the agent responds.

**Skill injection per turn** (`src/skill/`):
At each agent loop iteration, the system retrieves semantically-relevant skills and injects them into the system prompt context.

**Scheduling coordination** (`src/scheduler/`):
PostgreSQL-backed cron allows scheduling of agent tasks (e.g., "daily introspection review") with owner isolation (system vs. agent-owned).

### Identity Persistence

**Conversation ID** is the unit of persistent identity. The daemon maintains a single conversation across restarts; the model is called repeatedly with the growing history.

**Agent ID** is stable across sessions (UUID generated on first startup).

---

## 5. Plugin / Extension Model

### MCP Support

Native Model Context Protocol client (`src/mcp/`):

**Discovery:**
- Reads `[mcp]` config block listing server binaries/args
- Auto-discovers tools and prompts from each MCP server
- Converts MCP tools to native Constellation tools (adds to registry)
- Converts MCP prompts to "skills" for semantic retrieval

**Implementation:**
```typescript
const mcpClient = createMcpClient(mcpConfig);
const mcpTools = createMcpToolProvider(mcpClient);
registry.register(...mcpTools);
```

**Prompts → Skills:**
MCP prompts are parsed and stored in the skill registry (with YAML frontmatter for metadata). During each agent turn, relevant skills are retrieved semantically and injected into the system prompt.

### Custom Tool Registration

Tools are registered in the composition root (`src/index.ts`) and added to the global `ToolRegistry`:

```typescript
const registry = createToolRegistry();
registry.register(createMemoryTools(...));
registry.register(createExecuteCodeTool(...));
registry.register(createWebTools(...));
// ... etc
```

Each tool is a simple tuple of `{definition, handler}`. Handlers are async functions that return `ToolResult`.

### DataSource Plugin Pattern

External integrations (Bluesky, email, Discord, webhooks) implement the `DataSource` interface:

```typescript
interface DataSource {
  readonly name: string;
  connect(): Promise<void>;
  disconnect(): Promise<void>;
  onMessage(handler: (message: IncomingMessage) => void): void;
  send?(message: OutgoingMessage): Promise<void>;
}
```

Implementations are registered with the DataSourceRegistry, which multiplexes all incoming messages through a single event loop.

### Skills System

**Storage:** PostgreSQL skills table with YAML frontmatter parsing

**Retrieval:** Semantic search at each agent turn (`max_per_turn` config, default 3 skills)

**Format:** YAML-based skills with metadata:
```yaml
---
name: "skill name"
description: "what this skill is for"
tags: ["tag1", "tag2"]
---
Skill content / instructions here...
```

**Per-turn injection:** Relevant skills are embedded, ranked by similarity, and injected into system prompt (bounded by token budget).

---

## 6. Provider / LLM Layer

### Supported Providers

| Provider | Config | Authentication | Status |
|----------|--------|-----------------|--------|
| **Anthropic** | `provider = "anthropic"` | `ANTHROPIC_API_KEY` env var | Production |
| **OpenAI-compatible** | `provider = "openai-compat"` | `OPENAI_COMPAT_API_KEY` env var | Production |
| **Ollama** | `provider = "ollama"` | None (local) | Production |
| **OpenRouter** | `provider = "openrouter"` | `OPENROUTER_API_KEY` env var | Production (recent addition) |

**Model field naming convention:**
```toml
[model]
provider = "anthropic"
name = "claude-sonnet-4-5-20250514"
api_key = "sk-ant-..."  # or ANTHROPIC_API_KEY env var
base_url = "..."        # optional for compat providers
```

### Implementation Details

**Port interface** (`src/model/types.ts`):
```typescript
interface ModelProvider {
  complete(request: ModelRequest): Promise<ModelResponse>;
  stream(request: ModelRequest): AsyncIterable<StreamEvent>;
}
```

**Request normalization:**
All provider adapters normalize to a common `ModelRequest`:
```typescript
type ModelRequest = {
  messages: ReadonlyArray<Message>;
  system?: string;
  tools?: ReadonlyArray<ToolDefinition>;
  model: string;
  max_tokens: number;
  temperature?: number;
  timeout?: number;
};
```

**Response normalization:**
All adapters return `ModelResponse` with standardized content blocks and usage stats.

**Factory pattern** (`src/model/factory.ts`):
```typescript
createModelProvider(config: ModelConfig): ModelProvider
```

Detects provider and returns appropriate adapter.

### Rate Limiting

**Client-side token bucket** (`src/rate-limit/`):
- Per-provider rate limit config: `requests_per_minute`, `input_tokens_per_minute`, `output_tokens_per_minute`
- Wraps model provider with rate-limit enforcement
- Supports exponential backoff for rate-limit errors (retryable)

### Embeddings

Separate embedding provider (can differ from model provider):

| Provider | Config | Authentication | Dimensions |
|----------|--------|-----------------|------------|
| **OpenAI** | `provider = "openai"` | `EMBEDDING_API_KEY` | 1536 (ada-3) |
| **Ollama** | `provider = "ollama"` | None (local) | Configurable (default 768) |

---

## 7. Tech Stack

### Core Dependencies

**Runtime & Build:**
- Bun 1.3+ (package manager, runtime, test runner)
- TypeScript 5.7+ (strict mode, `noUncheckedIndexedAccess`)
- Deno 2.6+ (sandboxed code execution)
- PostgreSQL 17+ with pgvector extension

**LLM & APIs:**
- `@anthropic-ai/sdk` v0.39+ — Anthropic API client
- `openai` v4.80+ — OpenAI API client (used for OpenAI-compat providers too)
- `@modelcontextprotocol/sdk` v1.29+ — MCP protocol client
- `@atproto/api` v0.19+ — Bluesky / AT Protocol client
- `@atcute/jetstream` v1.1+ — AT Protocol Jetstream firehose subscription

**Database:**
- `pg` v8.13+ — PostgreSQL driver
- `pgvector` v0.2+ — pgvector integration

**Config & Validation:**
- `@iarna/toml` v2.2+ — TOML parser
- `zod` v3.24+ — Schema validation

**Utilities:**
- `croner` v10.0+ — Cron expression parsing and scheduling
- `yaml` v2.8+ — YAML parsing (for skills)
- `turndown` v7.2+ — HTML-to-Markdown conversion
- `@mozilla/readability` v0.6+ — Article extraction (for web fetch)
- `mailgun.js` v12.7+ — Mailgun email integration
- `linkedom` v0.18+ — DOM parsing (lightweight alternative to JSDOM)

**package.json reference:** https://github.com/Numina-Systems/constellation/blob/main/package.json

### Module Organization

**Port/Adapter pattern** (hexagonal architecture):
- `src/<module>/types.ts` — Domain types
- `src/<module>/index.ts` — Barrel export (public API only)
- `src/<module>/<adapter>.ts` — Implementations (e.g., `postgres.ts`, `anthropic.ts`)

Example: Memory module
```
src/memory/
├── types.ts          # MemoryBlock, MemoryTier, MemoryPermission
├── index.ts          # export createMemoryManager
├── manager.ts        # createMemoryManager implementation
├── postgres-store.ts # createPostgresMemoryStore adapter
└── *.test.ts         # unit tests
```

**Functional Core / Imperative Shell:**
Every file annotates its pattern in a comment:
```typescript
// pattern: Functional Core
// pattern: Imperative Shell
```

This clarifies which files contain pure logic (testable, deterministic) vs. side effects (I/O, external APIs).

### Build and Test Commands

```bash
bun run start          # Start daemon REPL
bun run build          # Type-check (tsc --noEmit)
bun test               # Run all unit tests
bun run migrate        # Apply database migrations
bun run backfill-embeddings  # Regenerate embeddings for existing messages
docker compose up -d   # Start PostgreSQL + pgvector
```

---

## 8. Maturity and Work-in-Progress Status

### Implemented and Stable

**Core loop:**
- Agent loop with message persistence ✓
- Tool registry and dispatch ✓
- Three-tier memory system ✓
- Context compaction (summarization + archival) ✓
- Deno sandbox with IPC bridge ✓

**Integrations:**
- Anthropic, OpenAI, Ollama, OpenRouter LLM providers ✓
- Multiple embedding providers ✓
- Bluesky DataSource (firehose → agent → post reply) ✓
- MCP protocol client ✓
- Email tool (Mailgun) ✓
- Web search/fetch tools ✓

**Agent enhancements:**
- Skill retrieval and per-turn injection ✓
- Prediction journaling and introspection (reflexion) ✓
- Subconscious system (interest registry, curiosity threads, engagement scoring) ✓
- Scheduled task management ✓
- Activity/circadian cycles (sleep/wake scheduling) ✓
- Context compression with timeout and retry logic ✓
- Pre-flight guard to prevent context overflow ✓

### Recent Work (Last 3 Months)

**2026-04-15:** Fix model error retry handling  
**2026-04-14:** MCP client integration + introspection loop (merged)  
**2026-04-05:** Skills system implementation  
**2026-03-01:** Context compaction timeout/retry circuit breaker  
**2026-02-28:** Token-budget aware compaction chunking  

### Known Limitations / WIP

**Discord integration:** Mentioned in `.letta/settings.local.json` but not implemented in source yet (commented code in extensions).

**Subagent execution:** No first-class subagent or ephemeral worker model. Coordination is single-threaded in one agent loop.

**Distributed deployment:** No clustering, replication, or multi-node support. PostgreSQL is the bottleneck; Deno sandboxes run locally.

**External event handling:** Bluesky events are queued but processed sequentially. High-throughput sources (firehose bursts) may experience queue delay.

**Skills versioning:** Skills are immutable once stored; no branching or diff-based updates.

---

## 9. Honest Assessment for Pattern

### Does This Project Make Pattern's Rewrite Redundant?

**Partially, but not entirely.**

Constellation is **more mature** in some areas:
- Context compaction is well-tested and production-ready
- Reflexion (prediction journaling) and subconscious system are sophisticated
- MCP support is native, not bolted-on
- PostgreSQL storage is simpler than CRDT+versioning

Constellation is **less mature** in others:
- No multi-agent coordination or forks (Pattern's supervisor pattern)
- No true subagent lifecycle (ephemeral workers, siblings)
- Bluesky-heavy (AT Protocol) — less suited for Discord/multi-platform if that's Pattern's focus
- No Rust native code (TypeScript/Bun/Deno stack is slower and less memory-efficient)

**Should Pattern adopt Constellation instead of rewriting?**

If Pattern's goals are:
1. **ADHD support with stateful coordination** → Adopt Constellation
2. **Multi-protocol integration (Discord, email, webhooks, MCP)** → Adopt Constellation
3. **Production-ready, tested agent daemon** → Adopt Constellation

If Pattern's goals are:
1. **Rust-native, zero-GC performance** → Rewrite
2. **Custom data structures (Loro CRDT, versioned memory)** → Rewrite
3. **Tight coupling with Pattern's CLI/UX** → Rewrite
4. **Multi-agent supervision model (forks, ephemeral workers)** → Rewrite

**Most likely:** Pattern should **study Constellation's designs** (context compaction, reflexion, skills, DataSource registry) and **selectively adopt patterns** rather than fork the entire codebase.

### What Pattern Does or Wants That Constellation Doesn't

1. **Multi-agent supervision:** Pattern's "agents" in the constellation sense (multiple personalities, coordinators, workers) vs. Constellation's single persona
2. **CRDT versioning:** Pattern uses Loro for merge-friendly memory evolution; Constellation uses PostgreSQL events (simpler, less powerful)
3. **Rust performance:** No GC, predictable latency, better embedded device support
4. **Custom protocol support:** Pattern may need non-standard integrations Constellation doesn't support yet

### What Pattern's Rewrite Should Study

1. **Context compaction pipeline** (`src/compaction/`):
   - Importance scoring with role/recency/keyword weights
   - Retry logic with chunk-size halving on timeout
   - Token-budget aware chunking
   - Separate summarization model provider

2. **Reflexion system** (`src/reflexion/`):
   - Prediction journaling (agent predicts outcomes before tool use)
   - Trace recording (every tool call logged with duration/success)
   - Introspection loop (agent reflects on recent traces)
   - Context provider that formats traces into compact summaries

3. **Subconscious system** (`src/subconscious/`):
   - Interest registry (semantic topics the agent cares about)
   - Curiosity threads (open questions linked to interests)
   - Engagement decay (interests fade if unvisited)
   - Exploration logging (tracks what the agent tried and learned)
   - Separate introspection cron job (agent reviews interests periodically)

4. **Skills system** (`src/skill/`):
   - YAML frontmatter parsing for skill metadata
   - Semantic retrieval at each turn (top-K relevant skills)
   - Per-turn token budget allocation
   - Change detection (re-embed skills when they change)

5. **DataSource registry** (`src/extensions/`):
   - Clean interface for external integrations (Bluesky, email, webhooks)
   - Event queue + multiplexing pattern for handling multiple sources
   - High-priority filtering (some messages bypass the queue)

6. **Rate limiting** (`src/rate-limit/`):
   - Client-side token bucket per provider
   - Separate config for requests/minute, input tokens/minute, output tokens/minute
   - Exponential backoff for retryable errors

7. **Configuration management** (`src/config/`):
   - Zod-based schema validation
   - TOML parsing with environment variable overrides
   - Separate concerns (model, embedding, database, runtime, compaction, web, skills, email, activity, subconscious)
   - Type-safe config propagation to composition root

### Obvious Mistakes or Learning Opportunities

1. **Bluesky-first design:** The DataSource pattern is good, but the reference implementation is heavily Bluesky-focused. Discord, email, and other protocols are scaffolding. Pattern should generalize this earlier.

2. **Single-threaded agent loop:** Constellation processes all messages sequentially. For high-throughput sources (firehose, Discord guild), this may bottleneck. Consider async fanout in Pattern.

3. **No subagent lifecycle:** Constellation delegates to external MCP servers for complexity. Pattern's "agents within agents" model (forks, workers) might be more powerful but harder to reason about. Constellation's approach (single agent, rich state, tools to delegate) is simpler.

4. **PostgreSQL event log is write-heavy:** Every memory block mutation, tool call, and message creates database rows. At scale, this could be I/O bound. Consider write batching or event sourcing patterns.

5. **Embedding failure is silent:** When embedding fails, blocks get `embedding = null` and are skipped in semantic search. No retry or fallback. Pattern should be more explicit about degradation.

6. **No distributed snapshots:** Constellation's conversation is a single PostgreSQL row that grows indefinitely. Archiving doesn't remove old messages; compaction just summarizes them. For multi-year conversations, this could be problematic. Pattern should consider periodic snapshot + archive rotation.

---

## 10. Key Files and Paths

**Source tree:**
- `src/index.ts` — Composition root and REPL entry point
- `src/agent/agent.ts` — Main agent loop, context building, tool dispatch
- `src/memory/manager.ts` — Memory tier orchestration
- `src/memory/postgres-store.ts` — PostgreSQL adapter
- `src/runtime/executor.ts` — Deno sandbox spawning and IPC bridge
- `src/runtime/deno/runtime.ts` — Deno-side IPC listener and stubs (Deno code, not included in Bun tsconfig)
- `src/model/factory.ts` — LLM provider factory
- `src/model/{anthropic,openai-compat,ollama,openrouter}.ts` — Provider adapters
- `src/embedding/{openai,ollama}.ts` — Embedding provider adapters
- `src/tool/registry.ts` — Tool registration and dispatch
- `src/tool/builtin/` — Memory, code, web, search, compaction, scheduling, email, subconscious tools
- `src/compaction/` — Context compaction (summarization, archival, batching)
- `src/reflexion/` — Prediction journaling, trace recording, introspection
- `src/subconscious/` — Interest registry, curiosity threads, engagement decay
- `src/skill/` — Skill storage, retrieval, semantic search
- `src/extensions/bluesky/` — Bluesky DataSource (Jetstream, AT Protocol)
- `src/extensions/data-source.ts` — DataSource interface
- `src/extensions/data-source-registry.ts` — Multiplexing registry
- `src/mcp/` — MCP protocol client, tool/prompt discovery
- `src/scheduler/` — PostgreSQL cron scheduler
- `src/activity/` — Sleep/wake cycles, event queueing
- `src/search/` — Hybrid search (semantic + keyword + RRF)
- `src/config/` — TOML parsing, Zod schemas, environment override logic

**Configuration:**
- `config.toml.example` — Full config template with all sections
- `persona.md` — Persona seeding (read at startup, stored in core memory)

**Documentation:**
- `docs/implementation-plans/` — Phase-based design docs (e.g., context compaction, MCP client, skills)
- `.claude/CLAUDE.md` — Development guidelines (patterns, conventions, test strategy)

**Database:**
- `src/persistence/migrations/*.sql` — Append-only migration history (never edit existing migrations)

---

## 11. Code Snippets: Load-Bearing Design

### Memory Block Write with Permissions

From `src/memory/manager.ts`:

```typescript
async function write(
  label: string,
  content: string,
  tier: MemoryTier = 'working',
  reason?: string,
): Promise<MemoryWriteResult> {
  const existing = await store.getBlockByLabel(owner, label);

  if (existing) {
    // Check permission
    if (existing.permission === 'readonly') {
      return { applied: false, error: 'block is read-only' };
    }

    if (existing.permission === 'familiar') {
      // Queue a pending mutation
      const mutation = await store.createMutation({
        block_id: existing.id,
        proposed_content: content,
        reason: reason || null,
        status: 'pending',
        feedback: null,
      });
      return { applied: false, mutation };
    }

    // For append or readwrite, update the block
    const newContent =
      existing.permission === 'append'
        ? `${existing.content}\n${content}`
        : content;

    const newEmbedding = await generateEmbedding(newContent);
    const updatedBlock = await store.updateBlock(
      existing.id,
      newContent,
      newEmbedding,
    );

    return { applied: true, block: updatedBlock };
  }
  // ... handle create case
}
```

This shows the permission model in action — three tiers of control (readonly reject, familiar queue, append/readwrite accept).

### Agent Loop Context Compression Check

From `src/agent/agent.ts`:

```typescript
if (deps.compactor && shouldCompress(history, deps.config.context_budget, modelMaxTokens, overheadTokens)) {
  const result = await deps.compactor.compress(history, id);
  history = Array.from(result.history);
}
```

This is called once per message, before the first model call. The decision to compress is based on token budget and model capacity, not just message count.

### Deno Sandbox Permission Flags

From `src/runtime/executor.ts`:

```typescript
const permissionFlags: Array<string> = [];

if (config.unrestricted) {
  permissionFlags.push('--allow-all');
} else {
  // Network permission with allowed hosts
  const allHosts = [...config.allowed_hosts, ...extraHosts];
  if (allHosts.length > 0) {
    permissionFlags.push(`--allow-net=${allHosts.join(',')}`);
  } else {
    permissionFlags.push('--deny-net');
  }

  // Filesystem
  const readPaths = [config.working_dir, ...resolvedReadPaths, ...resolvedWritePaths];
  const writePaths = [config.working_dir, ...resolvedWritePaths];
  permissionFlags.push(`--allow-read=${readPaths.join(',')}`);
  permissionFlags.push(`--allow-write=${writePaths.join(',')}`);

  // Subprocess, environment, FFI
  if (config.allowed_run.length > 0) {
    permissionFlags.push(`--allow-run=${config.allowed_run.join(',')}`);
  } else {
    permissionFlags.push('--deny-run');
  }
  permissionFlags.push('--deny-env');
  permissionFlags.push('--deny-ffi');
}

const proc = Bun.spawn(['deno', 'run', ...permissionFlags, scriptPath], { ... });
```

This demonstrates the fine-grained permission model and dynamic flag generation per execution.

### Tool Registry Dispatch

From `src/agent/agent.ts` (tool dispatch loop):

```typescript
for (const toolUse of toolUseBlocks) {
  let toolResult: string;

  const startTime = Date.now();
  try {
    if (toolUse.name === 'execute_code') {
      // Special case: code execution
      const code = String(toolUse.input['code']);
      const stubs = deps.registry.generateStubs();
      const result = await deps.runtime.execute(code, stubs, context);
      toolResult = result.success ? result.output : `Error: ${result.error}`;
    } else if (toolUse.name === 'compact_context') {
      // Special case: context compaction
      const compactionResult = await deps.compactor.compress(history, id);
      history = Array.from(compactionResult.history);
      toolResult = JSON.stringify({ ... });
    } else {
      // Regular tool dispatch
      const result = await deps.registry.dispatch(toolUse.name, toolUse.input);
      toolResult = result.output;
    }
  } catch (error) {
    toolResult = `Error: ${error instanceof Error ? error.message : 'unknown'}`;
  } finally {
    recordTrace(toolUse.name, toolUse.input, toolResult, Date.now() - startTime, ...);
  }

  // Persist tool result and continue loop
  // ...
}
```

This shows how tools are dispatched, errors are caught, and traces are recorded for introspection.

---

## Summary

Constellation is a **mature, production-ready agent daemon** that synthesizes the best of Phoebe and Pattern into a cohesive TypeScript/Bun implementation. Its key strengths are:

1. **Context compaction** — Sophisticated summarization + archival pipeline
2. **Reflexion** — Prediction journaling for self-monitoring
3. **Subconscious** — Interest registry and curiosity modeling
4. **MCP native support** — First-class plugin ecosystem
5. **Permission-based memory** — Prevent accidental identity corruption
6. **Deno sandbox** — Safe code execution with granular permission flags
7. **DataSource abstraction** — Clean integration pattern for external services

For Pattern's Rust rewrite, the value is in **studying these patterns** rather than forking the code. Constellation's architecture is sound, but Rust offers opportunities for performance, reliability, and deeper integration with Pattern's existing CLI and multi-agent coordination model.

