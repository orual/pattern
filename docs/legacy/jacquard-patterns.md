# Jacquard Patterns for Pattern

This document captures key patterns and insights for working with [Jacquard](https://tangled.org/nonbinary.computer/jacquard), the zero-copy AT Protocol library.

## Core Principle: Preserve Types, Convert at Boundaries

Jacquard's validated string types (`Did`, `AtUri`, `Handle`, `Nsid`, `Cid`, etc.) are not just type safety - they're often **cheaper than `String`**. Don't convert to String unless you're at an output boundary.

```rust
// BAD: Loses type info AND allocates
let did_string = did.to_string();
map.insert(did_string, value);

// GOOD: Preserves type AND often cheaper
let did_static = did.clone().into_static();
map.insert(did_static, value);
```

## Why Jacquard Types Are Cheap

All jacquard string types wrap `CowStr<'a>`, which is an enum over:

| Variant | When Used | Cost |
|---------|-----------|------|
| Borrowed slice | Parsing from buffer | Zero allocation |
| SmolStr (inlined) | Strings ≤23 bytes | Stack only, no heap |
| SmolStr (static) | `new_static()` calls | Zero cost, binary reference |
| SmolStr (Arc) | Larger owned strings | Single allocation, cheap clone |

Most DIDs, handles, and rkeys are small enough to inline. Even when they need Arc, cloning is just an atomic increment - no memcpy.

### Across Await Boundaries

```rust
// OLD THINKING: "Need String to cross await points"
let did_string = did.to_string();  // Allocates!
do_async_thing().await;
use_did(&did_string);

// CORRECT: Did<'static> is Send + Sync
let did_static = did.clone().into_static();  // Often just copies inline bytes
do_async_thing().await;
use_did(&did_static);  // Still typed, often cheaper
```

The `'static` lifetime means the data is either:
- Inlined in the struct (no external references)
- A static reference to binary data
- Owned via Arc (reference counted)

All of these are `Send + Sync` and cross await points fine.

## Accessing Untyped Data

`PostView.record` and similar fields are `Data<'a>` (jacquard's replacement for `serde_json::Value`). To access fields:

### Simple Field Access: `get_at_path()`

```rust
// Get a known field by path
let text = post_view.record
    .get_at_path(".text")
    .and_then(|v| v.as_str())
    .unwrap_or("");

// Nested paths work too
let root_uri = post_view.record
    .get_at_path(".reply.root.uri")
    .and_then(|v| v.as_str());
```

### Complex Queries: `query()`

For more complex operations, `Data::query()` provides powerful querying capabilities. See jacquard documentation for full syntax.

### Full Deserialization: `from_data()`

When you need the full typed struct:

```rust
use jacquard::common::from_data;
use jacquard::api::app_bsky::feed::post::Post;

let post: Post<'_> = from_data(&post_view.record)?;
// Now you have full type access
let text = post.text.as_ref();
let reply = post.reply.as_ref();
```

Trade-off: `from_data()` validates the entire structure, `get_at_path()` just navigates to what you need.

## Type Preservation Patterns

### Function Parameters

```rust
// BAD: Loses type info at call site
fn process_author(did: &str) { ... }

// GOOD: Preserves type, enables type-safe operations
fn process_author(did: &Did<'_>) { ... }
```

### Collection Keys

```rust
// BAD: String keys lose type info
let hydrated: DashMap<String, PostView<'static>> = DashMap::new();

// GOOD: Typed keys
let hydrated: DashMap<AtUri<'static>, PostView<'static>> = DashMap::new();
```

### Comparisons

```rust
// BAD: Allocates just to compare
if did.to_string() == other.to_string() { ... }

// GOOD: Compare underlying strings
if did.as_str() == other.as_str() { ... }
```

### Config Structs (The Exception)

Config structs that use `#[derive(Deserialize)]` need `DeserializeOwned`, which requires owned data. Use `String` in configs, then convert at the boundary:

```rust
#[derive(Deserialize)]
struct Config {
    friends: Vec<String>,  // String is fine here
}

// At runtime, compare with .as_str()
if config.friends.iter().any(|f| f == did.as_str()) { ... }
```

## Constructor Patterns

| Method | Allocates? | Use When |
|--------|-----------|----------|
| `Did::new(&str)` | No | Parsing from existing buffer |
| `Did::new_static("...")` | No | Compile-time known strings |
| `Did::new_owned(String)` | Reuses | You already have a String |
| `"...".parse::<Did>()` | **Yes** | Avoid - always allocates |

```rust
// BEST: Zero allocation for known strings
let nsid = Nsid::new_static("app.bsky.feed.post")?;

// GOOD: Borrows from input
let did = Did::new(did_str)?;

// AVOID: FromStr always allocates
let did: Did = did_str.parse()?;
```

## Response Handling

XRPC responses wrap a `Bytes` buffer:

```rust
let response = agent.send(request).await?;

// Option 1: Borrow from buffer (zero-copy, must keep response alive)
let output = response.parse()?;
process(&output);  // OK
drop(response);    // Now output is invalid

// Option 2: Convert to owned (allocates, can outlive response)
let output = response.into_output()?;
drop(response);    // Fine, output is 'static
return output;     // Can return from function
```

## Summary

1. **Preserve jacquard types** through your code - they're optimized for exactly these use cases
2. **Use `.into_static()` when storing** - it's often just copying inline bytes
3. **Use `.as_str()` for comparisons** - no allocation needed
4. **Use `get_at_path()` for simple field access** on `Data<'_>`
5. **Convert to String only at output boundaries** - notification text, API responses, etc.
6. **Config structs are the exception** - use String there, compare with `.as_str()`
