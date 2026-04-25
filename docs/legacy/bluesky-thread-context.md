# Bluesky Thread Context Implementation Plan

## Overview

Port the thread context display from the old atrium-based implementation to jacquard, taking advantage of jacquard's cleaner types to significantly reduce boilerplate.

## Key Insight: Jacquard Makes This Simpler

The old implementation had extensive wrapper types (`BlueskyPost`, `EmbedInfo`, `HydrationState`) because atrium's types were awkward. Jacquard's types are designed to be used directly:

| Old (atrium) | New (jacquard) |
|--------------|----------------|
| `BlueskyPost` wrapper | `PostView<'static>` directly |
| `EmbedInfo` enum | `PostViewEmbed` union directly |
| `HydrationState` tracking | Not needed - PostView comes hydrated |
| Manual `from_post_view()` conversion | Just `.into_static()` when storing |
| `ThreadContext.parent_chain: Vec<(BlueskyPost, Vec<BlueskyPost>)>` | Use `ThreadViewPost` tree directly |

### ThreadViewPost Structure

`GetPostThread` returns `ThreadViewPost` which already has the tree:

```rust
// app_bsky::feed::ThreadViewPost
pub struct ThreadViewPost<'a> {
    pub post: PostView<'a>,                                      // The post with author, embeds, counts
    pub parent: Option<ThreadViewPostParent<'a>>,                // Parent chain (recursive)
    pub replies: Option<Vec<ThreadViewPostRepliesItem<'a>>>,     // Reply tree (recursive)
    pub thread_context: Option<ThreadContext<'a>>,               // Thread metadata
}

// Parent union - each parent can be a full post, not found, or blocked
pub enum ThreadViewPostParent<'a> {
    ThreadViewPost(Box<ThreadViewPost<'a>>),   // Recursive - has its own parent/replies
    NotFoundPost(Box<NotFoundPost<'a>>),       // Post was deleted
    BlockedPost(Box<BlockedPost<'a>>),         // Blocked by author or viewer
}

// Replies union - same variants as parent
pub enum ThreadViewPostRepliesItem<'a> {
    ThreadViewPost(Box<ThreadViewPost<'a>>),   // Each reply is a full thread node
    NotFoundPost(Box<NotFoundPost<'a>>),
    BlockedPost(Box<BlockedPost<'a>>),
}
```

### GetPostThread Request/Response

```rust
// app_bsky::feed::get_post_thread::GetPostThread
pub struct GetPostThread<'a> {
    pub uri: AtUri<'a>,                    // Required - post URI to fetch thread for
    pub depth: Option<i64>,                // Reply depth (default: 6, max: 1000)
    pub parent_height: Option<i64>,        // Parent chain height (default: 80, max: 1000)
}

// Response output
pub struct GetPostThreadOutput<'a> {
    pub thread: GetPostThreadOutputThread<'a>,
    pub threadgate: Option<ThreadgateView<'a>>,  // Reply restrictions if any
}

// Thread can also be not found or blocked at the root
pub enum GetPostThreadOutputThread<'a> {
    ThreadViewPost(Box<ThreadViewPost<'a>>),
    NotFoundPost(Box<NotFoundPost<'a>>),
    BlockedPost(Box<BlockedPost<'a>>),
}
```

## What We Still Need

### 1. ThreadContext (Simplified)

```rust
/// Thread context for display - wraps GetPostThread result with display helpers
pub struct ThreadContext<'a> {
    /// The thread tree from GetPostThread
    pub thread: ThreadViewPost<'a>,

    /// Posts in the current batch that should be highlighted
    pub batch_uris: HashSet<AtUri<'a>>,

    /// Agent's DID for [YOU] markers
    pub agent_did: Option<Did<'a>>,

    /// Whether this thread was recently shown (for abbreviated display)
    pub recently_shown: bool,
}
```

### 2. Display Formatting

Methods on `ThreadContext` to format the tree:

