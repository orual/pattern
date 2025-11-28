# Pattern v2: Memory System Design

## Research Summary

### Loro CRDT Library

Loro is a Rust-native CRDT library with first-class support for:

**Data Types:**
- `LoroText` - Rich text with Fugue algorithm (minimizes interleaving)
- `LoroList` - Ordered list with move support
- `LoroMovableList` - List where items can be moved
- `LoroMap` - Last-write-wins map
- `LoroTree` - Hierarchical tree with move support
- `LoroCounter` - Numeric counter

**Key Features:**
- **Snapshot + Updates model** - Full snapshots for periodic saves, delta updates for frequent saves
- **Time Travel** - `doc.checkout(frontiers)` jumps to any version
- **Version Vectors** - Track what each peer has seen
- **Shallow Snapshots** - Like git shallow clone, archive old history
- **JSONPath queries** - Query document structure
- **Built-in checksums** - Validates imports, rejects corrupted data

**Persistence Pattern:**
```rust
// Periodic: export full snapshot
let snapshot = doc.export(ExportMode::Snapshot);
store_to_db(snapshot);

// Frequent: export delta updates
let updates = doc.export(ExportMode::Updates { from: last_version });
append_to_log(updates);

// Load: import snapshot + all updates
let doc = LoroDoc::new();
doc.import(snapshot);
for update in updates {
    doc.import(update);
}
```

**Detached State Consideration:**
When you `checkout()` to a historical version, the document becomes read-only. Must call `attach()` to return to editing. This is intentional - you're viewing history, not rewriting it.

### Letta Memory Blocks

Letta's memory system uses named blocks with clear semantics:

**Block Structure:**
- `label` - Unique identifier (e.g., "human", "persona", "organization")
- `description` - Crucial for LLM to understand purpose
- `value` - The actual content
- `limit` - Character limit
- `read_only` - Whether agent can modify

**Key Design Principles:**
1. **Description is critical** - The LLM uses the description to understand what to store
2. **Labels are semantic** - "human", "persona", "scratchpad", "organization"
3. **Blocks are shareable** - Multiple agents can attach to the same block
4. **Always in context** - No retrieval needed, blocks are always visible
5. **Agent-managed** - Agents autonomously organize based on labels

**Default Descriptions:**
- `persona`: "Stores details about your current persona, guiding how you behave and respond"
- `human`: "Stores key details about the person you are conversing with"

