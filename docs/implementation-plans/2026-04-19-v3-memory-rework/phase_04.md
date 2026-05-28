# Pattern v3 Memory Rework — Phase 4 Implementation Plan

**Goal:** Make LoroDoc the canonical write target while keeping filesystem and SQLite indexes in sync. Implement per-schema canonical file serialization (Markdown for text, KDL for map/list/composite, JSONL for logs), per-LoroDoc sync workers running as OS threads driven by Loro's `subscribe_root` callbacks, a supervisor that restarts failed workers, and a `notify` watcher that merges external human edits back into Loro via CRDT import.

**Architecture:** Writes apply to a LoroDoc and commit. The commit fires `subscribe_root` callbacks synchronously on the committing thread; each callback pushes an event into its doc's sync worker intake channel. The sync worker is a plain OS thread (mirroring Phase 3's eval worker decision — sync-dominant workload belongs on OS threads, not tokio tasks). It debounces 50ms via `crossbeam-channel::select!` with `after(...)`, drains pending events, borrows a pooled rusqlite connection from Phase 2's `ConstellationDb`, emits the canonical file (md/kdl/jsonl), updates the `memory_blocks_fts` row, and — if the blake3 content hash changed — pushes a re-embed request to an async queue consumed by a tokio task that calls `EmbeddingProvider::embed`. The supervisor is a tokio task on the async side that watches heartbeats from each sync worker via a dedicated channel; if a worker's heartbeat lapses for 30s it signals `tokio_util::sync::CancellationToken` + joins + respawns the thread with a fresh token. External edits are detected by a `notify-debouncer-full` watcher (500ms debounce) that reads the changed file, parses it through the format converter, imports the result into the LoroDoc as a CRDT update, and relies on self-emit-echo suppression (content-hash check against `last_emitted_hash`) to avoid write-notify-rewrite loops.

**Tech Stack:**
- New workspace deps: `kdl` (KDL v2 parser with round-trip fidelity), `crossbeam-channel` (bounded intake + `select!` for debounce multiplexing), `tokio-util` (CancellationToken — already a transitive dep; promoted to explicit), `notify-debouncer-full 0.5` (500ms file-watch debouncer), `metrics 0.23` (counter/gauge observability facade).
- Upgrades: `notify 7.0 → 8.2` in pattern_core (workspace-wide).
- Existing: `loro 1.6`, `blake3` (workspace content-hash convention per `pattern_runtime/CLAUDE.md` — `blake3::hash(..).as_bytes()[..8]` for cross-process stability), `tokio`, `proptest`, `insta`, `tempfile`, `rusqlite` (from Phase 2), `r2d2`/`r2d2_sqlite` (from Phase 2).
- Port-from source: `rewrite-staging/runtime_subsystems/data_source/file_source.rs` (2039 lines) — reference implementation of notify + conflict detection + bidirectional subscriptions. Port + adapt; do not import directly.

**Scope:** Phase 4 of 8.

**Codebase verified:** 2026-04-19 (codebase-investigator agent a8e5e90da84454779).
**External deps verified:** 2026-04-19 (internet-researcher ad0031f6ebd0df4d5).
**Sync-thread-pool crate survey:** 2026-04-19 (internet-researcher a653a40bbbcb9c14e) — confirmed no single crate wraps the "lazy-spawn per-resource-ID worker + bounded intake + debounce + heartbeat supervision" shape; hand-roll with crossbeam-channel + tokio_util::CancellationToken + std::thread is the correct library-first answer.

**Execution posture:** Hybrid — mostly autonomous subagent delegation; main executor sign-off at one checkpoint (after the KDL ↔ LoroValue converter lands, before subscribers are wired) because converter correctness is high-impact and benefits from human spot-checks against representative fixtures.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-memory-rework.AC6: Canonical file serialization round-trips

