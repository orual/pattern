# pattern_runtime

Agent runtime for Pattern v3. Houses Tidepool (Haskell-in-Rust) embedding, the
agent turn loop, `freer-simple` effect handlers, and turn-level checkpoint
machinery. Depends only on `pattern_core` trait definitions.

See the v3 foundation design at
`docs/design-plans/2026-04-16-v3-foundation.md` for the substrate choice,
SDK hierarchy, and phase ordering.

## Runtime setup

`pattern_runtime` compiles agent Haskell programs via the `tidepool-runtime`
Rust crate, which shells out to the **`tidepool-extract`** GHC plugin binary
(~300 MB, GHC 9.12). The binary must be available at runtime or
`compile_haskell()` fails. Resolution order:

1. `$TIDEPOOL_EXTRACT` env var if set (absolute path to the binary).
2. `tidepool-extract` on `$PATH` otherwise.

### With Nix (recommended)

```sh
nix develop   # enters pattern-shell with tidepool-extract on PATH
                # and $TIDEPOOL_EXTRACT exported to the absolute store path
which tidepool-extract   # should print a /nix/store/... path
```

The devshell module at `nix/modules/devshell.nix` pulls the
`github:tidepool-heavy-industries/tidepool` flake input and surfaces the
binary via the `tidepool-extract` derivation. The pinned revision lives in
`flake.lock`; bump it with `nix flake update tidepool` when chasing upstream
API changes.

Developers iterating on tidepool itself can override the input:

```sh
nix develop --override-input tidepool path:../tidepool
```

This picks up uncommitted local changes and skips the GitHub fetch.

### Without Nix

Clone and build tidepool-extract from
`https://github.com/tidepool-heavy-industries/tidepool` (requires GHC 9.12 +
Cabal; see that repo's `README.md` for build instructions). Then either
place the resulting binary on `$PATH` or export
`TIDEPOOL_EXTRACT=/abs/path/to/tidepool-extract`.

### Preflight

`pattern_runtime::preflight::check()` (Phase 3 Task 5) verifies the binary is
reachable and returns a structured error pointing at this section when the
setup is wrong. Run it at binary startup before opening any Session.

## Authoring agent programs

### SDK imports (current constraint)

Agent programs import from the `Pattern.*` SDK module tree (installed at
`$PATTERN_SDK_DIR` or `crates/pattern_runtime/haskell/Pattern/` by default):
`Pattern.Time`, `Pattern.Log`, `Pattern.Memory`, `Pattern.Message`,
`Pattern.Display`, plus `Pattern.Prelude` which re-exports the common subset.

**Imports must be unqualified**:

```haskell
-- Works:
import Pattern.Time
import Pattern.Log
-- With specific items (recommended for collision-aversion):
import Pattern.Time (now, Instant, Duration, seconds)

-- Does NOT work:
import qualified Pattern.Time as Time
-- then using `Time.now` — breaks.
```

**Why:** `pattern_runtime::tidepool::inline::inline_sdk_modules` preprocesses
agent source by flattening `Pattern.*` dependencies into the combined module
before `tidepool-extract` sees it. The flattening is a workaround for
tidepool's current limitation: multi-module compilation succeeds at extract
time but produces inconsistent `DataConTable` / `CoreExpr` state at JIT time
(manifests as `[CASE TRAP]` / `Jit(Yield(Undefined))`). The `haskell_inline!`
build-time macro in tidepool's own ecosystem uses the same flattening trick
for the same reason.

After flattening, the `Pattern.X` namespaces no longer exist as modules —
their top-level bindings are in scope directly. Qualified aliases
(`as Time`) become dangling references.

**Mitigation strategies** if a collision between SDK modules becomes a
problem:

- Use the explicit-list import form: `import Pattern.Time (now, Instant)` and
  `import Pattern.Log (info)` — only the listed names enter scope.
- Rename on import: `import Pattern.Time (now as timeNow)` where Haskell's
  `import` syntax allows.
- If the collision is unavoidable, inline a specific identifier in the agent
  source directly instead of importing it.

**This is provisional.** If upstream tidepool fixes multi-module DataCon
handling (tracking issue: the `investigation/multi-module-datacon-tags`
branch), the inliner becomes a no-op and qualified imports work natively.
At that point this section collapses to a one-liner. See
`crates/pattern_runtime/src/tidepool/inline.rs` for the current preprocessor
implementation.
