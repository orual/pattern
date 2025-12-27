# ConstellationSearchTool ToolContext Port Regressions

This document tracks functionality lost during the port of `ConstellationSearchTool` from `AgentHandle`-based implementation to `ToolContext`-based implementation (commit 61a6093).

## Overview

The refactoring simplified the tool's implementation by using the new `ToolContext` trait and unified `SearchOptions`, but several important features were lost in the process. These regressions need to be addressed by either:
1. Extending `SearchOptions` to support the missing parameters
2. Adding new methods to `ToolContext` for advanced search capabilities
3. Re-implementing lost logic in the tool itself

---

## Regression 1: Score Adjustment Logic Lost

### What Was Lost
The old implementation used `process_constellation_results()` from `search_utils.rs` which applied `adjust_message_score()` to downrank certain message types:
- Tool responses: 30% penalty (score × 0.7)
- Messages with reasoning/tool blocks: Up to 50% penalty based on ratio of non-content blocks
- Formula: `score *= 1.0 - (non_content_ratio * 0.5)`

### Impact
- Agents may receive lower-quality search results with too many tool responses or thinking blocks
- Archive agents designed to search across conversations now lack quality filtering
- BM25 scores alone don't account for message content type

### Old Code Location
- `crates/pattern_core/src/tool/builtin/search_utils.rs:7-40` - `adjust_message_score()`
- `crates/pattern_core/src/tool/builtin/search_utils.rs:171-192` - `process_constellation_results()`

### How to Restore
**Option A:** Add score adjustment in the tool itself after receiving results
```rust
// In search_constellation_messages(), after getting results:
let mut scored_messages = results;
for result in &mut scored_messages {
    if let Some(msg) = get_message_from_id(&result.id).await {
        result.score = adjust_message_score(&msg, result.score);
    }
}
scored_messages.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Equal));
```

**Option B:** Extend `MemorySearchResult` to include message metadata
- Add `message_type` field to distinguish tool/reasoning/content
- Apply adjustments in MemoryCache/MemoryStore before returning

**Option C:** Add post-processing filter to SearchOptions
- New field: `apply_content_scoring: bool`
- Let the database layer handle score adjustments

---

## Regression 2: Metadata Lost in Output

### What Was Lost
Old archival search included rich metadata:
```json
{
    "label": "user_preferences",
    "content": "...",
    "created_at": "2024-01-01T00:00:00Z",
    "updated_at": "2024-01-01T00:00:00Z",
    "relevance_score": 0.85
}
```

Old message search included:
```json
{
    "agent": "Archive",
    "id": "msg_123",
    "role": "assistant",
    "content": "...",
    "created_at": "2024-01-01T00:00:00Z",
    "relevance_score": 0.92
}
```

New implementation only returns:
```json
{
    "id": "...",
    "content": "...",
    "relevance_score": 0.85
}
```

### Impact
- Archive agents can't see which archival memory label results came from
- Can't determine which agent said what in constellation searches
- Can't filter or sort by time since timestamps are missing
- Reduced context awareness for intelligent follow-up queries

### How to Restore
**Option A:** Enrich MemorySearchResult type
```rust
pub struct MemorySearchResult {
    pub id: String,
    pub content_type: SearchContentType,
    pub content: Option<String>,
    pub score: f64,
    // NEW FIELDS:
    pub label: Option<String>,        // For blocks/archival
    pub agent_id: Option<String>,     // Which agent owns this
    pub agent_name: Option<String>,   // Display name
    pub role: Option<String>,         // For messages: user/assistant/tool
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}
```

**Option B:** Separate result types per content type
- `BlockSearchResult`, `ArchivalSearchResult`, `MessageSearchResult`
- Each with appropriate metadata
- Tool combines them into final output

**Option C:** Make MemoryStore return full entities
- Change search methods to return full `ArchivalEntry` or message types
- Let tools extract what they need for display

---

## Regression 3: Fuzzy Parameter Ignored

