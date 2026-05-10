# Pagination primitives for the Pattern SDK

**Last updated:** 2026-05-10

**Status:** design plan, not yet implemented

## What we're solving

Several agent-facing surfaces produce more output than fits comfortably in context: File.read on big files, Memory.get on a kB-scale block, Search.* on broad queries, Web.fetch* on long pages, captured Shell.execute output. Today the agent has to chunk these manually — `T.take`/`T.drop` on the result, or for Web.fetch use the existing offset-based `fetchContinue`.

Two kinds of pagination naturally fall out of this:

1. **Re-runnable cursor pagination** for queries against durable backends (DB, filesystem, in-memory data). Cursor encodes query + offset; handler reruns from offset on continue. Stateless on the handler. Web.fetchContinue is the existing model.

2. **Wrap-any-text pagination** for one-shot Text results that came back too large. Generic chunker takes a Text + chunk size, returns the first chunk + cursor; subsequent calls walk the same text. Avoids retrofitting every text-returning effect into a paginated shape — the agent just pipes a long result through the paginator.

Both worth landing. (2) covers the long tail without API changes; (1) is the right shape for the surfaces that benefit most (File.read, Search).

Relationship to the existing in-preamble `truncVal/truncGo`: those slice a JSON `Value` tree to fit a budget by replacing large subtrees with stub markers. Different layer entirely — they shrink one in-hand response, not paginate across calls. Both useful; complementary.

## Shape: wrap-any-text (the generic one)

New effect `Pattern.Pagination`:

```haskell
data Pagination a where
  PaginateBegin    :: Text -> Int -> Pagination Value
  PaginateContinue :: Text -> Pagination Value
  PaginateRelease  :: Text -> Pagination ()

-- helpers
paginate :: Member Pagination effs => Text -> Int -> Eff effs Value
paginateContinue :: Member Pagination effs => Text -> Eff effs Value
paginateRelease :: Member Pagination effs => Text -> Eff effs ()
```

Returned `Value` is a JSON object:

```json
{
  "content": "<chunk text>",
  "cursor": "<opaque-text-or-null>",
  "offset": 0,
  "total_size": 12345,
  "has_more": true
}
```

`cursor` is null when this is the last page. Null cursor on `PaginateContinue` argues an error.

Agent flow:

```haskell
page0 <- Pagination.paginate longText 8000
-- inspect content; if has_more, save cursor (e.g. into a working block)
-- next turn, or later in same turn:
page1 <- Pagination.paginateContinue cursor0
```

### Handler-side state

Per-`SessionContext` cache:

```rust
struct PaginationCache {
    entries: DashMap<CursorId, Arc<CacheEntry>>,
    // tokio task evicts idle > 10 min
}

struct CacheEntry {
    content: String,           // the full text we're paginating
    chunk_size: usize,
    last_accessed: Instant,
}
```

`PaginateBegin` mints a UUIDv4 cursor id, stores entry, returns first chunk + cursor (encoded as `"<cursor_id>:<offset>"` opaque text). `PaginateContinue` decodes cursor, looks up entry, returns next chunk. Last chunk drops the entry. `PaginateRelease` lets agents free a cache entry early.

Eviction: background tokio task wakes every 60s, drops entries with `last_accessed > 10min ago`. On cache miss in `PaginateContinue`, return `EffectError::Handler("pagination cursor expired or unknown — call PaginateBegin again")`.

Cache lives on `SessionContext`, dies when session does. Ephemeral spawns share the parent's cache (they share `SessionContext.adapter` already; plumbing is similar).

### Chunking

