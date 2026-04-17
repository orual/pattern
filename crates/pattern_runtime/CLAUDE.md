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
`github:orual/tidepool` flake input (our fork — see `flake.nix` for the
reasoning) and surfaces the binary via the `tidepool-extract` derivation.
The pinned revision lives in `flake.lock`; bump it with
`nix flake update tidepool` when chasing updated fixes on our fork or to
swap back to upstream once our patches merge.

Developers iterating on tidepool itself can override the input:

```sh
nix develop --override-input tidepool path:../tidepool
```

This picks up uncommitted local changes and skips the GitHub fetch.

### Stale-harness troubleshooting

**Symptom:** `test_cross_module_effect_runs` or other multi-module agent
compilation fails with `CASE TRAP` / `Jit(Yield(Undefined))`, *despite*
`flake.lock` pinning tidepool at a commit that contains the fix.

**Cause:** `$TIDEPOOL_EXTRACT` in the active devshell / direnv cache
points at an older `tidepool-extract` derivation built from a pre-fix
harness snapshot. The symlink chain
(wrapper → harness → haskell-snapshot) may be pinned to a stale store
path even after `flake.lock` moves forward.

**Recovery:**

```sh
# 1. Force eval of the pinned harness (no-op if cache already has it).
nix build github:orual/tidepool/$(jq -r '.nodes.tidepool.locked.rev' flake.lock)#tidepool-extract

# 2. Reload direnv — this is what actually refreshes $TIDEPOOL_EXTRACT.
direnv reload

# 3. Verify the resolved binary.
readlink -f "$TIDEPOOL_EXTRACT"
# Must match the path produced by step (1).
```

Or, for one-off runs: `TIDEPOOL_EXTRACT=$(nix build --print-out-paths .#tidepool-extract)/bin/tidepool-extract cargo nextest run ...`

Hardening opportunity (upstream, not urgent): add a
`tidepool-extract --version` endpoint whose commit-hash output
`tidepool-runtime::compile_haskell` cross-checks against its own
`EXPECTED_HARNESS_VERSION` constant at session open. Self-diagnosing
error instead of silent CASE TRAP.

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

### SDK imports

Agent programs import from the `Pattern.*` SDK module tree (installed at
`$PATTERN_SDK_DIR` or `crates/pattern_runtime/haskell/Pattern/` by default).
`tidepool-extract` compiles agents with the SDK directory on its include
path — both qualified and unqualified imports work:

```haskell
import qualified Pattern.File as F   -- recommended for rarer effects
F.read_ "/tmp/foo"

import Pattern.Time                  -- fine when no name collisions
now
```

The full 11-effect SDK is available: `Pattern.Memory`, `Pattern.Message`,
`Pattern.Display`, `Pattern.Time`, `Pattern.Log`, `Pattern.Shell`,
`Pattern.File`, `Pattern.Sources`, `Pattern.Mcp`, `Pattern.Ipc`,
`Pattern.Spawn`. `Pattern.Prelude` re-exports the common five
(`Memory, Message, Display, Time, Log`); the rarer modules have to be
imported explicitly because some share constructor names with Memory
(e.g. `Pattern.Memory.Read` vs `Pattern.File.Read`), and tidepool-bridge's
current `FromCore` lookup is by unqualified name only. In practice this
means agents using the rarer effects should import them `qualified` and
never let two conflicting modules be in unqualified scope simultaneously.

Effect-row ordering matters: handler position in the `SdkBundle` HList
determines the JIT effect tag. The canonical order is Prelude-5 first,
then rarer effects:
`Memory, Message, Display, Time, Log, Shell, File, Sources, Mcp, Ipc,
Spawn`. Agent `Eff '[...]` rows must line up with this prefix.