- **v3-memory-rework.AC6.1 Success:** Text block round-trip: write text, emit `.md`, parse `.md`, import into loro, frontier equals original
- **v3-memory-rework.AC6.2 Success:** Map block round-trip via KDL: write map fields, emit `.kdl`, parse, re-import, loro state equals original (property-tested with proptest)
- **v3-memory-rework.AC6.3 Success:** List block round-trip via KDL: nested lists, ordered correctly, survives round-trip
- **v3-memory-rework.AC6.4 Success:** Log block round-trip via JSONL: entries serialize line-per-entry; parsed back in same order
- **v3-memory-rework.AC6.5 Success:** Composite block round-trip: sections serialize as top-level KDL nodes; section boundaries preserved
- **v3-memory-rework.AC6.6 Failure:** LoroValue containing a type kdl cannot represent (if any are discovered) produces a typed `KdlConversionError`; no silent data loss
- **v3-memory-rework.AC6.7 Edge:** KDL numeric precision: large integers (i128 boundary), floats with special values (#inf, #nan) round-trip exactly per the kdl crate's preservation contract
- **v3-memory-rework.AC6.8 Edge:** Strings with embedded newlines, quotes, and unicode round-trip correctly through KDL

### v3-memory-rework.AC7: Loro-native subscribers + external edit merge

- **v3-memory-rework.AC7.1 Success:** Write a block, observe emitted file matching block content within 100ms (50ms debounce + overhead)
- **v3-memory-rework.AC7.2 Success:** Subscriber emits FTS5 row update matching block content
- **v3-memory-rework.AC7.3 Success:** Subscriber queues vector re-embed only when content hash changes; no spurious re-embeds
- **v3-memory-rework.AC7.4 Success:** External edit to `.md` via text editor: notify detects, loro merges, re-emission produces canonical content
- **v3-memory-rework.AC7.5 Success:** Self-emit-echo suppression: write block → observe single emission (not an infinite loop)
- **v3-memory-rework.AC7.6 Failure:** Invalid KDL from human edit: parse fails, `metrics::counter!("memory.kdl.parse_failed")` increments, no loro merge attempted, prior valid content re-emitted
- **v3-memory-rework.AC7.7 Failure:** Subscriber panic: supervisor detects heartbeat timeout within 30s, logs ERROR, restarts worker, increments restart counter
- **v3-memory-rework.AC7.8 Edge:** Concurrent human edit + agent write: loro CRDT merges both; final state reflects both changes

---

## Codebase verification findings

Key realities that shape the task breakdown:

- ✓ `StructuredDocument` at `pattern_core/src/memory/document.rs` (stayed in pattern_core during Phase 1 — it appears in `MemoryStore` trait signatures, moving it would create a circular dep; `pattern_memory` re-exports it for convenience) wraps `LoroDoc` as `doc: LoroDoc` (private field). `subscribe_root` is implemented but not actively used; Phase 4 wires it. Current commit callsites: mutations call `doc.commit()` eagerly — fine for Phase 4, since subscriber debouncing coalesces downstream.
- ✓ `rewrite-staging/runtime_subsystems/data_source/file_source.rs` exists, 2039 lines, zero sqlx deps, uses `notify::{RecommendedWatcher, RecursiveMode, Watcher}`, `tokio::sync::{Mutex, broadcast}`, `loro::{LoroDoc, Subscription, VersionVector}`. **Not compiled** (rewrite-staging/ not a workspace member) — port-and-adapt, don't import. Note: the staging file used `sha2`; Phase 4 diverges from staging here and uses `blake3` to match the workspace content-hash convention (see next bullet).
- ✓ Content hashing uses **blake3**, not sha2. `pattern_runtime/CLAUDE.md` documents the existing convention: `blake3::hash(...).as_bytes()[..8]` for content_hash values (cross-process stable; used for compose-pipeline delta detection). Phase 4 matches this — project-wide consistency beats the micro-benchmark argument for sha2 on small inputs. Subscribers and the watcher both use blake3 for content_hash + self-emit-echo suppression.
- ✓ `notify 7.0` in `crates/pattern_core/Cargo.toml`; Phase 4 bumps to `8.2` (minor version upgrade; API differences are minor per upstream changelog). Adds `notify-debouncer-full 0.5`.
- ✓ `kdl`, `metrics`, `crossbeam-channel`, `insta` are all new workspace deps (`insta` was first added in Phase 2 for pattern_db; Phase 4 re-uses).
- ✓ `proptest 1` already a dev-dep in `pattern_core/Cargo.toml`; Phase 4 adds it to `pattern_memory/Cargo.toml` for round-trip property tests.
- ✓ `tempfile 3` already a dev-dep workspace-wide; Phase 4's fs fixture tests inherit this.
- ✓ `MemoryCache::evict(agent_id, label)` exists today. Design calls this `drop_doc`. Phase 4 **renames `evict` to `drop_doc`** for clarity (subscribers' lifecycle is tied to this method — naming should reflect the role). Call sites: update everywhere `evict` is called.
- ✓ Re-embedding is currently inline (called directly from handlers where relevant); there's no existing async queue. Phase 4 introduces `ReembedQueue` as a new tokio-task-backed queue.
- ✓ Loro `subscribe_root` callback fires synchronously on the thread that called `commit()`, `import()`, or `checkout()` — confirmed. Idempotent import is confirmed. No `LoroValue::Counter` (Counter semantics live via List/Map mutation ops, not as a LoroValue variant) — adjust the KDL converter accordingly (no Counter variant to convert; just List/Map of scalars).
- ✓ `LoroValue::Binary` usage per Phase 1 audit: one match-skip site in `document.rs:1185` (`LoroValue::Binary(_) => return None`). Zero construction sites in block content. Phase 4's KDL converter treats `LoroValue::Binary` as an error-path variant and emits `KdlConversionError::UnsupportedBinary` rather than silently base64-encoding — prevents introducing binary-in-block usage accidentally. The existing match-skip site either (a) remains as-is (it's already defensive) or (b) is replaced with a call to the converter's error path for consistency — implementor's call based on context.
- ✓ FTS5 row-update queries live in `pattern_db/src/queries/memory.rs` (59 queries) — post-Phase-2. Subscribers call the sync `upsert_memory_block_fts(...)` function (post-Phase-2 naming; verify at implementation time).
- ✗ No "DB worker" crate-ecosystem winner — hand-roll the supervisor with stdlib threads + crossbeam-channel + tokio_util::CancellationToken per the sync-thread-pool survey.
- ✓ `pattern_memory/CLAUDE.md` (created in Phase 1, minimal stub) gets freshened during this phase with the subscriber supervisor architecture + watcher behavior.

---

## Dependency changes

`crates/pattern_memory/Cargo.toml`:

```toml
[dependencies]
# ... existing from Phase 1 ...

# Phase 4 additions:
kdl = "6"                        # KDL v2 parser, preserves formatting
crossbeam-channel = "0.5"        # bounded intake + select! multiplex
tokio-util = { version = "0.7", features = ["rt"] }  # CancellationToken
metrics = "0.23"                 # observability counter/gauge facade
notify = "8.2"                   # file-system watcher
notify-debouncer-full = "0.5"    # 500ms debouncer
blake3 = "1"                     # content hashing — matches workspace convention (pattern_runtime CLAUDE.md)

[dev-dependencies]
# ... existing ...
proptest = "1"                   # round-trip property tests
insta = { version = "1", features = ["yaml"] }
tempfile = "3"
```

`crates/pattern_core/Cargo.toml`: bump `notify = "7"` → `notify = "8.2"` (if pattern_core still uses notify directly — if only pattern_memory needs it post-refactor, remove from pattern_core entirely).

---

## Implementation tasks

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

### Subcomponent A: Canonical file serialization — format modules

<!-- START_TASK_1 -->
### Task 1: `fs/markdown.rs` — Text block ↔ `.md`

**Verifies:** v3-memory-rework.AC6.1, AC6.8 (partial)

**Files:**
- Create: `crates/pattern_memory/src/fs/mod.rs` (re-exports)
- Create: `crates/pattern_memory/src/fs/markdown.rs`
- Create: `crates/pattern_memory/src/fs/error.rs` (shared `FsError` type)

**Implementation:**

1. `fs/error.rs`:

   ```rust
   #[derive(Debug, thiserror::Error)]
   #[non_exhaustive]
   pub enum FsError {
       #[error("io error reading/writing block file at {path}: {source}")]
       Io { path: PathBuf, #[source] source: std::io::Error },

       #[error("invalid file format for {path}: {reason}")]
       ParseError { path: PathBuf, reason: String },

       #[error(transparent)]
       KdlConversion(#[from] KdlConversionError),

       #[error(transparent)]
       JsonLine(#[from] serde_json::Error),

       #[error("UTF-8 error reading {path}: {source}")]
       Utf8 { path: PathBuf, #[source] source: std::string::FromUtf8Error },
   }
   ```

2. `fs/markdown.rs`:

   ```rust
   pub fn text_to_markdown(text: &str) -> String {
       // Plain passthrough — Pattern text blocks are raw strings, not rendered markdown.
       // The .md extension is a social signal (opens in editors with markdown mode) but
       // the file content is whatever the agent wrote. No escaping, no normalization.
       text.to_owned()
   }

   pub fn markdown_to_text(content: &str) -> String {
       // Reverse passthrough. Normalize nothing; preserve embedded newlines + trailing
       // whitespace + everything. Loro's text merge is responsible for reconciling any
       // diffs; we just pipe bytes through.
       content.to_owned()
   }

   pub fn read_markdown_file(path: &Path) -> Result<String, FsError> {
       std::fs::read_to_string(path).map_err(|e| FsError::Io { path: path.to_owned(), source: e })
   }

   pub fn write_markdown_file(path: &Path, text: &str) -> Result<(), FsError> {
       std::fs::write(path, text).map_err(|e| FsError::Io { path: path.to_owned(), source: e })
   }
   ```

3. Add atomic-write helper in `fs/mod.rs` for all three formats:

   ```rust
   /// Write `content` to `path` atomically: write to `path.tmp`, fsync, rename over `path`.
   /// Prevents partial writes visible to the notify watcher or human editors.
   pub fn atomic_write(path: &Path, content: &[u8]) -> Result<(), FsError> {
       let tmp = path.with_extension(format!(
           "{}.tmp",
           path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
       ));
       {
           let mut f = std::fs::File::create(&tmp)
               .map_err(|e| FsError::Io { path: tmp.clone(), source: e })?;
           f.write_all(content).map_err(|e| FsError::Io { path: tmp.clone(), source: e })?;
           f.sync_all().map_err(|e| FsError::Io { path: tmp.clone(), source: e })?;
       }
       std::fs::rename(&tmp, path).map_err(|e| FsError::Io { path: path.to_owned(), source: e })
   }
   ```

   Use from all three format modules.

**Testing:**

Inline unit tests + property tests (proptest) for round-trip equivalence:
- `text → md → text` is identity under the passthrough definition.
- Atomic-write test: target file content is either pre-write or post-write, never partial (hard to test directly; instead verify the temp file is cleaned up and final content matches).
- UTF-8 edge cases: multi-byte codepoints, combining characters, BOM handling (strip or preserve? Decision: preserve).

**Verification:**

Run: `cargo nextest run -p pattern_memory --lib fs::markdown`
Expected: all pass.

**Commit:** `[pattern-memory] add fs/markdown.rs + fs/error.rs + atomic_write helper`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `fs/kdl.rs` — `LoroValue` ↔ `KdlDocument` (Map / List / Composite blocks)

**Verifies:** v3-memory-rework.AC6.2, AC6.3, AC6.5, AC6.6, AC6.7, AC6.8

**Files:**
- Create: `crates/pattern_memory/src/fs/kdl.rs`

**Implementation:**

1. `KdlConversionError` types:

   ```rust
   #[derive(Debug, thiserror::Error)]
   #[non_exhaustive]
   pub enum KdlConversionError {
       #[error("unsupported LoroValue variant: {0}")]
       UnsupportedVariant(String),

       #[error("unsupported Binary LoroValue — blocks must not contain raw bytes")]
       UnsupportedBinary,

       #[error("KDL parse error: {0}")]
       ParseError(String),

       #[error("KDL round-trip fidelity violation: {reason}")]
       RoundtripViolation { reason: String },
   }
   ```

2. Forward converter `LoroValue → KdlDocument`:

   **KDL-native shape conventions** (no sentinel nodes, no fake `item{i}` names):

   - **Map** → KDL's natural shape. Each key becomes a named node. Scalar values go as a single argument on the node (`foo "bar"`). Nested Map / List values go as children inside `{ ... }`.
   - **List** → repeated-name children pattern. Each list item is a node with the reserved name `"-"` (quoted identifier). Scalar items carry their value as a single argument (`- "first"`). Complex items use children. Position is preserved by document order.
   - **Disambiguation**: the block's `BlockSchema` (already stored in memory.db per Phase 2) tells the parser whether to interpret the document as Map or List. No in-file sentinel needed — the existing block metadata is authoritative.

   Example Map block (`.kdl`):
   ```kdl
   persona "@reviewer"
   tags "urgent" "wip"
   nested {
       foo "bar"
       count 42
   }
   ```

   Example List block:
   ```kdl
   - "first"
   - "second"
   - { name "complex"; value 42 }
   ```

   ```rust
   /// Serialize a LoroValue to KDL. The caller supplies a top-level shape hint
   /// (matching the block's BlockSchema) so the output format matches.
   pub fn loro_value_to_kdl(value: &LoroValue, shape: TopShape) -> Result<KdlDocument, KdlConversionError> {
       let mut doc = KdlDocument::new();
       match (shape, value) {
           (TopShape::Map, LoroValue::Map(m)) => {
               for (k, v) in m.iter() {
                   doc.nodes_mut().push(loro_value_to_kdl_node(k, v)?);
               }
           }
           (TopShape::List, LoroValue::List(l)) => {
               for v in l.iter() {
                   // "-" is a quoted-identifier node name reserved for list items.
                   doc.nodes_mut().push(loro_value_to_kdl_node("-", v)?);
               }
           }
           (TopShape::Map, other) | (TopShape::List, other) => {
               // Mismatch between declared schema and actual value shape — surface loud.
               return Err(KdlConversionError::ShapeMismatch {
                   expected: shape,
                   actual: format!("{other:?}"),
               });
           }
       }
       Ok(doc)
   }

   #[derive(Debug, Clone, Copy)]
   pub enum TopShape {
       Map,
       List,
       // Composite is structurally a Map at top level (sections are named nodes
       // with children); handled via the Map variant.
   }

   fn loro_value_to_kdl_node(name: &str, value: &LoroValue) -> Result<KdlNode, KdlConversionError> {
       let mut node = KdlNode::new(name);
       match value {
           LoroValue::Null => { node.push(KdlEntry::new(KdlValue::Null)); }
           LoroValue::Bool(b) => { node.push(KdlEntry::new(*b)); }
           LoroValue::Double(d) => {
               // KDL preserves f64 including ±inf and NaN per v2 spec.
               node.push(KdlEntry::new(*d));
           }
           LoroValue::I64(i) => { node.push(KdlEntry::new(*i)); }
           LoroValue::String(s) => { node.push(KdlEntry::new(s.as_str())); }
           LoroValue::List(l) => {
               // Nested list: if all items are scalar AND none contains a newline,
               // collapse into positional arguments on this node (KDL-native;
               // kdl crate handles multi-line formatting via `\` continuation
               // automatically if the line gets long). If any item is non-scalar
               // (Map, nested List) OR any scalar string contains a newline,
               // fall back to repeated-name children with the "-" sentinel.
               let all_scalar_single_line = l.iter().all(|v| match v {
                   LoroValue::Null | LoroValue::Bool(_)
                   | LoroValue::Double(_) | LoroValue::I64(_) => true,
                   LoroValue::String(s) => !s.contains('\n'),
                   _ => false,
               });
               if all_scalar_single_line {
                   for v in l.iter() {
                       node.push(scalar_loro_to_kdl_entry(v)?);
                   }
                   // KDL crate's formatter handles line continuation (`\`) for
                   // long arg lists. No explicit breaking logic needed.
               } else {
                   let mut children = KdlDocument::new();
                   for v in l.iter() {
                       children.nodes_mut().push(loro_value_to_kdl_node("-", v)?);
                   }
                   node.set_children(Some(children));
               }
           }
           LoroValue::Map(m) => {
               let mut children = KdlDocument::new();
               for (k, v) in m.iter() {
                   children.nodes_mut().push(loro_value_to_kdl_node(k, v)?);
               }
               node.set_children(Some(children));
           }
           LoroValue::Binary(_) => return Err(KdlConversionError::UnsupportedBinary),
           LoroValue::Container(cid) => {
               // Container type: emit as typed annotation carrying the ContainerID string.
               // cid.to_string() format: `🦜:cid:...`. Stable across loro versions.
               let mut entry = KdlEntry::new(cid.to_string());
               entry.set_ty("container");
               node.push(entry);
           }
       }
       Ok(node)
   }

   /// Extract a scalar LoroValue into a KdlEntry (for positional arguments).
   /// Panics/errors on non-scalar variants — caller must check `all_scalar` first.
   fn scalar_loro_to_kdl_entry(value: &LoroValue) -> Result<KdlEntry, KdlConversionError> {
       match value {
           LoroValue::Null => Ok(KdlEntry::new(KdlValue::Null)),
           LoroValue::Bool(b) => Ok(KdlEntry::new(*b)),
           LoroValue::Double(d) => Ok(KdlEntry::new(*d)),
           LoroValue::I64(i) => Ok(KdlEntry::new(*i)),
           LoroValue::String(s) => Ok(KdlEntry::new(s.as_str())),
           other => Err(KdlConversionError::UnsupportedVariant(format!(
               "scalar-only context, got {other:?}"
           ))),
       }
   }
   ```

3. Reverse converter `KdlDocument → LoroValue` — schema-directed:

   ```rust
   /// Deserialize a KdlDocument into a LoroValue using the block's declared shape.
   /// The caller consults the block's BlockSchema (from memory.db metadata) and
   /// passes the matching TopShape. This makes the Map/List distinction
   /// unambiguous — no in-file sentinel needed.
   ///
   /// Returns a typed error when the KDL shape doesn't match the declared schema
   /// (e.g. List schema but the file contains arbitrary-named nodes). These
   /// surface as "invalid external edit" via the watcher; the subscriber
   /// re-emits the canonical form (see AC7.6).
   pub fn kdl_to_loro_value(doc: &KdlDocument, shape: TopShape) -> Result<LoroValue, KdlConversionError> {
       let nodes = doc.nodes();
       match shape {
           TopShape::Map => {
               let mut out = HashMap::new();
               for n in nodes {
                   let key = n.name().value().to_owned();
                   if key == "-" {
                       return Err(KdlConversionError::ShapeMismatch {
                           expected: TopShape::Map,
                           actual: "document contains list-item sentinel `-` but schema is Map".into(),
                       });
                   }
                   if out.contains_key(&key) {
                       return Err(KdlConversionError::DuplicateKey { key });
                   }
                   out.insert(key, kdl_node_to_loro_value(n)?);
               }
               Ok(LoroValue::Map(out.into()))
           }
           TopShape::List => {
               let mut out = Vec::with_capacity(nodes.len());
               for n in nodes {
                   if n.name().value() != "-" {
                       return Err(KdlConversionError::ShapeMismatch {
                           expected: TopShape::List,
                           actual: format!(
                               "list schema requires all top-level nodes named `-`; found `{}`",
                               n.name().value()
                           ),
                       });
                   }
                   out.push(kdl_node_to_loro_value(n)?);
               }
               Ok(LoroValue::List(out.into()))
           }
       }
   }
   // kdl_node_to_loro_value is the inverse of loro_value_to_kdl_node. Shape
   // decision rules per node:
   //   1. Node has >1 positional arg, no children → LoroValue::List of scalars
   //      (arg form, e.g. `tags "a" "b" "c"`).
   //   2. Node has 1 positional arg, no children → scalar LoroValue
   //      (the arg's value, e.g. `name "foo"` → LoroValue::String("foo")).
   //   3. Node has 0 args, children all named "-" → LoroValue::List of the
   //      child values (child form, e.g. `tags { - "a"; - "b"; }`).
   //   4. Node has 0 args, children with distinct names → LoroValue::Map
   //      of {child_name: recurse_value} (nested Map).
   //   5. Node has 0 args, 0 children → LoroValue::Null (entry-less node).
   //   6. Node has >1 arg AND children → error (ambiguous; KDL allows this
   //      but the converter doesn't assign a single LoroValue shape to it).
   // See tests/kdl_roundtrip_proptest.rs for exhaustive shape coverage.
   ```

   **Edge cases covered by the schema-directed approach:**
   - Empty Map vs empty List: empty KDL document parses as whichever shape the caller requests; no ambiguity.
   - Map with keys that look like list markers (e.g., a user genuinely wants a key named `-` in a Map): rejected with `ShapeMismatch` error — `-` is reserved as the list-item sentinel. Users who truly need that key can use a different name, or we could extend to allow escaped keys if requested — out of scope for this phase.
   - Map with duplicate keys (from bad KDL authoring): rejected with `DuplicateKey` — Map semantics require unique keys.
   - List block edited by a human to have arbitrary node names: rejected with `ShapeMismatch` pointing the user to the `-` convention.
   - Composite schema: handled via Map shape at top level (composite sections are named nodes), with each section's children parsed recursively.

   **Round-trip property tests (proptest):** generate arbitrary LoroValues with random Map/List structure + string/numeric content; emit via `loro_value_to_kdl`; re-parse via `kdl_to_loro_value` with the same shape; assert equal. Generator strategies avoid producing Maps with literal `-` keys (not supported per the reserved-name rule) and avoid keys with KDL-problematic characters.

4. Composite blocks: sections are top-level KdlNode entries with their own child blocks. `CompositeSection` type (from `pattern_core::types::memory_types::schema`) maps 1:1 to a top-level node with a name + children. Round-trip should preserve section order.

**Testing:**

- Unit tests for each `LoroValue` variant in isolation.
- `proptest` round-trip tests: generate arbitrary `LoroValue` (recursive strategies), convert to KDL, parse back, assert equal. Generator strategies in a shared `tests/loro_strategies.rs`.
- Edge-case tests (AC6.7, AC6.8): i64 boundary values, f64 infinity/NaN, strings with newlines/quotes/unicode/backslashes.
- AC6.6 failure test: attempt to convert a `LoroValue::Binary(vec![0xDE, 0xAD])`; assert `Err(KdlConversionError::UnsupportedBinary)`.

**Verification:**

Run: `cargo nextest run -p pattern_memory --lib fs::kdl`
Expected: all pass.

Run: `cargo nextest run -p pattern_memory --test kdl_roundtrip_proptest`
Expected: property tests pass across ≥1000 generated inputs.

**Commit:** `[pattern-memory] add fs/kdl.rs LoroValue↔KdlDocument converter with round-trip property tests`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `fs/jsonl.rs` — Log block ↔ `.jsonl`

**Verifies:** v3-memory-rework.AC6.4

**Files:**
- Create: `crates/pattern_memory/src/fs/jsonl.rs`

**Implementation:**

1. Log blocks have `BlockSchema::Log { display_limit, entry_schema }`. Each log entry is a JSON value; the `.jsonl` file is one JSON value per line (newline-delimited).

2. Converter:

   ```rust
   /// Serialize a list of log entries to JSONL bytes.
   pub fn log_entries_to_jsonl(entries: &[serde_json::Value]) -> Result<Vec<u8>, FsError> {
       let mut out = Vec::new();
       for entry in entries {
           serde_json::to_writer(&mut out, entry)?;
           out.push(b'\n');
       }
       Ok(out)
   }

   /// Parse JSONL bytes into a list of log entries. Skips blank lines. Errors
   /// loudly on malformed JSON — do not silently drop entries.
   pub fn jsonl_to_log_entries(content: &str) -> Result<Vec<serde_json::Value>, FsError> {
       let mut out = Vec::new();
       for (lineno, line) in content.lines().enumerate() {
           if line.trim().is_empty() { continue; }
           let v: serde_json::Value = serde_json::from_str(line)
               .map_err(|e| FsError::ParseError {
                   path: PathBuf::from("<jsonl>"),
                   reason: format!("line {}: {}", lineno + 1, e),
               })?;
           out.push(v);
       }
       Ok(out)
   }
   ```

3. Integration with `LoroValue::List` + log-schema: a log block's content is a `LoroValue::List` of JSON-shaped entries. The `jsonl` format module takes the list, serializes each entry on its own line, preserving append order.

**Testing:**

- Unit tests for round-trip equivalence (entries → JSONL → entries).
- Large-line test: 1 MB single entry parses without issue.
- Malformed-line test: second of three lines is bad JSON; assert error contains the line number.

**Verification:**

Run: `cargo nextest run -p pattern_memory --lib fs::jsonl`
Expected: all pass.

**Commit:** `[pattern-memory] add fs/jsonl.rs Log block serialization`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

**GATE (main-executor sign-off):**

- `cargo check -p pattern_memory` clean.
- All proptest round-trip tests pass across 1000+ cases.
- Spot-check KDL output on a representative Map block + Composite block manually — does the emitted KDL look human-readable? If not, flag to implementor; the point of KDL over JSON is readability.

After the gate, Subcomponent B (subscribers + watcher) begins.

---

<!-- START_SUBCOMPONENT_B (tasks 4-7) -->

### Subcomponent B: Loro-native subscribers + supervisor + notify watcher

<!-- START_TASK_4 -->
### Task 4: Rename `MemoryCache::evict` → `drop_doc`; establish subscriber hook point

**Verifies:** prerequisite for AC7.* (clean lifecycle boundary)

**Files:**
- Modify: `crates/pattern_memory/src/cache.rs` (rename `evict` → `drop_doc`)
- Modify: all call sites across pattern_runtime, pattern_cli, tests

**Implementation:**

1. Rename `MemoryCache::evict(agent_id, label)` → `drop_doc(agent_id, label)`. Signature preserved; behavior extended: on call, if a sync worker is running for this doc, cancel the worker, join its thread, remove it from the worker registry before removing the block from the cache.

2. Add a `DocSubscriberRegistry` field to `MemoryCache`:

   ```rust
   pub struct MemoryCache {
       // ... existing ...
       subscribers: Arc<DashMap<DocId, SubscriberHandle>>,
   }

   struct SubscriberHandle {
       cancel: tokio_util::sync::CancellationToken,
       thread: std::thread::JoinHandle<()>,
       // heartbeat state shared with supervisor
   }
   ```

   Lazy-spawn: on the first **write** to a doc, spawn the subscriber (Task 5 below). Reads don't spawn subscribers — pure read traffic doesn't need fs emission.

3. `drop_doc` impl:

   ```rust
   pub fn drop_doc(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
       let key = DocId::from((agent_id, label));
       if let Some((_, handle)) = self.subscribers.remove(&key) {
           handle.cancel.cancel();
           let _ = handle.thread.join();  // best-effort; log on panic but continue
       }
       // ... existing block removal logic ...
   }
   ```

**Testing:**

- Unit test: call `drop_doc` on a doc with a spawned subscriber; verify worker thread exits within 1s.
- Unit test: `drop_doc` on a doc that was never written (no subscriber): succeeds without panic.

**Verification:**

Run: `grep -rn "\.evict(" crates/ --include="*.rs"`
Expected: zero matches (all renamed).

Run: `cargo nextest run -p pattern_memory --lib cache::drop_doc`
Expected: passes.

**Commit:** `[pattern-memory] rename MemoryCache::evict → drop_doc; add subscriber registry + cancel/join lifecycle`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Per-doc sync worker (OS thread + crossbeam-channel + tokio_util::CancellationToken)

**Verifies:** v3-memory-rework.AC7.1, AC7.2, AC7.3, AC7.5

**Files:**
- Create: `crates/pattern_memory/src/subscriber/mod.rs` (re-exports)
- Create: `crates/pattern_memory/src/subscriber/worker.rs` (sync worker loop)
- Create: `crates/pattern_memory/src/subscriber/event.rs` (event types)
- Create: `crates/pattern_memory/src/reembed/mod.rs` (re-embed queue — async task)

**Library-first audit results** (established 2026-04-19 via two research agents):

- Stdlib `std::thread::spawn` for the thread itself — no focused crate wraps this better.
- `crossbeam-channel` for bounded intake + debounce multiplex via `select!` with `after(...)`. Standard, widely-vendored, exactly the right ergonomics.
- `tokio_util::sync::CancellationToken` for cross-thread cancel (async supervisor ↔ sync worker). Already a transitive dep; promote to explicit.
- Heartbeat + restart supervisor: hand-rolled ~60 lines as a tokio task. No single crate wraps "watch N OS thread heartbeats, restart on timeout" in a focused way (actor frameworks are too heavy; `task-supervisor` targets tokio tasks not threads; `stoppable_thread` only handles cancel).

**Implementation:**

1. Subscriber event type:

   ```rust
   pub struct CommitEvent {
       pub doc_id: DocId,
       pub frontier_before: loro::VersionVector,
       // Content hash at commit time — subscriber compares to last_emitted_hash
       // to decide whether to re-emit + re-embed.
       pub content_hash: [u8; 32],
   }
   ```

2. Worker loop:

   ```rust
   fn run_subscriber(
       doc_id: DocId,
       rx: crossbeam_channel::Receiver<CommitEvent>,
       cancel: tokio_util::sync::CancellationToken,
       pool: Arc<r2d2::Pool<SqliteConnectionManager>>,
       reembed_tx: tokio::sync::mpsc::UnboundedSender<ReembedRequest>,
       heartbeat_tx: crossbeam_channel::Sender<Heartbeat>,
       mount_path: Arc<Path>,
       doc: Arc<LoroDoc>,
   ) {
       let mut last_emitted_hash: Option<[u8; 32]> = None;
       loop {
           if cancel.is_cancelled() { break; }

           // Block waiting for event OR 50ms debounce timeout OR cancel tick.
           let deadline = std::time::Instant::now() + Duration::from_millis(50);
           let mut latest_event: Option<CommitEvent> = None;
           crossbeam_channel::select! {
               recv(rx) -> msg => {
                   match msg {
                       Ok(ev) => { latest_event = Some(ev); }
                       Err(_) => break,  // sender dropped → unload in progress
                   }
               }
               default(Duration::from_millis(50)) => {
                   // Heartbeat ping — no event, just prove we're alive.
                   let _ = heartbeat_tx.send(Heartbeat { doc_id: doc_id.clone(), at: Instant::now() });
                   continue;
               }
           }

           // Drain any further events that arrived within the debounce window.
           while std::time::Instant::now() < deadline {
               match rx.try_recv() {
                   Ok(ev) => latest_event = Some(ev),
                   Err(_) => break,
               }
               std::thread::sleep(Duration::from_millis(5));
           }

           let Some(event) = latest_event else { continue; };

           // Compute the emission content from the doc's current state.
           let (canonical_bytes, schema_kind) = match render_canonical(&doc, &event.doc_id) {
               Ok(out) => out,
               Err(e) => {
                   metrics::counter!("memory.subscriber.render_failed").increment(1);
                   tracing::error!(doc_id = ?event.doc_id, error = %e, "render failed");
                   continue;
               }
           };

           // blake3 matches the workspace content-hash convention
           // (pattern_runtime::compose uses blake3::hash(..).as_bytes()[..8]).
           let new_hash: [u8; 32] = blake3::hash(&canonical_bytes).into();

           // Self-emit-echo suppression: if the hash matches what we already wrote,
           // skip. (notify watcher should have the same hash in last_emitted_hash
           // to filter its echo events, but the worker-side check is defense-in-depth.)
           if Some(new_hash) == last_emitted_hash {
               let _ = heartbeat_tx.send(Heartbeat { doc_id: doc_id.clone(), at: Instant::now() });
               continue;
           }

           // Emit the canonical file + update FTS5 row + queue re-embed if needed.
           let file_path = canonical_path(&mount_path, &event.doc_id, schema_kind);
           if let Err(e) = fs::atomic_write(&file_path, &canonical_bytes) {
               metrics::counter!("memory.subscriber.fs_write_failed").increment(1);
               tracing::error!(path = ?file_path, error = %e, "atomic_write failed");
               continue;
           }

           // Borrow a pooled connection, update FTS5.
           match pool.get() {
               Ok(conn) => {
                   if let Err(e) = pattern_db::queries::memory::upsert_memory_block_fts(
                       &conn, &event.doc_id, &canonical_bytes
                   ) {
                       metrics::counter!("memory.subscriber.fts_update_failed").increment(1);
                       tracing::error!(doc_id = ?event.doc_id, error = %e, "fts update failed");
                   }
               }
               Err(e) => {
                   metrics::counter!("memory.subscriber.pool_exhausted").increment(1);
                   tracing::error!(error = %e, "pool get failed");
               }
           }

           // Re-embed only when content changes (AC7.3).
           if last_emitted_hash.is_some_and(|prev| prev != new_hash) || last_emitted_hash.is_none() {
               let _ = reembed_tx.send(ReembedRequest {
                   doc_id: event.doc_id.clone(),
                   canonical_bytes: canonical_bytes.clone(),
                   content_hash: new_hash,
               });
           }
           last_emitted_hash = Some(new_hash);

           let _ = heartbeat_tx.send(Heartbeat { doc_id: doc_id.clone(), at: Instant::now() });
       }
   }
   ```

3. Re-embed queue (tokio side):

   ```rust
   pub struct ReembedQueue { rx: tokio::sync::mpsc::UnboundedReceiver<ReembedRequest> }

   impl ReembedQueue {
       pub fn spawn(provider: Arc<dyn EmbeddingProvider>, pool: Arc<r2d2::Pool<...>>) -> (Self, mpsc::UnboundedSender<ReembedRequest>) {
           let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
           tokio::spawn(async move {
               while let Some(req) = rx.recv().await {
                   // Compute embedding via async provider.
                   // Persist to vec0 via spawn_blocking (genuine async→sync DB call here).
               }
           });
           (Self { rx }, tx)
       }
   }
   ```

   The re-embed queue is the one place where a spawn_blocking-style bridge is genuinely warranted: it sits on the async side (calls async `EmbeddingProvider::embed`) and writes to the sync rusqlite pool via `spawn_blocking`. That's the right pattern here because the async work (embedding provider call) dominates the latency; the sync DB write is secondary.

4. Wire `StructuredDocument::subscribe_root`: when a doc is first written, `MemoryCache` spawns the subscriber (Task 4's registry) and attaches a loro callback that pushes `CommitEvent`s into the subscriber's crossbeam-channel:

   ```rust
   let (event_tx, event_rx) = crossbeam_channel::bounded(64);
   let _subscription = doc.subscribe_root(move |event| {
       // Compute hash, frontier, etc. — sync, runs on the commit thread.
       let _ = event_tx.try_send(CommitEvent { ... });  // drop on overflow
       // (Or .send() with block — design says bounded with backpressure.)
   });
   ```

   Note: subscription handle must outlive the subscriber thread; store it in the `SubscriberHandle` struct (Task 4) so dropping the handle unsubscribes.

**Testing:**

- Integration test: write to a block, observe file emitted within 100ms (AC7.1).
- Integration test: write to a block, assert FTS5 row contains the new content (AC7.2).
- Integration test: write same content twice, assert only ONE re-embed request queued (AC7.3).
- Self-emit-echo test: write content, observe ONE emission (AC7.5).
- Bounded channel backpressure test: flood 1000 events rapidly; assert worker completes all without panic.

**Verification:** per above.

**Commit:** `[pattern-memory] per-doc sync subscriber on OS thread with crossbeam-channel + debounce + FTS5 update + re-embed queue`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Subscriber supervisor (async tokio task) — heartbeat watchdog + restart

**Verifies:** v3-memory-rework.AC7.7

**Files:**
- Create: `crates/pattern_memory/src/subscriber/supervisor.rs`

**Implementation:**

```rust
pub struct SubscriberSupervisor {
    heartbeat_rx: crossbeam_channel::Receiver<Heartbeat>,
    workers: Arc<DashMap<DocId, SubscriberState>>,
    timeout: Duration,  // 30s per design
    // ... shared pool, reembed_tx, mount_path Arc<Path> ...
}

struct SubscriberState {
    last_heartbeat: Instant,
    handle: SubscriberHandle,
    restart_count: u32,
}

impl SubscriberSupervisor {
    pub fn spawn(/* deps */) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        // Poll heartbeat channel non-blockingly for recent heartbeats.
                        while let Ok(hb) = self.heartbeat_rx.try_recv() {
                            if let Some(mut state) = self.workers.get_mut(&hb.doc_id) {
                                state.last_heartbeat = hb.at;
                            }
                        }

                        // Check timeouts.
                        let now = Instant::now();
                        let to_restart: Vec<DocId> = self.workers.iter()
                            .filter(|e| now.duration_since(e.value().last_heartbeat) > self.timeout)
                            .map(|e| e.key().clone())
                            .collect();

                        for doc_id in to_restart {
                            metrics::counter!("memory.sync_worker.restart",
                                              "doc_id" => doc_id.to_string()).increment(1);
                            tracing::error!(doc_id = ?doc_id, "subscriber heartbeat timeout; restarting");
                            self.restart(doc_id).await;
                        }

                        metrics::gauge!("memory.sync_worker.active").set(self.workers.len() as f64);
                    }
                    // cancel on MemoryCache drop — add CancellationToken here
                }
            }
        })
    }

    async fn restart(&self, doc_id: DocId) {
        if let Some((_, state)) = self.workers.remove(&doc_id) {
            state.handle.cancel.cancel();
            // join in a spawn_blocking — OS thread join is a blocking op
            tokio::task::spawn_blocking(move || { let _ = state.handle.thread.join(); }).await.ok();
            // Respawn with a fresh cancel token and fresh restart_count + 1.
            // (Implementation details: MemoryCache needs a spawn_subscriber helper
            // that takes a doc_id and returns a new SubscriberHandle.)
        }
    }
}
```

**Testing:**

- Panic test: inject a panic into the subscriber's main loop via a test-only deps injection; assert supervisor detects timeout within 30s, logs ERROR, restart counter increments, worker is respawned.
- Heartbeat-liveness test: normal write traffic produces heartbeats within timeout window; no spurious restarts.

**Verification:** per above.

**Commit:** `[pattern-memory] subscriber supervisor with 30s heartbeat timeout + restart + metrics`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: `fs/watcher.rs` — notify 8.2 + notify-debouncer-full + loro CRDT import for external edits

**Verifies:** v3-memory-rework.AC7.4, AC7.6, AC7.8

**Files:**
- Create: `crates/pattern_memory/src/fs/watcher.rs`

**Implementation:**

Port + adapt the patterns from `rewrite-staging/runtime_subsystems/data_source/file_source.rs`. Key adaptations:

- File paths derived from `(DocId, schema_kind) → mount_path/blocks/<tier>/<label>.<ext>` (not arbitrary paths).
- Event filter: only paths matching the block-path scheme; ignore tmp files from atomic_write.
- On event: hash the current file; if matches `last_emitted_hash` for the doc, drop (self-echo); otherwise read + parse via the appropriate format module + `doc.import(as_update)` to merge via CRDT.

```rust
pub struct MountWatcher {
    _debouncer: notify_debouncer_full::Debouncer<...>,
    // ... channel to ingest task ...
}

impl MountWatcher {
    pub fn start(
        mount_path: &Path,
        cache: Arc<MemoryCache>,
        last_emitted_hashes: Arc<DashMap<PathBuf, [u8; 32]>>,
    ) -> Result<Self, FsError> {
        let (tx, rx) = crossbeam_channel::bounded(256);
        let mut debouncer = notify_debouncer_full::new_debouncer(
            Duration::from_millis(500),
            None,
            move |res: DebounceEventResult| {
                // Fires on the notify thread. Forward events to the ingest task.
                if let Ok(events) = res {
                    for event in events {
                        let _ = tx.try_send(event);
                    }
                }
            }
        )?;
        debouncer.watch(mount_path, RecursiveMode::Recursive)?;

        // Spawn ingest thread — sync worker, same shape as the subscribers.
        let last_hashes = last_emitted_hashes.clone();
        std::thread::spawn(move || ingest_loop(rx, cache, last_hashes));

        Ok(MountWatcher { _debouncer: debouncer })
    }
}

fn ingest_loop(
    rx: crossbeam_channel::Receiver<DebouncedEvent>,
    cache: Arc<MemoryCache>,
    last_emitted_hashes: Arc<DashMap<PathBuf, [u8; 32]>>,
) {
    for event in rx {
        let path = &event.paths[0];
        if !is_block_path(path) { continue; }

        // Hash the current file.
        let content = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let hash: [u8; 32] = blake3::hash(&content).into();

        // Self-echo suppression.
        if last_emitted_hashes.get(path).map(|v| *v) == Some(hash) {
            continue;
        }

        // Parse + merge.
        let (doc_id, schema_kind) = parse_block_path(path);
        let parsed = match schema_kind {
            SchemaKind::Text => LoroValue::from_string_via_markdown(&content),
            SchemaKind::Map | SchemaKind::List | SchemaKind::Composite => {
                match kdl::KdlDocument::parse(&String::from_utf8(content).ok().unwrap_or_default()) {
                    Ok(doc) => match fs::kdl::kdl_to_loro_value(&doc) {
                        Ok(v) => v,
                        Err(e) => {
                            metrics::counter!("memory.kdl.parse_failed").increment(1);
                            tracing::warn!(path = ?path, error = %e, "kdl parse failed; dropping human edit");
                            continue;  // AC7.6
                        }
                    }
                    Err(e) => {
                        metrics::counter!("memory.kdl.parse_failed").increment(1);
                        tracing::warn!(path = ?path, error = %e, "kdl syntax error");
                        continue;
                    }
                }
            }
            SchemaKind::Log => { /* jsonl parse */ }
        };

        // Merge via doc.import — AC7.8 CRDT semantics.
        if let Some(doc) = cache.get_doc(&doc_id) {
            match doc.import_as_loro_value(parsed) {
                Ok(_) => {
                    doc.commit();  // fires subscriber → re-emits canonical
                    metrics::counter!("memory.external_edit.merged").increment(1);
                }
                Err(e) => {
                    tracing::warn!(doc_id = ?doc_id, error = %e, "external edit merge failed");
                }
            }
        }
    }
}
```

Note: `doc.import_as_loro_value` is pseudocode for "convert the parsed LoroValue into a loro update and apply via `doc.import`". The actual API is `doc.import(&update_bytes)` where update_bytes is a serialized loro-update; the "parsed LoroValue → update bytes" conversion may require intermediate steps — validate at implementation time against the loro 1.6 API.

**Testing:**

- External-edit test: write a block via cache; close a text editor handle with new content on the .md file; assert loro state merges the edit within 1s; assert re-emission produces canonical (AC7.4).
- Invalid KDL test: write malformed KDL to a .kdl file; assert `metrics::counter!("memory.kdl.parse_failed")` increments; assert no merge happens; assert subscriber re-emits previous valid content (AC7.6).
- Concurrent agent+human test: start a write via agent, simultaneously edit the .md file; loro merges both; assert final state reflects both (AC7.8).

**Verification:** per above.

**Commit:** `[pattern-memory] fs/watcher.rs with notify 8.2 + notify-debouncer-full + CRDT import on external edit + self-echo suppression`
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Update design doc to reflect sync_worker OS-thread architecture

**Verifies:** documentation consistency post-Phase-4

**Files:**
- Modify: `docs/design-plans/2026-04-19-v3-memory-rework.md`

**Implementation:**

The design plan at multiple points describes sync_workers as tokio tasks. This was the initial architectural intent but the implementation (this phase) uses OS threads instead — sync-dominant workload (rusqlite FTS5 updates, file I/O, blake3 hashing) belongs on OS threads, and the library-first survey confirmed no crate wraps "lazy-spawn per-resource-ID worker + bounded intake + debounce + heartbeat supervision" for either thread or task models.

Specific design-plan passages to update:

**A. sync_worker references (tokio task → OS thread):**
- Line 64: "Storage topology: **loro-primary with per-doc subscribers**. Writes go to loro; `doc.subscribe_root` callbacks fire post-commit; per-doc `sync_worker` tokio tasks emit the canonical file..." → change "tokio tasks" to "OS threads".
- Around line 334 (Architecture section): the subscriber flow diagram. Change "(tokio task; supervised)" to "(OS thread; supervised by async task)".
- Around line 447: "`sync_worker` (plural, per-LoroDoc): tokio tasks in `pattern_memory::subscriber`..." → change to "OS threads in `pattern_memory::subscriber`".
- Around line 1110 (Glossary for `sync_worker`): same change.

**B. MemoryStore method count (28 → "~18" → actual 19):**

The design plan says "audited down from 28 to ~18" but Phase 3's actual arithmetic (28 − 3 list collapse − 3 setter collapse − 1 undo/redo − 1 depth − 1 search = 19) lands at 19. Update all 6 references to say 19:

- Line 26: "audited down from 28 methods to ~18 via collapse" → "audited down from 28 methods to 19 via collapse"
- Line 195: AC4.2 text "Trait has 18 methods" → "Trait has 19 methods"
- Line 316: Glossary "it has 18 methods" → "it has 19 methods"
- Line 393: "consolidate to ~18" → "consolidate to 19"
- Line 724: "trait surface audited from 28 → ~18" → "trait surface audited from 28 → 19"
- Line 816: "consolidated down to 18 methods" → "consolidated down to 19 methods"

The arithmetic shown in phase_03.md's header is the canonical derivation; it can be quoted verbatim into the design plan's architecture section if a reviewer wants the math visible alongside the new number.

Also add a new paragraph in the design plan's Architecture section near the subscriber description explaining the decision:

> **Why OS threads, not tokio tasks** (2026-04 implementation note): sync_worker workload is sync-dominant — rusqlite FTS5 updates, file I/O, blake3 hashing. A tokio task wrapping `spawn_blocking` for every step would be needless overhead for a 50-sub-1000 active-worker scale. Loro's `subscribe_root` callback is already synchronous. The supervisor that watches heartbeats is async (tokio task) because it naturally multiplexes across N workers; the workers themselves are plain `std::thread::spawn`ed with `crossbeam-channel` intake + `tokio_util::sync::CancellationToken` for cross-thread cancel. The library-first survey (`docs/implementation-plans/2026-04-19-v3-memory-rework/phase_04.md` — Task 5's library-first audit block) confirmed no single focused crate wraps this pattern; we compose stdlib threads + crossbeam + tokio-util + a hand-rolled ~60-line supervisor.

Similar principles apply to Phase 3's eval worker (sync-dominant evaluation workload on a plain OS thread driven by `std::sync::mpsc`), though eval_worker's requirements are simpler (no multiplex), so it uses stdlib channels rather than crossbeam.

**Testing:**

Documentation change — manual verification.

**Verification:**

Run: `grep -n 'tokio task\|tokio tasks' docs/design-plans/2026-04-19-v3-memory-rework.md`
Expected: zero occurrences in sync_worker-related passages. Other tokio-task mentions (for the supervisor, the re-embed queue, etc.) stay.

Run: `grep -cn '~18\|"18 methods"\|18 via collapse' docs/design-plans/2026-04-19-v3-memory-rework.md`
Expected: zero. All six method-count references have been updated to 19.

**Commit:** `[meta] design plan: update sync_worker references to OS threads + method count 18 → 19 (post-Phase-3/4 decision records)`
<!-- END_TASK_8 -->

<!-- END_SUBCOMPONENT_B -->

---

## Phase 4 Done-when recap

- `cargo check --workspace` clean.
- `cargo nextest run -p pattern_memory` passes every Phase 4 test, including proptest round-trip (1000+ cases) and all integration/regression tests.
- `cargo nextest run --workspace` still green (no collateral breakage).
- All AC6.* and AC7.* test cases have a corresponding test file + test function; all pass.
- `metrics::counter!("memory.sync_worker.restart")` + `metrics::gauge!("memory.sync_worker.active")` + `metrics::counter!("memory.kdl.parse_failed")` + `metrics::counter!("memory.external_edit.merged")` + `metrics::counter!("memory.subscriber.*_failed")` all wired.
- `pattern_memory/CLAUDE.md` freshened with subscriber supervisor + watcher architecture.

## Notes for downstream phases

- **Phase 5** (jj CLI adapter + quiesce): `quiesce()` signals every subscriber to drain (via the cancel token's child-token pattern or a dedicated "drain" message in the event channel). The supervisor is the natural coordinator for this — quiesce flows through it. Phase 5's adapter relies on subscribers being durable + supervised (this phase) so drain is always possible.
- **Phase 6** (storage modes + attach/detach): `MountWatcher` is per-mount. Mode A/B/C attach spawns a watcher on the mount's path; detach cancels + joins it. Lifecycle tied to `MountedStore`.
- **Phase 7** (messages.db backup): backup runs while subscribers are active; rusqlite's backup API is atomic w.r.t. concurrent writers, so subscribers don't need special handling during backup windows.
- **Phase 8 capstone** (end-to-end smoke): the smoke test exercises every subcomponent of this phase — write block → file emitted → FTS5 updated → external edit → merged → re-emitted. Phase 8's smoke test is a complete regression check over Phase 4.
- **kdl crate maintainer contingency**: design plan notes the upstream kdl maintainer's stance on AI-assisted development. No hostile actions observed. If that changes, fork at last safe version; pin the fork in workspace. Not part of this phase's deliverable — just a contingency flagged in the port-list doc.
