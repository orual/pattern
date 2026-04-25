# Auth Migration: SurrealDB → pattern-auth, atrium → Jacquard

**Date:** 2025-12-26
**Status:** Ready for implementation

## Overview

Migrate auth/identity infrastructure away from SurrealDB and atrium libraries toward SQLite-based pattern-auth and Jacquard. This completes the transition started with the pattern-auth crate creation.

## Scope

### In Scope
1. Discord cleanup & debug command fixes
2. BlueskyEndpoint migration from bsky-sdk to Jacquard
3. CLI auth flow fix (agent→endpoint linking)
4. pattern_core cleanup (remove atrium/bsky-sdk dependencies)

### Out of Scope (Future Tasks)
- Anthropic OAuth migration to pattern-auth
- pattern_server auth rework (ATProto OAuth as identity provider)
- Multi-user Discord OAuth linking

---

## Implementation Plan

### Phase 1: pattern_core Cleanup

**Goal:** Remove dead code and legacy dependencies before building new implementation.

#### 1.1 Delete atproto_identity.rs
- File: `crates/pattern_core/src/atproto_identity.rs`
- Superseded by pattern-auth's Jacquard-based implementation
- Remove from `lib.rs` exports

#### 1.2 Remove Legacy Dependencies from Cargo.toml
Remove from `crates/pattern_core/Cargo.toml`:
- `bsky-sdk`
- `atrium-api`
- `atrium-oauth`
- `atrium-common`
- `atrium-identity`
- `atrium-xrpc`

Keep:
- `jacquard = "0.9"`

#### 1.3 Fix Compilation Errors
- Update any remaining references to removed types
- Ensure `cargo check` passes

---

### Phase 2: BlueskyEndpoint Migration

**Goal:** Replace bsky-sdk with Jacquard, use pattern-auth for session storage.

#### 2.1 Session Type Enum
Create enum wrapper for OAuth vs AppPassword sessions (not dyn-compatible):

```rust
pub enum BlueskySession {
    OAuth(OAuthSession),
    AppPassword(CredentialSession),
}
```

