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
path — all 11 modules are compiled and linked together.

The SDK uses distinct constructor names across modules, so `import
Pattern.Prelude` unqualified works even for agents that mix effects:

```haskell
import Pattern.Prelude

agent = do
  put "notes" "hello"           -- Memory.Put
  write "/tmp/f" "contents"     -- File.Write  (distinct from Memory.Put)
  _ <- read_ "/tmp/f"           -- File.Read
  _ <- get "notes"              -- Memory.Get  (distinct from File.Read)
  send_ "agent:orual" "ping"    -- Message.Send
  info "done"                   -- Log.Info
```

Qualified imports remain a fine stylistic choice when you want explicit
module attribution at the call site:

```haskell
import qualified Pattern.Memory as Memory
import qualified Pattern.File as File

agent = do
  Memory.Get "notes"
  File.Read "/tmp/f"
```

Collision-avoidance decisions on the Haskell side:

- `Memory` uses `Get`/`Put` (KV semantics) — leaving `Read`/`Write` to `File`.
- `File.List` is `ListDir` — leaves `List` to `Sources` (list all sources).
- `Rpc.Call` (request/response) — leaves `Send` to `Message` for
  agent-to-agent messaging.

Defense-in-depth at the host-runtime decode boundary is provided by the
derive layer (arity disambiguation + `#[core(module = "Pattern.<Module>",
name = "...")]` on every SDK request variant).

Effect-row ordering matters: handler position in the `SdkBundle` HList
determines the JIT effect tag. The canonical order is Prelude-5 first,
then rarer effects:
`Memory, Message, Display, Time, Log, Shell, File, Sources, Mcp, Rpc,
Spawn`. Agent `Eff '[...]` rows must line up with this prefix.

## Known flakes — MUST fix before GA

These tests pass in isolation but intermittently fail under
`cargo nextest run --workspace` parallel load. Observed 2026-04-17
during Phase 5 Tier 1 work; different tests fail on different runs,
so the root cause is load-induced contention rather than a per-test
regression. **This is tech debt that blocks shipping a stable release**
— CI that occasionally fails for reasons unrelated to the PR under
review corrodes trust in the signal.

**Flaky tests observed so far:**

- `session_lifecycle::open_step_twice_does_not_recompile`
- `timeout::hard_abandon_await_enforces_cancel_grace_ceiling`

Both touch the `tidepool-extract` subprocess path. Hypothesis: when N
parallel test binaries spawn `tidepool-extract` concurrently, they
contend on some combination of:

- Shared cache / temp-dir paths (spurious "was recompiled" signal when
  another test touched the cache state between open and step)
- Wall-clock margins tight enough that scheduler jitter under load
  pushes grace-ceiling assertions past their threshold
- Filesystem-level races on the extract binary's lockfile or scratch
  directory

**Investigation vectors** (pick up when we come back to this):

1. Add tracing-level logging to the subprocess spawn / cache-lookup
   path to see which shared resource is getting hit.
2. Run the suite under `cargo nextest run --test-threads=1` to confirm
   single-threaded runs are always clean. If yes, contention is the
   whole story; if no, there's a second bug.
3. Check whether per-test tempdirs are actually per-test, or whether
   something's collapsing to a shared `/tmp` or `$XDG_CACHE_HOME` path.
4. For the timeout test specifically: widen the grace ceiling to
   something less schedule-sensitive, or switch from wall-clock to a
   deterministic tokio-test clock.

**Why not fix it now:** the flake is intermittent, passes on rerun, and
doesn't block Phase 5 work. Pushing it behind a phase boundary prevents
scope creep. But it must be addressed before shipping — a flaky CI is
worse than a slower CI.
