# rewrite-staging/

Holding pen for pattern_core code in transit during the v3 foundation rewrite.

## Status

Active. Populated in Phase 2, drained by Phases 3–5 and beyond as each
subsystem is reshaped into its final home.

## Policy

- This directory is **not** a cargo member. Cargo never touches anything in here.
- Every file in this tree carries a fate-marker header (first line of every
  source file) naming:
  1. Its original path in pattern_core.
  2. Its destination crate + path.
  3. The phase (and where relevant, the AC) that will consume it.
  4. Any special note about required reshape.
- The port-list doc (`docs/plans/rewrite-v3-portlist.md`) "Staging contents"
  section is the authoritative index. Every file in here must be listed there.
- Files do not leave this directory by being edited in place. They leave by
  being pulled, reshaped, and committed into their final home. When all files
  destined for a crate have been consumed, the staging subdirectory is deleted
  in a dedicated commit.

## Layout

Organised by destination crate, not by origin:

- `agent_runtime/` — destined for pattern_runtime (Phase 3: loop, checkpoint, router)
- `runtime_subsystems/` — destined for pattern_runtime (tool registry, coordination, sources, realtime, queue)
- `context/` — destined for pattern_provider/compose (Phase 5: compression, block rendering)
- `provider/` — destined for pattern_provider (Phase 4: oauth, ModelProvider impls, embeddings)

## Fate-marker header format

Every source file in this tree begins with:

```rust
// MOVING TO: <destination-crate>/<relative-path>
// ORIGIN: <original-pattern_core-path>
// PHASE: <N> (AC: <ac-ids-if-any>)
// RESHAPE: <none | brief-note>
//
// This file is retained verbatim for reference during the v3 foundation
// rewrite. It does not compile in this location; rewrite-staging/ is not a
// cargo workspace member.
```

## When a subdirectory is drained

```bash
# After Phase 3 fully absorbs rewrite-staging/agent_runtime/:
jj new -m "[meta] remove drained staging dir: agent_runtime (absorbed by pattern_runtime)"
rm -r rewrite-staging/agent_runtime
# Update port-list "Staging contents" section accordingly in the same change.
```