#### 2.2 BlueskyEndpoint Struct Redesign
- Hold Jacquard client instead of bsky_sdk agent
- Clone `AuthDb` (it's a handle, safe to clone)
- Store agent_id for session lookup

#### 2.3 Session Lookup Logic
1. Query pattern_db `agent_atproto_endpoints` for agent-specific session
2. If not found, fall back to `_constellation_` session
3. Load session from auth.db by (did, session_id)
4. Error if neither exists

#### 2.4 HTTP Client Setup
Use proper resolver with DNS and identifiable user agent:
```rust
JacquardResolver::new_dns(pattern_reqwest_client(), ResolverOptions::default())
```

#### 2.5 API Method Migration
Replace bsky-sdk calls with Jacquard equivalents:
- `create_record()` for posting
- `get_posts()` for fetching
- Rich text/facet handling via Jacquard types

---

### Phase 3: CLI Auth Flow Fix

**Goal:** Properly link authenticated sessions to agents.

#### 3.1 Login Flow Enhancement
- Accept optional agent ID parameter (default: `_constellation_`)
- After successful auth, create agent→endpoint mapping
- Store in pattern_db `agent_atproto_endpoints` table

#### 3.2 Agent Endpoint Linking
Table schema (already exists):
- `agent_id` - Agent identifier or `_constellation_`
- `did` - ATProto DID
- `session_id` - Session identifier in auth.db

#### 3.3 Verification
- `pattern-cli atproto status` should show linked agents
- BlueskyEndpoint should successfully load sessions via the new lookup

---

### Phase 4: Discord Cleanup

**Goal:** Remove broken OAuth code, fix disabled debug commands.

#### 4.1 Remove Broken OAuth
- Delete: `crates/pattern_discord/src/oauth.rs`
- Remove `pub mod oauth;` from `crates/pattern_discord/src/lib.rs`

#### 4.2 Remove Unused Identity Type
- Remove/deprecate `discord_identity.rs` from pattern_core
- This was for user→account linking (not needed for bot auth)

#### 4.3 Fix Debug Commands

The Discord slash commands currently use deprecated APIs that no longer exist. This section provides detailed migration guidance.

##### 4.3.1 Threading ConstellationDatabases

The Discord bot needs access to `ConstellationDatabases` for database queries. Current approach uses env vars and agents list only.

**Required Changes to `bot.rs`:**
1. Add `dbs: Arc<ConstellationDatabases>` field to `DiscordBot` struct
2. Update constructors (`new_cli_mode`, `new_full_mode`) to accept `dbs` parameter
3. Pass `dbs` to slash command handlers

**Required Changes to `slash_commands.rs`:**
- Add `dbs: &ConstellationDatabases` parameter to handlers that need database access

##### 4.3.2 API Migration Reference

**Old Agent methods → New API via AgentRuntime:**

| Old API | New API |
|---------|---------|
| `agent.list_memory_keys()` | `agent.runtime().memory().list_blocks(agent.id().as_str())` → returns `Vec<BlockMetadata>` |
| `agent.get_memory(label)` | `agent.runtime().memory().get_block(agent.id().as_str(), label)` → returns `Option<StructuredDocument>` |
| `agent.handle().search_archival_memories(query, limit)` | `agent.runtime().memory().search_archival(agent.id().as_str(), query, limit)` → returns `Vec<ArchivalEntry>` |
| `agent.handle().count_archival_memories()` | Use `search_archival` with high limit, count results (or add count query) |
| `agent.handle().search_conversations(query, ...)` | `agent.runtime().memory().search(/* with appropriate options */, query, limit)` |

**Old SurrealDB ops → New pattern_db queries:**

| Old API | New API |
|---------|---------|
| `ops::list_entities::<AgentRecord, _>(&DB)` | `pattern_db::queries::list_agents(dbs.constellation.pool())` |

**Type Mappings:**

| Old Type | New Type | Location |
|----------|----------|----------|
| `AgentRecord` | `pattern_db::models::Agent` | `pattern_db::models` |
| `DB` (SurrealDB static) | `ConstellationDatabases` (threaded through) | `pattern_core::db` |

##### 4.3.3 Command-by-Command Fix Guide

**`/status` (handle_status_command):**
```rust
// OLD:
if let Ok(memory_blocks) = agent.list_memory_keys().await {
    embed = embed.field("Memory Blocks", memory_blocks.len().to_string(), true);
}

// NEW:
if let Ok(memory_blocks) = agent.runtime().memory().list_blocks(agent.id().as_str()).await {
    embed = embed.field("Memory Blocks", memory_blocks.len().to_string(), true);
}
```

**`/memory` (handle_memory_command):**
```rust
// OLD - list blocks:
match agent.list_memory_keys().await { ... }

// NEW - list blocks:
match agent.runtime().memory().list_blocks(agent.id().as_str()).await {
    Ok(blocks) => {
        // blocks is Vec<BlockMetadata>, extract labels
        let labels: Vec<&str> = blocks.iter().map(|b| b.label.as_str()).collect();
        ...
    }
    Err(e) => { ... }
}

// OLD - get specific block:
match agent.get_memory(block_name).await { ... }

// NEW - get specific block:
match agent.runtime().memory().get_block(agent.id().as_str(), block_name).await {
    Ok(Some(doc)) => {
        // doc is StructuredDocument - access doc.text() or render content
        let content = agent.runtime().memory()
            .get_rendered_content(agent.id().as_str(), block_name)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        ...
    }
    Ok(None) => { /* block not found */ }
    Err(e) => { ... }
}
```

**`/archival` (handle_archival_command):**
```rust
// OLD:
let handle = agent.handle().await;
match handle.search_archival_memories(query, 5).await { ... }

// NEW:
match agent.runtime().memory().search_archival(agent.id().as_str(), query, 5).await {
    Ok(entries) => {
        // entries is Vec<ArchivalEntry> with fields: id, agent_id, content, metadata, created_at
        for entry in entries.iter().take(3) {
            let preview = if entry.content.len() > 200 {
                format!("{}...", &entry.content[..200])
            } else {
                entry.content.clone()
            };
            ...
        }
    }
    Err(e) => { ... }
}
```

**`/context` (handle_context_command):**
```rust
// OLD:
let handle = agent.handle().await;
match handle.search_conversations(None, None, None, None, 100).await { ... }

// NEW:
match agent.runtime().messages().get_recent(100).await {
    Ok(messages) => {
        // messages is Vec<Message> from pattern_core::message
        for msg in messages.iter().rev() {
            let role = format!("{:?}", msg.role);
            let content = msg.display_content();
            ...
        }
    }
    Err(e) => { ... }
}
```

**`/search` (handle_search_command):**
```rust
// OLD:
let handle = agent.handle().await;
match handle.search_conversations(Some(query), None, None, None, 5).await { ... }

// NEW:
// Option 1: Use memory search for text search
match agent.runtime().memory().search(
    agent.id().as_str(),
    query,
    pattern_core::memory::SearchOptions::default().with_limit(5)
).await {
    Ok(results) => {
        // results is Vec<MemorySearchResult>
        ...
    }
    Err(e) => { ... }
}

// Option 2: MessageStore has search_text if needed for conversation-specific search
```

**`/list` (handle_list_command):**
```rust
// OLD:
match ops::list_entities::<AgentRecord, _>(&DB).await { ... }

// NEW (requires dbs parameter):
match pattern_db::queries::list_agents(dbs.constellation.pool()).await {
    Ok(agents) => {
        // agents is Vec<pattern_db::models::Agent>
        let agent_list = agents
            .iter()
            .map(|a| format!("• **{}** - `{}`", a.name, a.id))
            .collect::<Vec<_>>()
            .join("\n");
        ...
    }
    Err(e) => { ... }
}
```

##### 4.3.4 Configuration Migration

**Current:** Uses env vars directly (`DISCORD_ADMIN_USERS`, `DISCORD_CHANNEL_ID`, etc.)

**Target:** Use `pattern_auth::AuthDb` for Discord bot configuration:
```rust
// Load config from auth database
let config = dbs.auth.get_discord_bot_config().await?;

// Or for authorized users check:
fn is_authorized_user(user_id: u64, dbs: &ConstellationDatabases) -> bool {
    // Check against stored admin list in auth.db
    // This allows runtime updates without env var changes
}
```

##### 4.3.5 Files to Modify

1. **`pattern_discord/src/bot.rs`:**
   - Add `dbs: Arc<ConstellationDatabases>` field
   - Update constructors to accept dbs
   - Update `DiscordEventHandler` to pass dbs to handlers

2. **`pattern_discord/src/slash_commands.rs`:**
   - Add `dbs` parameter to handlers needing it
   - Replace all `agent.list_memory_keys()` calls
   - Replace all `agent.get_memory()` calls
   - Replace all `agent.handle()` calls
   - Replace `ops::list_entities` with `pattern_db::queries`
   - Remove undefined imports (`ops`, `AgentRecord`, `DB`)

3. **`pattern_discord/src/lib.rs`:**
   - Ensure proper exports for updated types

#### 4.4 Verify Bot Functionality
- Bot startup loads config from `AuthDb::get_discord_bot_config()`
- All existing bot functionality unchanged
- Debug commands working with pattern-db

---

## Key Decisions

### Session Identifier Convention
- `_constellation_` - Canonical identifier for shared constellation-level identity
- Agent-specific IDs for per-agent identities (future use)

### Session Lookup Priority
1. Agent-specific session (by agent_id)
2. Constellation fallback (`_constellation_`)
3. Error if neither found

### AuthDb Cloning
`AuthDb` is a handle and can be safely cloned. BlueskyEndpoint clones it directly rather than using Arc wrapper.

### HTTP Client
All Jacquard clients use `JacquardResolver::new_dns()` with `pattern_reqwest_client()` for:
- DNS resolution capabilities
- Identifiable user agent
- Consistent HTTP behavior across Pattern

---

## Files Changed

### Deleted
- `crates/pattern_core/src/atproto_identity.rs`
- `crates/pattern_discord/src/oauth.rs`

### Modified
- `crates/pattern_core/Cargo.toml` - Remove atrium/bsky-sdk deps
- `crates/pattern_core/src/lib.rs` - Remove atproto_identity export
- `crates/pattern_core/src/runtime/endpoints/mod.rs` - BlueskyEndpoint rewrite
- `crates/pattern_discord/src/lib.rs` - Remove oauth module
- `crates/pattern_cli/src/commands/atproto.rs` - Add agent linking

### Unchanged
- `crates/pattern_auth/` - Already complete, production ready
- `crates/pattern_core/src/oauth/` - Anthropic OAuth (separate future task)

---

## Testing

1. `cargo check` passes after each phase
2. `pattern-cli atproto login` creates session + agent link
3. `pattern-cli atproto status` shows linked sessions
4. Discord bot starts and responds to commands
5. BlueskyEndpoint can post via both OAuth and AppPassword sessions

---

## Future Work

- Anthropic OAuth → pattern-auth migration
- pattern_server: ATProto OAuth as user identity provider
- Multi-user Discord OAuth linking (when multi-user becomes relevant)