```rust
impl ThreadContext<'_> {
    /// Full thread tree display
    pub fn format_full(&self) -> String { ... }

    /// Abbreviated display (when recently shown)
    pub fn format_abbreviated(&self) -> String { ... }

    /// Format reply options for agent
    pub fn format_reply_options(&self) -> String { ... }
}
```

Display helpers that work directly on jacquard types:

```rust
/// Format a PostView for display at various positions in tree
trait PostDisplay {
    fn format_as_root(&self, agent_did: Option<&Did<'_>>) -> String;
    fn format_as_parent(&self, agent_did: Option<&Did<'_>>, indent: &str) -> String;
    fn format_as_main(&self, agent_did: Option<&Did<'_>>) -> String;
    fn format_as_sibling(&self, agent_did: Option<&Did<'_>>, indent: &str, is_last: bool) -> String;
    fn format_as_reply(&self, agent_did: Option<&Did<'_>>, indent: &str, depth: usize) -> String;
}

impl PostDisplay for PostView<'_> { ... }
```

### 3. Batching Logic

Keep the existing batching by thread root, but enhance:

```rust
struct PendingBatch {
    /// Posts grouped by thread root URI
    posts_by_thread: DashMap<AtUri<'static>, Vec<FirehosePost>>,

    /// Timer for each batch (when first post arrived)
    batch_timers: DashMap<AtUri<'static>, Instant>,

    /// Recently processed URIs (dedup)
    processed_uris: DashMap<AtUri<'static>, Instant>,
}
```

When flushing a batch:
1. Collect all posts for that thread root
2. Find the best "vantage point" (usually 1-2 parents above the most recent post)
3. Call `GetPostThread` on that URI
4. The batch posts should appear naturally in the tree (or be inserted if not)

### 4. Thread Fetching

```rust
impl BlueskyStreamInner {
    /// Fetch thread context for a post
    async fn fetch_thread(
        &self,
        uri: AtUri<'_>,
        depth: usize,
        parent_height: usize,
    ) -> Option<ThreadViewPost<'static>> {
        let agent = self.authenticated_agent.as_ref()?;

        let request = GetPostThread::new()
            .uri(uri)
            .depth(depth as i64)
            .parent_height(parent_height as i64)
            .build();

        let response = match &**agent {
            BlueskyAgent::OAuth(a) => a.send(request).await,
            BlueskyAgent::Credential(a) => a.send(request).await,
        };

        match response {
            Ok(resp) => {
                let output = resp.into_output().ok()?;
                match output.thread {
                    GetPostThreadOutputThread::ThreadViewPost(tvp) => Some(tvp.into_static()),
                    GetPostThreadOutputThread::BlockedPost(_) => None, // Can't see
                    GetPostThreadOutputThread::NotFoundPost(_) => None,
                    _ => None,
                }
            }
            Err(e) => {
                warn!("Failed to fetch thread: {}", e);
                None
            }
        }
    }
}
```

### 5. User Blocks (Current Task)

When we have a `PostView`, create/update user block from `post.author`:

```rust
async fn get_or_create_user_block(&self, author: &ProfileViewBasic<'_>) -> Option<BlockRef> {
    let did = author.did.as_str();
    let handle = author.handle.as_str();
    let display_name = author.display_name.as_ref().map(|s| s.as_ref());
    let avatar = author.avatar.as_ref().map(|s| s.as_ref());

    // ... create/update block logic
}
```

Note: `ProfileViewBasic` doesn't have `description`. For full profiles, we can call `GetProfiles` on unique DIDs from the thread.

### 6. Constellation API (For Siblings)

`GetPostThread` may not give us all siblings at each level. The constellation API at `https://constellation.microcosm.blue/links` can fill gaps:

```rust
/// Fetch posts that reply to the same parent (siblings)
async fn fetch_siblings(&self, parent_uri: &str) -> Vec<AtUri> {
    let url = format!(
        "https://constellation.microcosm.blue/links?target={}&collection=app.bsky.feed.post&path=.reply.parent.uri",
        urlencoding::encode(parent_uri)
    );
    // ... fetch and parse
}
```