Line-aware byte budget:
1. Walk forward from offset until cumulative bytes ≥ chunk_size.
2. If we land mid-line, back up to the previous newline (don't split lines).
3. If no newline within range (single line longer than budget), fall back to byte-cut.
4. Return `content = text[offset..end]`, advance offset.

Edge cases:
- empty input → `PaginateBegin` returns `{content: "", cursor: null, total_size: 0, has_more: false}` immediately.
- chunk_size ≤ 0 → handler error.
- chunk_size larger than total → single chunk, has_more=false, cursor=null.

## Shape: re-runnable cursor pagination (per-effect)

Each effect that benefits gets a `continueWith` companion taking an opaque cursor:

```haskell
File.read :: Path -> Eff effs (Page Text)
File.continueWith :: Cursor -> Eff effs (Page Text)

Search.messages :: SearchQuery -> Maybe Scope -> Eff effs (Page SearchHit)
Search.continueWith :: Cursor -> Eff effs (Page SearchHit)

Recall.search :: RecallQuery -> Maybe Scope -> Eff effs (Page ArchivalHit)
Recall.continueWith :: Cursor -> Eff effs (Page ArchivalHit)
```

Cursor encodes everything needed to re-run: `base64(json({op: "file_read", path: ..., line_offset: 1234}))`. Handler is stateless — decode, run query from offset, return next page + new cursor.

This is a bigger change because each effect's wire shape grows a Page wrapper or a new continue constructor. Land Pagination (the generic one) first; come back for these.

## Implementation order

1. **Pagination effect** (wrap-any-text). Closes the immediate "long output is awkward" pain point. ~6 file touches: requests/pagination.rs, handlers/pagination.rs, Pattern/Pagination.hs, bundle.rs, effect_classes.rs, session.rs (cache lives on SessionContext).

2. **File.read paginated** (re-runnable). Currently the most awkward shape — agents have to know the offset story. Convert to Page-returning + continueWith. Existing call sites stay simple if Page.content gives the first-page text directly.

3. **Search.* paginated**. Same shape as File.read.

4. **Recall.search paginated**. Same shape.

Phases 2-4 are independent of phase 1 in implementation but agent-facing ergonomics improve when both are available (an agent can `paginate (someEffect ...) chunkSize` to wrap any non-paginated effect's text result).

## Out of scope for this plan

- **Drill-down on truncVal stubs.** The preamble's `truncVal` produces stub markers like `[~512 chars -> stub_3]`. tidepool-mcp has an interactive variant that lets agents call `expandStub n` to fetch the original subtree by id. That requires Message.ask wiring (LLM-call effect) which is a larger lift. Note for later.

- **File.search / Search.text** (ripgrep-as-library effects). Independently useful but separate concern; plan in a follow-up.

## Testing

Per-phase unit tests:
- Pagination handler tests: empty input, single-chunk, multi-chunk, cursor expiry, line-boundary chunking, large single-line fallback.
- File.read continueWith: read a known fixture in chunks of N bytes, verify reconstruction equals full file.
- Search continueWith: SQL query with > N matching messages, verify all retrieved across pages, no duplicates, no gaps.

Integration: live agent test through the `code` tool — open a long file, paginate through it, verify reassembled content matches `cat`.

## Why this shape

Considered a few alternatives:

- **Just truncVal + agent-driven slicing.** Insufficient — truncVal slices in-hand JSON, doesn't help when you want to walk through a long Text. Agents end up doing T.take/T.drop arithmetic which is fragile (especially given the JIT bug filed today) and breaks across turns.

- **Always return Page<a> from every effect.** Forces wire-shape changes everywhere. Breaks existing agent code. Phase 1 (wrap-any-text) gives 80% of the value without retrofit.

- **Per-effect handler-side cache for re-runnable queries.** Considered for File.read/Search where the underlying data is durable. Decided stateless cursor (encoding the query state) wins: no eviction policy to maintain, no cold-cache failure mode, cursor is portable across daemon restarts. Web.fetchContinue's existing pattern is the model.

- **Make agents use Memory blocks as scratch for big results.** Already possible (Memory.put then chunked Memory.get with viewport) but requires the agent to mint blocks just for paging. The generic Pagination effect with handler-side cache is cleaner and self-cleaning.