**Use Cases from Letta:**
- Tool usage guidelines (avoid past mistakes)
- Working memory / scratchpad
- Mirror external state (user's current document)
- Read-only policies shared across agents
- Multi-agent coordination (watch subagent result blocks)
- Emergent behaviour (`performance_tracking`, `emotional_state`)

### sqlite-vec

Vector search extension for SQLite:

**Features:**
- Virtual table: `CREATE VIRTUAL TABLE vec_items USING vec0(embedding float[384])`
- KNN queries with `match` and `order by distance`
- Supports float, int8, and binary vectors
- Metadata columns, partition keys, auxiliary columns
- Pure C, no dependencies, runs anywhere SQLite runs

**Usage:**
```sql
CREATE VIRTUAL TABLE vec_memories USING vec0(
  embedding float[384],
  +memory_id TEXT,      -- metadata column
  +block_label TEXT     -- for filtering
);

-- KNN query
SELECT memory_id, distance
FROM vec_memories
WHERE embedding MATCH ?
  AND block_label = 'archival'
ORDER BY distance
LIMIT 10;
```

---

## Existing v1 Tool Structure (Preserving)

The current tools are well-designed and should be preserved with minimal changes:

### `context` Tool
Manages **Core** and **Working** memory - blocks always/usually in context:
- `append` - Add content to a block
- `replace` - Find and replace content within a block
- `archive` - Move working memory to archival (frees context space)
- `load` - Bring archival memory into working memory
- `swap` - Exchange working and archival blocks

### `recall` Tool
Manages **Archival** memory - long-term storage not in context:
- `insert` - Create new archival entry
- `append` - Add to existing archival entry
- `read` - Retrieve by label
- `delete` - Remove archival entry

### `search` Tool
Unified search across domains:
- `archival_memory` - Search archival blocks
- `conversations` - Search message history
- `constellation_messages` - Search across all agents in constellation
- `all` - Search everything

**Key insight:** The tool interface is solid. What needs to change is the storage backend and the isolation model.

### What to Preserve
- Tool names and basic operations
- Core/Working/Archival distinction
- Search across multiple domains
- Permission system (ACL checks)

### What to Change/Improve
- **Storage backend** - SQLite + Loro instead of SurrealDB + DashMap
- **Ownership model** - Agent-scoped instead of User-scoped
- **Versioning** - Loro gives us history/rollback for free
- **Descriptions** - Add Letta-style descriptions to guide LLM usage
- **Templates** - Structured schemas for common block patterns
- **Rolling logs** - System-maintained logs the agent doesn't manage
- **Archival entries** - Separate table from blocks for fine-grained storage
- **Search** - sqlite-vec for vectors, FTS5 for full-text (replacing SurrealDB's BM25)

---

## v2 Memory Architecture

### Core Principles

1. **Loro documents as memory blocks** - Each memory block is a Loro document
2. **Agent-scoped, not user-scoped** - Memories belong to agents, not users (KEY CHANGE from v1)
3. **Labels with semantic descriptions** - Following Letta's pattern
4. **Versioned by default** - Every change tracked via Loro
5. **No in-memory cache** - SQLite + Loro documents are the source of truth
6. **Preserve tool interface** - Same tools, new backend

### Memory Block Types

```rust
pub enum MemoryBlockType {
    /// Always in context, critical for agent identity
    /// Examples: persona, human, system guidelines
    Core,
    
    /// Working memory, can be swapped in/out based on relevance
    /// Examples: scratchpad, current_task, session_notes
    Working,
    
    /// Long-term storage, NOT in context by default
    /// Retrieved via recall/search tools using semantic search
    /// Examples: past conversations, learned facts, reference material
    Archival,
    
    /// System-maintained logs (read-only to agent)
    /// Recent entries shown in context, older entries searchable
    /// Examples: tool_execution_log, event_log, compression_summaries
    Log,
}
```

### Context Inclusion Model

**Always in context:**
- `Core` blocks - Always included, agent identity depends on these
- `Log` blocks - Recent N entries included (configurable)

**Available via tool call:**
- `Archival` blocks - Agent uses `recall` or `search` tools to retrieve
- `Working` blocks marked as swapped out

**The key insight:** Core memory is the agent's "working memory" that's always visible. Archival memory is "long-term storage" that requires explicit retrieval. This mirrors how MemGPT/Letta works - the agent has limited context but can search its full history.

```
┌─────────────────────────────────────────────────────────┐
│                    Context Window                        │
├─────────────────────────────────────────────────────────┤
│  System Prompt                                          │
├─────────────────────────────────────────────────────────┤
│  Core Memory Blocks (always present)                    │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐       │
│  │   persona   │ │    human    │ │  guidelines │       │
│  └─────────────┘ └─────────────┘ └─────────────┘       │
├─────────────────────────────────────────────────────────┤
│  Working Memory (if active)                             │
│  ┌─────────────┐ ┌─────────────┐                       │
│  │ scratchpad  │ │current_task │                       │
│  └─────────────┘ └─────────────┘                       │
├─────────────────────────────────────────────────────────┤
│  Recent Log Entries (last N)                            │
│  - tool call: read_file("/src/main.rs") → success      │
│  - tool call: search("auth") → 3 results               │
├─────────────────────────────────────────────────────────┤
│  Conversation Messages                                  │
│  (with compression/summarization as needed)             │
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│           Archival Memory (via tool access)             │
│  ┌─────────────────────────────────────────────────┐   │
│  │  Searchable via: recall(query), search(query)   │   │
│  │  - Past conversation summaries                   │   │
│  │  - Learned facts about partner                   │   │
│  │  - Reference documentation                       │   │
│  │  - Old log entries                               │   │
│  └─────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
```

### Block Schema

```rust
pub struct MemoryBlock {
    /// Unique identifier
    pub id: MemoryBlockId,
    
    /// Owning agent (NOT user - this is the key change from v1)
    pub agent_id: AgentId,
    
    /// Semantic label: "persona", "human", "scratchpad", etc.
    pub label: String,
    
    /// Description for the LLM (critical for proper usage)
    pub description: String,
    
    /// Block type determines context inclusion behavior
    pub block_type: MemoryBlockType,
    
    /// Character limit for the block
    pub limit: usize,
    
    /// Whether the agent can modify this block
    pub read_only: bool,
    
    /// The Loro document containing the block content
    /// Stored as snapshot blob in SQLite
    pub document: LoroDoc,
    
    /// Embedding for semantic search (archival blocks)
    pub embedding: Option<Vec<f32>>,
    
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    
    /// Last modified timestamp
    pub updated_at: DateTime<Utc>,
}
```

### Loro Document Structure for Blocks

Each memory block's `LoroDoc` contains:

```rust
// For simple text blocks (persona, human)
doc.get_text("content")

// For structured blocks (could support complex data)
doc.get_map("metadata")
doc.get_text("content")
doc.get_list("entries")  // for log-style blocks
```

### Rolling Logs (System-Maintained)

New concept: blocks that the system appends to, agent observes but doesn't manage:

```rust
pub struct RollingLog {
    /// Uses LoroList under the hood
    entries: LoroList,
    
    /// Max entries before oldest are pruned
    max_entries: usize,
    
    /// Entry schema
    entry_type: LogEntryType,
}

pub enum LogEntryType {
    /// Tool calls and results
    ToolExecution { tool: String, result: String, timestamp: DateTime<Utc> },
    
    /// Significant events
    Event { event_type: String, details: String, timestamp: DateTime<Utc> },
    
    /// Summaries from compression
    ContextSummary { summary: String, messages_summarized: usize, timestamp: DateTime<Utc> },
}
```

The agent sees a read-only view of recent entries in context, but doesn't have to manage the log.

### Templated Memory Blocks

Pre-defined block schemas for common patterns:

```rust
pub enum BlockTemplate {
    /// Agent's personality and behavior
    Persona {
        name: String,
        traits: Vec<String>,
        style: String,
        guidelines: String,
    },
    
    /// Information about the human
    Human {
        name: Option<String>,
        preferences: Vec<String>,
        context: String,
    },
    
    /// Working scratchpad
    Scratchpad {
        current_task: Option<String>,
        notes: String,
    },
    
    /// Shared organizational info (read-only)
    Organization {
        name: String,
        policies: String,
    },
    
    /// Custom free-form
    Custom {
        schema: Option<serde_json::Value>,
    },
}
```

Templates generate the description automatically and can validate content.

### Versioning & Rollback

Thanks to Loro, every memory block has full history:

```rust
impl MemoryBlock {
    /// Get all versions of this block
    pub fn get_history(&self) -> Vec<MemoryVersion> {
        let changes = self.document.getAllChanges();
        // Convert to version summaries
    }
    
    /// View block at a specific version (read-only)
    pub fn checkout(&self, version: Frontiers) -> MemoryBlockView {
        let mut doc = self.document.clone();
        doc.checkout(version);
        MemoryBlockView { doc }
    }
    
    /// Roll back to a previous version (creates new change)
    pub fn rollback_to(&mut self, version: Frontiers) {
        let old_content = self.checkout(version).content();
        self.document.attach(); // Return to head
        // Set content to old value - this creates a new change
        self.document.get_text("content").delete(0, current_len);
        self.document.get_text("content").insert(0, &old_content);
    }
}
```

### Shared Blocks Between Agents

**Key distinction from v1:** Sharing is now *explicit and intentional*, not accidental.

v1 problem: Memories owned by User, so any agent in the constellation could accidentally overwrite another agent's "persona" block if they used the same label.

v2 solution: Memories owned by Agent by default. Sharing requires explicit attachment.

```rust
/// A memory block can be shared with other agents via explicit attachment
pub struct SharedBlockAttachment {
    /// The block being shared (has a primary owner agent)
    pub block_id: MemoryBlockId,
    
    /// Agent gaining access (not the owner)
    pub agent_id: AgentId,
    
    /// What this agent can do with the block
    pub access: SharedAccess,
    
    /// When the attachment was created
    pub attached_at: DateTime<Utc>,
}

pub enum SharedAccess {
    /// Can read but not modify
    ReadOnly,
    /// Can append but not overwrite
    AppendOnly,
    /// Full read/write access
    ReadWrite,
}
```

**How sharing works:**

```rust
// Agent A creates a block (A owns it)
let block = memory_store.create_block(
    &agent_a_id,
    "shared_task_board",
    "Shared task tracking for the constellation",
    MemoryBlockType::Working,
    "## Tasks\n- [ ] Initial task",
).await?;

// Explicitly share with Agent B (read-write)
memory_store.share_block(
    &block.id,
    &agent_b_id,
    SharedAccess::ReadWrite,
).await?;

// Share with Agent C (read-only)
memory_store.share_block(
    &block.id,
    &agent_c_id,
    SharedAccess::ReadOnly,
).await?;
```

**Constellation-level shared blocks:**

For blocks that should be visible to ALL agents in a constellation:

```rust
/// Special owner_id indicating constellation-level ownership
pub const CONSTELLATION_OWNER: &str = "_constellation_";

// Create a constellation-wide block
let org_block = memory_store.create_constellation_block(
    "organization",
    "Read-only information about the organization and its policies",
    MemoryBlockType::Core,
    "Organization: Pattern Project\nPolicies: Be helpful, be honest...",
    SharedAccess::ReadOnly,  // All agents get this access level
).await?;
```

**Use cases:**
- `organization` - Read-only policies shared across all agents
- `partner_profile` - Info about the human, all agents can read, one can write
- `task_board` - Shared task tracking, multiple agents can write
- `handoff_notes` - Agent A writes context for Agent B during handoffs

**What this prevents:**
- Agent A's "persona" accidentally overwriting Agent B's "persona" (different blocks now)
- New agents inheriting memories they shouldn't have
- Confusion about which agent's data is which

---

## SQLite Schema

```sql
-- Memory blocks table
CREATE TABLE memory_blocks (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    label TEXT NOT NULL,
    description TEXT NOT NULL,
    block_type TEXT NOT NULL,  -- 'core', 'working', 'archival', 'log'
    char_limit INTEGER NOT NULL DEFAULT 5000,
    read_only INTEGER NOT NULL DEFAULT 0,
    
    -- Loro document stored as blob
    loro_snapshot BLOB NOT NULL,
    
    -- For quick content access without deserializing Loro
    content_preview TEXT,  -- First N chars
    
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    
    UNIQUE(agent_id, label),
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);

-- Pending updates (between snapshots)
CREATE TABLE memory_block_updates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    block_id TEXT NOT NULL,
    loro_update BLOB NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (block_id) REFERENCES memory_blocks(id)
);

-- Vector search for archival blocks (via sqlite-vec)
CREATE VIRTUAL TABLE memory_embeddings USING vec0(
    embedding float[384],
    +block_id TEXT,
    +content_hash TEXT
);

-- Shared block attachments
CREATE TABLE shared_block_agents (
    block_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    write_access INTEGER NOT NULL DEFAULT 0,
    attached_at TEXT NOT NULL,
    PRIMARY KEY (block_id, agent_id),
    FOREIGN KEY (block_id) REFERENCES memory_blocks(id),
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);

-- Block history metadata (for UI, not full Loro history)
CREATE TABLE memory_block_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    block_id TEXT NOT NULL,
    version_frontiers TEXT NOT NULL,  -- JSON serialized
    change_summary TEXT,
    changed_by TEXT,  -- 'agent', 'user', 'system'
    timestamp TEXT NOT NULL,
    FOREIGN KEY (block_id) REFERENCES memory_blocks(id)
);
```

---

## Memory Operations API

```rust
pub trait MemoryStore {
    /// Create a new memory block for an agent
    async fn create_block(
        &self,
        agent_id: &AgentId,
        label: &str,
        description: &str,
        block_type: MemoryBlockType,
        initial_content: &str,
    ) -> Result<MemoryBlock>;
    
    /// Get a block by agent and label
    async fn get_block(
        &self,
        agent_id: &AgentId,
        label: &str,
    ) -> Result<Option<MemoryBlock>>;
    
    /// Update block content (creates new Loro change)
    async fn update_block_content(
        &self,
        block_id: &MemoryBlockId,
        new_content: &str,
        changed_by: &str,
    ) -> Result<()>;
    
    /// Get all blocks for an agent
    async fn list_agent_blocks(
        &self,
        agent_id: &AgentId,
    ) -> Result<Vec<MemoryBlock>>;
    
    /// Semantic search across archival blocks
    async fn search_archival(
        &self,
        agent_id: &AgentId,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<ArchivalSearchResult>>;
    
    /// Get block history
    async fn get_block_history(
        &self,
        block_id: &MemoryBlockId,
        limit: usize,
    ) -> Result<Vec<MemoryVersion>>;
    
    /// Rollback block to previous version
    async fn rollback_block(
        &self,
        block_id: &MemoryBlockId,
        version: Frontiers,
    ) -> Result<()>;
    
    /// Attach shared block to agent
    async fn attach_shared_block(
        &self,
        block_id: &MemoryBlockId,
        agent_id: &AgentId,
        write_access: bool,
    ) -> Result<()>;
    
    /// Insert content into archival memory
    async fn archival_insert(
        &self,
        agent_id: &AgentId,
        content: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<ArchivalEntryId>;
}

pub struct ArchivalSearchResult {
    pub entry_id: ArchivalEntryId,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
    pub relevance_score: f32,
    pub created_at: DateTime<Utc>,
}
```

---

## Agent Memory Tools

The agent interacts with memory through built-in tools:

### Core Memory Tools

```rust
/// Update a core memory block
/// Only works on Core/Working blocks the agent has write access to
pub struct CoreMemoryUpdate {
    /// Which block to update: "persona", "human", etc.
    pub label: String,
    /// New content (replaces existing)
    pub content: String,
}

/// Append to a core memory block (useful for incremental updates)
pub struct CoreMemoryAppend {
    pub label: String,
    pub content: String,
}
```

### Archival Memory Tools

```rust
/// Search archival memory using semantic similarity
/// Returns relevant entries from long-term storage
pub struct ArchivalSearch {
    /// Natural language query
    pub query: String,
    /// Max results to return
    pub limit: Option<usize>,
}

/// Insert new content into archival memory
/// Use for facts worth remembering long-term
pub struct ArchivalInsert {
    /// Content to store
    pub content: String,
    /// Optional structured metadata
    pub metadata: Option<serde_json::Value>,
}
```

### Recall Tool

```rust
/// Recall searches BOTH conversation history AND archival memory
/// More comprehensive than archival_search alone
pub struct Recall {
    /// Natural language query
    pub query: String,
    /// Max results
    pub limit: Option<usize>,
}
```

### Tool Descriptions for LLM

These descriptions help the agent understand when to use each tool:

```
core_memory_update: Update your core memory blocks (persona, human, etc.). 
Use this to modify persistent information about yourself or the person 
you're talking to. Changes are immediately reflected in your context.

core_memory_append: Add content to an existing core memory block without 
replacing it. Useful for incrementally building up information.

archival_search: Search your long-term archival memory using semantic 
similarity. Use this when you need to recall specific facts, past 
conversations, or reference material that isn't in your current context.

archival_insert: Save important information to your archival memory for 
future reference. Use this for facts worth remembering that don't belong 
in core memory. Good for: learned preferences, important events, 
reference material.

recall: Comprehensive search across your conversation history AND archival 
memory. Use this when you're trying to remember something but aren't sure 
if it was in a conversation or saved to archival.
```

---

## Context Building

When building context for an LLM request:

```rust
pub struct ContextBuilder {
    agent_id: AgentId,
    memory_store: Arc<dyn MemoryStore>,
}

impl ContextBuilder {
    pub async fn build_memory_section(&self) -> Result<String> {
        let blocks = self.memory_store.list_agent_blocks(&self.agent_id).await?;
        
        let mut sections = Vec::new();
        
        for block in blocks {
            // Skip archival blocks (retrieved on demand)
            if block.block_type == MemoryBlockType::Archival {
                continue;
            }
            
            let content = block.document.get_text("content").to_string();
            
            // Format with label and description for the LLM
            sections.push(format!(
                "<{}>\n{}\n\n{}\n</{}>",
                block.label,
                block.description,
                content,
                block.label
            ));
        }
        
        Ok(sections.join("\n\n"))
    }
}
```

---

## Migration from v1

Memory blocks migrate via CAR export:

```rust
// v1 export includes:
// - Raw content
// - Label
// - MemoryType (maps to block_type)
// - Pinned status (maps to Core block_type)
// - Permission (maps to read_only)

// v2 import creates:
// - New Loro document with content
// - Sets agent_id based on import context
// - Generates description from label if missing
```

---

---

## Constellation Resources

Beyond agent-owned memory blocks, Pattern manages shared resources at the constellation level. This is a key differentiator from systems like Letta which focus on single-agent memory.

### Resource Types

```rust
pub enum ConstellationResource {
    /// Shared memory blocks (explicit sharing, has an owner)
    SharedMemory {
        block_id: MemoryBlockId,
        owner: AgentId,
        access_policy: AccessPolicy,
    },
    
    /// Folders of files with automatic embedding
    Folder {
        id: FolderId,
        name: String,
        description: String,
        embedding_config: EmbeddingConfig,
    },
    
    /// Activity stream - system-maintained, read-only to agents
    ActivityStream {
        context_window: usize,    // recent events shown in context
        retention: Duration,       // full history searchable
    },
    
    /// Cross-agent shared context (summaries, notable events)
    SharedContext {
        summaries: Vec<ConversationSummary>,
        summary_interval: Duration,
        max_summaries: usize,
    },
    
    /// Coordination state - explicit shared mutable state
    Coordination {
        state: CoordinationState,
    },
    
    /// External data sources (bluesky, discord, etc.)
    DataSource {
        id: DataSourceId,
        source_type: String,
    },
}
```

### Why Differentiate From Memory Blocks?

v1 presented the activity log as a memory block, which confused agents - it *looked* like something they should edit but wasn't meant to be edited. The same problem applies to shared summaries, activity streams, etc.

Clear differentiation:
- **Memory blocks** - Agent-owned, agent-editable (with permissions)
- **Constellation resources** - System-managed, agents consume/observe

This reduces cognitive load on agents while giving them access to rich shared context.

---

## Folders (File Access)

Inspired by Letta's filesystem, but integrated with Pattern's multi-agent model.

### Folder Structure

```rust
pub struct Folder {
    pub id: FolderId,
    pub name: String,
    pub description: String,  // helps agents understand what's in it
    pub path: FolderPath,     // local filesystem or virtual
    pub embedding_config: EmbeddingConfig,
    pub created_at: DateTime<Utc>,
}

pub enum FolderPath {
    /// Local filesystem path
    Local(PathBuf),
    /// Virtual folder (files stored in DB)
    Virtual,
    /// Remote (future: S3, etc.)
    Remote { url: String, credentials: String },
}

pub struct FolderFile {
    pub id: FileId,
    pub folder_id: FolderId,
    pub name: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub uploaded_at: DateTime<Utc>,
    pub indexed_at: Option<DateTime<Utc>>,
}

pub struct FilePassage {
    pub id: PassageId,
    pub file_id: FileId,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub embedding: Vec<f32>,
}
```

### Folder Attachment

When a folder is attached to an agent, they gain access to file tools:

```rust
pub struct FolderAttachment {
    pub folder_id: FolderId,
    pub agent_id: AgentId,
    pub access: FolderAccess,
    pub attached_at: DateTime<Utc>,
}

pub enum FolderAccess {
    Read,       // can open, grep, search
    ReadWrite,  // can also upload, modify
}
```

Tools automatically available when folders are attached:
- `/open <path>` - Open file, show window in context
- `/read <path> [lines]` - Read specific lines
- `/grep <pattern> [path]` - Regex search
- `/search <folder> <query>` - Semantic search via embeddings

Multiple agents can attach to the same folder with different access levels.

### File Windowing

Large files aren't dumped into context. Instead, a "window" is shown:

```rust
pub struct FileWindow {
    pub file_id: FileId,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub has_more_before: bool,
    pub has_more_after: bool,
}
```

Agent can navigate: `/read file.rs lines 50-100` or `/read file.rs next` to scroll.

### Future: Virtual Shell

For more complex file operations, a sandboxed virtual shell could provide composable commands:

```
/sh ls project_docs/
/sh grep -r "TODO" . | head -20
/sh find . -name "*.rs" | wc -l
```

This is deferred for now but the folder/file infrastructure supports it.

---

## Activity Stream

System-maintained log of constellation activity. Agents observe but don't manage.

### Event Types

```rust
pub struct ActivityEvent {
    pub id: ActivityEventId,
    pub timestamp: DateTime<Utc>,
    pub agent_id: Option<AgentId>,  // None = system event
    pub event_type: ActivityEventType,
    pub details: serde_json::Value,
}

pub enum ActivityEventType {
    // Agent lifecycle
    AgentActivated,
    AgentDeactivated,
    
    // Communication
    MessageReceived { source: String, summary: String },
    MessageSent { target: String, summary: String },
    
    // Tool usage
    ToolExecuted { tool: String, success: bool },
    
    // Memory changes
    MemoryBlockUpdated { label: String, change_type: String },
    
    // File access
    FileAccessed { folder: String, path: String, operation: String },
    
    // Coordination
    TaskAssigned { task: String, assignee: AgentId },
    HandoffInitiated { from: AgentId, to: AgentId },
    
    // Notable events (flagged by agents or system)
    Notable { description: String, importance: Importance },
}
```

### Context Inclusion

Recent activity events are included in agent context:

```
<recent_activity>
[2 hours ago] Flux responded to Bluesky thread about async Rust
[4 hours ago] Entropy broke down task "refactor memory system" into 5 subtasks
[yesterday] Partner updated persona block with new preferences
[yesterday] Anchor flagged Flux's post for tone review
</recent_activity>
```

Older events are searchable via `/search activity "keyword"`.

---

## Shared Context

Cross-agent memory for constellation coherence, especially important for agents activated infrequently.

### Structure

```rust
pub struct SharedContext {
    /// Per-agent activity summaries
    pub agent_summaries: HashMap<AgentId, AgentActivitySummary>,
    
    /// Constellation-wide periodic summaries
    pub constellation_summaries: Vec<ConstellationSummary>,
    
    /// Key events worth long-term remembering
    pub notable_events: Vec<NotableEvent>,
}

pub struct AgentActivitySummary {
    pub agent_id: AgentId,
    pub agent_name: String,
    pub last_active: DateTime<Utc>,
    pub recent_summary: String,  // LLM-generated
    pub generated_at: DateTime<Utc>,
    pub messages_covered: usize,
}

pub struct ConstellationSummary {
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub summary: String,
    pub key_decisions: Vec<String>,
    pub open_threads: Vec<String>,
}

pub struct NotableEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub description: String,
    pub agents_involved: Vec<AgentId>,
    pub importance: Importance,
}

pub enum Importance {
    Low,
    Medium,
    High,
    Critical,
}
```

### Context Building for Returning Agents

When an agent is activated after a period of inactivity:

```rust
impl SharedContextManager {
    pub async fn build_context_for_agent(
        &self, 
        agent_id: &AgentId,
    ) -> Result<Option<String>> {
        let last_active = self.db.get_agent_last_active(agent_id).await?;
        let time_away = Utc::now() - last_active;
        
        // Recently active - minimal or no catch-up needed
        if time_away < Duration::hours(1) {
            return Ok(None);
        }
        
        // Build catch-up context
        let mut context = String::new();
        
        context.push_str(&format!(
            "## Constellation Update (you were last active {})\n\n",
            humanize_duration(time_away)
        ));
        
        // What's happened since
        let events_since = self.db
            .get_activity_events_since(last_active)
            .await?;
        context.push_str(&self.format_events_summary(&events_since));
        
        // Key decisions
        let decisions = self.db
            .get_notable_events_since(last_active, Importance::Medium)
            .await?;
        if !decisions.is_empty() {
            context.push_str("\n### Key Decisions\n");
            for decision in decisions {
                context.push_str(&format!("- {}\n", decision.description));
            }
        }
        
        // Per-agent summaries
        context.push_str("\n### Agent Activity\n");
        let summaries = self.db.get_all_agent_summaries().await?;
        for summary in summaries {
            if summary.agent_id != *agent_id {
                context.push_str(&format!(
                    "**{}** (last active: {})\n{}\n\n",
                    summary.agent_name,
                    humanize_time(summary.last_active),
                    summary.recent_summary
                ));
            }
        }
        
        Ok(Some(context))
    }
}
```

### Example Output

For an agent waking up after a week:

```
## Constellation Update (you were last active 7 days ago)

### What's Happened
- 47 messages processed across the constellation
- Flux handled 12 Bluesky threads
- 3 tasks completed, 2 new tasks created
- Partner had 2 direct conversations with Entropy

### Key Decisions
- Partner decided to pause project-X until next month
- New policy: no engagement with political content on Bluesky
- Memory system redesign approved, work started

### Open Threads
- Partner mentioned wanting to revisit medication adjustments
- Ongoing discussion about v2 architecture

### Agent Activity
**Flux** (last active: 2 hours ago)
Primarily handling Bluesky engagement. Responded to threads about Rust async 
patterns and ADHD coping strategies. Tone was flagged once by Anchor.

**Entropy** (last active: yesterday)
Task breakdown focus. Created detailed subtasks for pattern v2 refactor. 
Helped partner organize project priorities.

**Anchor** (last active: 3 days ago)
Reviewed 8 of Flux's public posts. Flagged 2 for tone adjustment, both resolved.
No escalations to partner needed.
```

### Summary Generation

Summaries are generated by LLM calls on a schedule:

```rust
pub struct SharedContextManager {
    summarizer: Arc<dyn Summarizer>,
    db: DatabaseConnection,
}

impl SharedContextManager {
    /// Called periodically or on agent deactivation
    pub async fn refresh_agent_summary(&self, agent_id: &AgentId) -> Result<()> {
        let since = self.db.get_last_summary_time(agent_id).await?;
        let messages = self.db
            .get_agent_messages_since(agent_id, since)
            .await?;
        
        if messages.len() < 5 {
            return Ok(());  // not enough new activity
        }
        
        let summary = self.summarizer.summarize(
            &messages,
            "Summarize this agent's recent activity in 2-3 sentences. \
             Focus on what they worked on and any notable outcomes."
        ).await?;
        
        self.db.update_agent_summary(agent_id, &summary).await
    }
    
    /// Called on schedule (daily/weekly)
    pub async fn generate_constellation_summary(&self) -> Result<()> {
        let agent_summaries = self.db.get_all_agent_summaries().await?;
        let notable = self.db.get_recent_notable_events(50).await?;
        
        let summary = self.summarizer.summarize_constellation(
            &agent_summaries,
            &notable,
        ).await?;
        
        self.db.insert_constellation_summary(summary).await
    }
}
```

---

## Coordination State

Explicit shared mutable state for multi-agent coordination.

```rust
pub struct CoordinationState {
    /// Task assignments
    pub tasks: HashMap<TaskId, TaskAssignment>,
    
    /// Which agents are currently active
    pub active_agents: HashSet<AgentId>,
    
    /// Handoff notes between agents
    pub handoff_notes: HashMap<AgentId, HandoffNote>,
    
    /// Custom fields (constellation-configurable)
    pub custom: serde_json::Value,
}

pub struct TaskAssignment {
    pub task_id: TaskId,
    pub description: String,
    pub assigned_to: Option<AgentId>,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct HandoffNote {
    pub from_agent: AgentId,
    pub content: String,
    pub created_at: DateTime<Utc>,
}
```

Agents interact via dialect:

```
/coord status                        # view current state
/coord assign "fix bug" to @entropy  # assign task  
/coord complete task-123             # mark done
/coord handoff @anchor "context..."  # leave notes for another agent
/coord note "important observation"  # add to shared notes
```

---

## SQLite Schema Updates

```sql
-- Folders
CREATE TABLE folders (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT,
    path_type TEXT NOT NULL,  -- 'local', 'virtual', 'remote'
    path_value TEXT,          -- filesystem path or URL
    embedding_model TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- Files within folders  
CREATE TABLE folder_files (
    id TEXT PRIMARY KEY,
    folder_id TEXT NOT NULL REFERENCES folders(id),
    name TEXT NOT NULL,
    content_type TEXT,
    size_bytes INTEGER,
    content BLOB,             -- for virtual folders
    uploaded_at TEXT NOT NULL,
    indexed_at TEXT,
    UNIQUE(folder_id, name)
);

-- File passages (chunks with embeddings)
CREATE TABLE file_passages (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES folder_files(id),
    content TEXT NOT NULL,
    start_line INTEGER,
    end_line INTEGER,
    created_at TEXT NOT NULL
);

-- Passage embeddings (sqlite-vec)
CREATE VIRTUAL TABLE file_passage_embeddings USING vec0(
    embedding float[384],
    +passage_id TEXT,
    +file_id TEXT,
    +folder_id TEXT
);

-- Folder attachments to agents
CREATE TABLE folder_attachments (
    folder_id TEXT NOT NULL REFERENCES folders(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    access TEXT NOT NULL,  -- 'read', 'read_write'
    attached_at TEXT NOT NULL,
    PRIMARY KEY (folder_id, agent_id)
);

-- Activity stream
CREATE TABLE activity_events (
    id TEXT PRIMARY KEY,
    timestamp TEXT NOT NULL,
    agent_id TEXT REFERENCES agents(id),
    event_type TEXT NOT NULL,
    details TEXT NOT NULL,  -- JSON
    importance TEXT
);

CREATE INDEX idx_activity_timestamp ON activity_events(timestamp);
CREATE INDEX idx_activity_agent ON activity_events(agent_id);

-- Agent activity summaries
CREATE TABLE agent_summaries (
    agent_id TEXT PRIMARY KEY REFERENCES agents(id),
    summary TEXT NOT NULL,
    messages_covered INTEGER,
    generated_at TEXT NOT NULL,
    last_active TEXT NOT NULL
);

-- Constellation summaries
CREATE TABLE constellation_summaries (
    id TEXT PRIMARY KEY,
    period_start TEXT NOT NULL,
    period_end TEXT NOT NULL,
    summary TEXT NOT NULL,
    key_decisions TEXT,  -- JSON array
    open_threads TEXT,   -- JSON array
    created_at TEXT NOT NULL
);

-- Notable events
CREATE TABLE notable_events (
    id TEXT PRIMARY KEY,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL,
    description TEXT NOT NULL,
    agents_involved TEXT,  -- JSON array of agent IDs
    importance TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- Coordination state (simple key-value for flexibility)
CREATE TABLE coordination_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,  -- JSON
    updated_at TEXT NOT NULL,
    updated_by TEXT       -- agent ID or 'system'
);

-- Task assignments
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    description TEXT NOT NULL,
    assigned_to TEXT REFERENCES agents(id),
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Handoff notes
CREATE TABLE handoff_notes (
    id TEXT PRIMARY KEY,
    from_agent TEXT NOT NULL REFERENCES agents(id),
    to_agent TEXT REFERENCES agents(id),  -- NULL = for anyone
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    read_at TEXT
);
```

---

## Open Questions

1. **Embedding updates** - When block content changes, when do we regenerate embeddings?
   - Option A: On every save (expensive)
   - Option B: Background job (eventual consistency)
   - Option C: On-demand when searching (lazy)

2. **Loro snapshot frequency** - How often to consolidate updates into snapshots?
   - Could be time-based (every N minutes)
   - Or change-count based (every N updates)
   - Or size-based (when updates exceed N bytes)

3. **History retention** - How much Loro history to keep?
   - Shallow snapshots after N days?
   - Full history forever?
   - Configurable per constellation?

4. **Block templates** - Should templates be part of core, or an optional layer?

5. **Diff visibility to agents** - Should agents see what changed between versions?
   - Could add a "recent_changes" read-only block updated by system

6. **Summary generation costs** - LLM calls for summaries add up. How to balance freshness vs cost?
   - Generate on agent deactivation (natural breakpoint)
   - Generate on schedule with batching
   - Skip if activity below threshold

7. **Shared context size** - How much catch-up context is too much?
   - Configurable per agent role?
   - Adaptive based on time away?
