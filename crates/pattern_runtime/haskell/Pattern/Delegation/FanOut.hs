-- | Pattern.Delegation.FanOut — parallel fan-out across a worker pool.
--
-- Submits the same task to every worker concurrently and collects results
-- in worker order. Per-id failure is preserved via 'Spawn.SpawnAwaitOutcome'
-- so callers can inspect partial outcomes without short-circuiting.
--
-- The fan-out is genuine parallel: all workers are spawned before any
-- await is issued. 'Spawn.awaitAll' issues a single sync-bridge round-trip
-- on the Rust side, so N workers cost one crossing, not N.
module Pattern.Delegation.FanOut (fanOut) where

import Pattern.Prelude
import Control.Monad.Freer (Eff, Member)
import qualified Pattern.Spawn as Spawn

-- | Submit the same task to every worker in parallel; collect results in
-- worker order.
--
-- Each worker config is augmented with the shared task via @attach@ before
-- the ephemeral is spawned. The @attach@ function receives the task and
-- the per-worker config and returns the modified config, allowing callers
-- to embed task context in whichever field is appropriate (e.g.
-- 'Spawn.ephemeralPrompt').
--
-- Results are 'Spawn.SpawnAwaitOutcome' rather than a bare
-- @Either Spawn.SpawnError Spawn.SpawnResult@ so callers can pattern-match
-- on 'Spawn.SpawnOk' \/ 'Spawn.SpawnFail' without unpacking 'Either'.
fanOut
    :: Member Spawn.Spawn effs
    => [Spawn.EphemeralConfig]
    -- ^ worker configs; one ephemeral is spawned per entry
    -> task
    -- ^ shared task value attached to each worker
    -> (task -> Spawn.EphemeralConfig -> Spawn.EphemeralConfig)
    -- ^ function that embeds the task into a worker config
    -> Eff effs [Spawn.SpawnAwaitOutcome]
fanOut workers task attach = do
    -- Spawn all workers before awaiting any — genuine parallel fan-out.
    -- Each call returns immediately with an 'EphemeralSpawn' handle; the
    -- child session runs in the background on the Rust side.
    spawns <- traverse (\w -> Spawn.ephemeral (attach task w)) workers
    -- Collect all spawn ids and await the whole batch in a single
    -- sync-bridge round-trip. Result order matches the input worker order.
    Spawn.awaitAll (map Spawn.ephemeralSpawnId spawns)