This is useful when the thread tree from GetPostThread is truncated.

## Jacquard Patterns to Follow

From the working-with-jacquard skill:

1. **No `.to_string()` in loops** - use `.as_str()` or `.as_ref()`
2. **`.into_static()` only when storing** - e.g., in DashMap or returning from function
3. **Use `AtUri::new()` not `.parse()`** - zero-alloc for borrowed
4. **Match on union variants** - always handle `Unknown` case
5. **Borrow response, own when needed** - `.parse()` borrows, `.into_output()` owns

## Files to Modify

| File | Changes |
|------|---------|
| `data_source/bluesky.rs` | Add ThreadContext, display formatting, thread fetching |
| `data_source/bluesky.rs` | Fix user block creation to use borrowed refs |
| `data_source/bluesky.rs` | Update build_notification_hydrated to build thread context |

## Implementation Order

### Phase 1: Fix Current Code
1. Fix string allocation antipatterns (`.to_string()` → `.as_str()`)
2. Complete user block creation with proper borrowing

### Phase 2: Thread Fetching
3. Add `fetch_thread()` method using `GetPostThread`
4. Handle blocked/not-found/gate cases

### Phase 3: Thread Display
5. Add `ThreadContext` struct (simplified)
6. Implement `PostDisplay` trait for `PostView`
7. Port display formatting from old impl (adapt for jacquard types)

### Phase 4: Integration
8. Update batch flush to fetch thread context
9. Select best vantage point for batch
10. Include batch posts in tree display
11. Add thread caching and "recently shown" tracking

### Phase 5: Constellation (if needed)
12. Add sibling fetching via constellation API
13. Merge siblings into thread tree

## Display Format Examples

### Full Thread Display

```
• Thread context:

    ┌─ @alice.bsky.social - 2h ago: Original post that started the thread
       🔗 at://did:plc:abc/app.bsky.feed.post/123
    │
    ├─ @bob.bsky.social - 1h ago: Reply to alice
       🔗 at://did:plc:def/app.bsky.feed.post/456
    │
    └─ [YOU] @agent.bsky.social - 30m ago: Your earlier reply
       🔗 at://did:plc:xyz/app.bsky.feed.post/789
  │

>>> MAIN POST >>>
@carol.bsky.social - 5m ago: The post that triggered this notification
│ 🔗 at://did:plc:ghi/app.bsky.feed.post/012

   ↳ 2 direct replies:
     └─ @dave.bsky.social - 2m ago: Quick reply
        🔗 at://did:plc:jkl/app.bsky.feed.post/345
```

### Abbreviated (Recently Shown)

```
Thread context trimmed - full context shown recently

ℹ️ You've replied 1 time in this thread (to: @bob.bsky.social)

  └─ @bob.bsky.social - 1h ago: Reply to alice
     🔗 at://did:plc:def/app.bsky.feed.post/456
  │

>>> MAIN POST >>>
@carol.bsky.social - 5m ago: New reply in thread
│ 🔗 at://did:plc:ghi/app.bsky.feed.post/012

  ℹ️ Thread has 3 ancestors, 2 other replies and 1 nested reply (see recent history for full context)
```

## Handling Special Cases

### Blocked Posts
When `GetPostThread` returns `BlockedPost` in parent or replies:
- Show placeholder: `[Blocked by author or viewer]`
- Don't attempt to fetch or display content

### Thread Gates
When thread has reply restrictions:
- Show warning: `⚠️ This thread has reply restrictions - you may not be able to reply`
- Still show context for awareness

### Not Found
When post was deleted:
- Show placeholder: `[Post not found - may have been deleted]`

## Questions/Decisions

1. **Constellation API necessity**: Does `GetPostThread` with reasonable depth/parent_height give us enough? Test first before adding constellation.

2. **Profile descriptions**: Call `GetProfiles` for unique DIDs to get descriptions, or skip for now? (ProfileViewBasic lacks description)

3. **Thread cache TTL**: Old impl used configurable TTL. Keep same pattern?