### What Was Lost
Old implementation converted the `fuzzy: bool` parameter to `fuzzy_level: Option<i32>`:
```rust
let fuzzy_level = if fuzzy { Some(1) } else { None };
match self.handle.search_archival_memories_with_options(query, limit, fuzzy_level).await
```

New implementation prefixes with `_fuzzy` and always uses `SearchMode::Fts`:
```rust
async fn search_local_archival(&self, query: &str, limit: usize, _fuzzy: bool)
```

### Impact
- Fuzzy search feature completely disabled
- No typo tolerance in searches
- Parameter accepted but silently ignored (confusing for users)

### How to Restore
**Option A:** Add fuzzy mode to SearchOptions
```rust
pub struct SearchOptions {
    pub mode: SearchMode,
    pub fuzzy_level: Option<i32>,  // NEW: 0=exact, 1=some tolerance, 2=high tolerance
    pub content_types: Vec<SearchContentType>,
    pub limit: usize,
}
```

**Option B:** Extend SearchMode enum
```rust
pub enum SearchMode {
    Fts,
    FtsFuzzy(i32),  // NEW: FTS with fuzzy level
    Vector,
    Hybrid,
    Auto,
}
```

**Option C:** Wait for SurrealDB fuzzy functions
- Document that fuzzy is not yet implemented
- Remove the parameter or make it explicit it's a placeholder
- Add comment referencing issue/plan for implementation

---

## Regression 4: Role and Time Filtering Lost

### What Was Lost
Old implementation parsed and used role/time parameters:
```rust
async fn search_constellation_messages(
    &self,
    query: &str,
    role: Option<ChatRole>,         // Used in database query
    start_time: Option<DateTime<Utc>>, // Used in database query
    end_time: Option<DateTime<Utc>>,   // Used in database query
    limit: usize,
    fuzzy: bool,
)
```

New implementation parses but prefixes with `_` (ignored):
```rust
async fn search_constellation_messages(
    &self,
    query: &str,
    _role: Option<ChatRole>,          // IGNORED
    _start_time: Option<DateTime<Utc>>, // IGNORED
    _end_time: Option<DateTime<Utc>>,   // IGNORED
    limit: usize,
    _fuzzy: bool,
)
```

### Impact
- Can't filter messages by role (user/assistant/tool)
- Can't limit search to time ranges
- Tool accepts parameters but doesn't use them (confusing UX)
- Archive agents lose important filtering capabilities

### Current Status
Has TODO comment at line 379-380:
```rust
// TODO: ToolContext doesn't currently expose role/time filtering for message search
// Need to add these parameters to SearchOptions once message search is fully integrated
```

### How to Restore
**Extend SearchOptions with filter fields:**
```rust
pub struct SearchOptions {
    pub mode: SearchMode,
    pub content_types: Vec<SearchContentType>,
    pub limit: usize,
    // NEW FIELDS:
    pub role_filter: Option<ChatRole>,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
}
```

Then update pattern_db search functions to use these filters in WHERE clauses.

---

## Regression 5: search_all Limit Behavior Changed

### What Was Lost
Old implementation searched each domain with the full limit:
```rust
async fn search_all(&self, query: &str, limit: usize, fuzzy: bool) {
    let archival_result = self.search_local_archival(query, limit, fuzzy).await?;  // Up to `limit` results
    let conv_result = self.search_constellation_messages(query, None, None, None, limit, fuzzy).await?; // Up to `limit` results
    // Could return up to 2×limit total results
}
```

New implementation searches both with a shared limit:
```rust
async fn search_all(&self, query: &str, limit: usize, _fuzzy: bool) {
    let options = SearchOptions {
        mode: SearchMode::Fts,
        content_types: vec![SearchContentType::Archival, SearchContentType::Messages],
        limit, // Single limit shared across both types
    };
    // Returns up to `limit` total results (not per type)
}
```

### Impact
- Archive agents get fewer total results when searching "all"
- If limit=30, old returned up to 60 results (30 archival + 30 messages)
- New returns up to 30 results total (maybe 15 archival + 15 messages)
- Reduced information density for comprehensive searches

### How to Restore
**Option A:** Add `limit_per_type: bool` to SearchOptions
```rust
pub struct SearchOptions {
    pub limit: usize,
    pub limit_per_type: bool, // If true, apply limit to each content type separately
}
```

**Option B:** Separate the searches like before
```rust
async fn search_all(&self, query: &str, limit: usize) {
    let archival_opts = SearchOptions::new().archival_only().limit(limit);
    let msg_opts = SearchOptions::new().messages_only().limit(limit);

    let archival = self.ctx.search(query, Constellation, archival_opts).await?;
    let messages = self.ctx.search(query, Constellation, msg_opts).await?;

    // Combine up to 2×limit results
}
```

**Option C:** Document as intentional change
- New behavior may be better (balanced results)
- Old behavior could overwhelm with too many results
- If keeping new behavior, update documentation/examples

---

## Regression 6: Progressive Truncation Limits Changed

### What Was Lost
Old implementation had different truncation limits for constellation vs local search:

**Constellation search** (group_archival):
```rust
let content = if i < 5 {
    sb.block.value.clone()  // Full content for top 5
} else if i < 15 {
    extract_snippet(&sb.block.value, query, 1500)  // 1500 chars
} else {
    extract_snippet(&sb.block.value, query, 800)   // 800 chars
};
```

**Local search** (local_archival):
```rust
let content = if i < 2 {
    sb.block.value.clone()  // Full content for top 2
} else if i < 5 {
    extract_snippet(&sb.block.value, query, 1000)  // 1000 chars
} else {
    extract_snippet(&sb.block.value, query, 400)   // 400 chars
};
```

New implementation uses the same limits everywhere:
```rust
let content = r.content.as_ref().map(|c| {
    if i < 2 {
        c.clone()
    } else if i < 5 {
        extract_snippet(c, query, 1000)  // Always 1000
    } else {
        extract_snippet(c, query, 400)   // Always 400
    }
});
```

### Impact
- Constellation archival search now shows less content (1000 vs 1500 chars for mid-range results, 400 vs 800 for lower results)
- Archive agents were specifically designed for comprehensive constellation searches with longer snippets
- May reduce effectiveness for Archive agent's primary use case

### How to Restore
**Option A:** Restore constellation-specific limits in search_group_archival
```rust
async fn search_group_archival(&self, ...) {
    // After getting results:
    let formatted: Vec<_> = results.iter().enumerate().map(|(i, r)| {
        let content = r.content.as_ref().map(|c| {
            if i < 5 { c.clone() }
            else if i < 15 { extract_snippet(c, query, 1500) }  // Constellation-specific
            else { extract_snippet(c, query, 800) }
        });
        // ...
    }).collect();
}
```

**Option B:** Make truncation limits configurable
- Add to ConstellationSearchInput or SearchOptions
- Different tools/agents can specify their preferred verbosity

**Option C:** Document as intentional simplification
- Unified behavior is easier to maintain
- If performance is acceptable, keep it simple

---

## Regression 7: search_archival_in_memory() Removed

### What Was Lost
Old implementation had a fallback method for in-memory searching:
```rust
fn search_archival_in_memory(&self, query: &str, limit: usize) -> Result<SearchOutput> {
    let query_lower = query.to_lowercase();
    let mut results: Vec<_> = self.handle.memory
        .get_all_blocks()
        .into_iter()
        .filter(|block| {
            block.memory_type == crate::memory::MemoryType::Archival
                && block.value.to_lowercase().contains(&query_lower)
        })
        .take(limit)
        .map(|block| {
            json!({
                "label": block.label,
                "content": block.value,
                "created_at": block.created_at,
                "updated_at": block.updated_at
            })
        })
        .collect();

    results.sort_by(|a, b| {
        let a_time = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let b_time = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        b_time.cmp(a_time)
    });

    Ok(SearchOutput { ... })
}
```

Called as fallback when database search failed:
```rust
match self.handle.search_archival_memories_with_options(...).await {
    Ok(scored_blocks) => { /* Use DB results */ }
    Err(e) => {
        tracing::warn!("Database search failed, falling back to in-memory: {}", e);
        self.search_archival_in_memory(query, limit)
    }
}
```

### Impact
- No fallback when database search fails
- Tools completely fail instead of degrading gracefully
- In-memory-only agents (tests, demos) can't use search at all
- Reduced resilience

### How to Restore
**Option A:** Add in-memory search to MemoryStore trait
```rust
#[async_trait]
pub trait MemoryStore {
    // ... existing methods ...

    /// Fallback search using only in-memory cache (no database)
    async fn search_in_memory(
        &self,
        agent_id: &str,
        query: &str,
        options: SearchOptions,
    ) -> MemoryResult<Vec<MemorySearchResult>>;
}
```

**Option B:** Make MemoryCache search always work
- MemoryCache.search() should work even if database is unavailable
- Return results from cache only, with a warning log
- Tools automatically get resilience through MemoryStore

**Option C:** Add fallback in the tool itself
```rust
match self.ctx.search(query, scope, options).await {
    Ok(results) => Ok(results),
    Err(e) if e.is_database_error() => {
        tracing::warn!("DB search failed, trying memory cache: {}", e);
        self.search_memory_cache_fallback(query, limit).await
    }
    Err(e) => Err(e),
}
```

---

## Summary Table

| Regression | Severity | Restoration Effort | Recommended Approach |
|------------|----------|-------------------|---------------------|
| Score adjustment logic | High | Medium | Option A: Post-process in tool |
| Metadata lost | High | Medium | Option A: Extend MemorySearchResult |
| Fuzzy parameter ignored | Low | Low | Option C: Document as TODO |
| Role/time filtering | High | Medium | Extend SearchOptions (per TODO) |
| search_all limit changed | Medium | Low | Option B: Separate searches |
| Truncation limits changed | Low | Low | Option A: Restore constellation-specific |
| In-memory fallback removed | Medium | Medium | Option B: Make MemoryCache resilient |

---

## Next Steps

1. **Priority 1 (P1):** Role/time filtering in SearchOptions
   - Already has TODO comment
   - Critical for Archive agent functionality
   - Needed for time-based queries

2. **Priority 1 (P1):** Restore metadata in search results
   - Archive agents need to know which agent/label results came from
   - Timestamps needed for temporal awareness
   - Labels needed for context tool integration

3. **Priority 2 (P2):** Score adjustment logic
   - Quality of search results significantly impacted
   - Can be implemented as post-processing step
   - Doesn't require SearchOptions changes

4. **Priority 2 (P2):** search_all limit behavior
   - Quick fix, restore old behavior for comprehensive searches
   - Important for Archive agent's primary use case

5. **Priority 3 (P3):** In-memory fallback
   - Affects resilience and testing
   - Can be addressed by making MemoryCache more robust

6. **Priority 3 (P3):** Progressive truncation limits
   - Low impact, mostly affects display
   - Easy to restore constellation-specific limits

7. **Priority 4 (P4):** Fuzzy search parameter
   - Already documented as placeholder in code
   - Wait for SurrealDB fuzzy functions

---

## Related Files

- `/home/orual/Projects/PatternProject/pattern/crates/pattern_core/src/tool/builtin/constellation_search.rs` - Current implementation
- `/home/orual/Projects/PatternProject/pattern/crates/pattern_core/src/tool/builtin/search_utils.rs` - Score adjustment logic (still present)
- `/home/orual/Projects/PatternProject/pattern/crates/pattern_core/src/memory/types.rs` - SearchOptions, MemorySearchResult
- `/home/orual/Projects/PatternProject/pattern/crates/pattern_core/src/memory/store.rs` - MemoryStore trait
- `/home/orual/Projects/PatternProject/pattern/crates/pattern_core/src/runtime/tool_context.rs` - ToolContext trait

---

## Git References

- **Port commit:** 61a6093 "refactor(tools): port ConstellationSearchTool to ToolContext"
- **Before port:** `git show 61a6093^:crates/pattern_core/src/tool/builtin/constellation_search.rs`
- **After port:** `git show 61a6093:crates/pattern_core/src/tool/builtin/constellation_search.rs`
