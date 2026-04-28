{-# LANGUAGE FlexibleContexts, NoImplicitPrelude #-}
-- | Pattern.Delegation.RoundRobin — distribute tasks across a fixed worker pool.
--
-- Cycles a list of ephemeral configs over an arbitrary task list, spawning one
-- child session per task and batch-awaiting all results via a single
-- @AwaitAll@ round-trip to the Rust side.
--
-- Ordering guarantee: results are returned in the same order as the input task
-- list, regardless of which worker runs each task or in what order children
-- complete. Partial failures are preserved — individual @SpawnFail@ outcomes do
-- not abort the batch.
module Pattern.Delegation.RoundRobin (roundRobin) where

import Pattern.Prelude
import Control.Monad.Freer (Eff, Member)
import qualified Pattern.Spawn as Spawn

-- | Distribute tasks across a fixed list of ephemeral worker configs.
--
-- Each task is assigned to the next worker in the ring via @cycle@; the
-- caller-supplied @attach@ function merges the task payload into the
-- per-task ephemeral config before spawning. All children are launched
-- in one @mapM ephemeral@ pass and then awaited together via a single
-- @AwaitAll@ sync-bridge round-trip.
--
-- Invariants:
--
-- * If @workers@ is empty, @roundRobin@ returns an empty list immediately
--   (no children spawned).
-- * Result order matches the @tasks@ input list (position-stable).
-- * Per-child failure (@SpawnFail@) is preserved so callers can inspect
--   partial outcomes.
--
-- Example (2 workers, 4 tasks → each worker runs exactly 2 tasks):
--
-- @
-- let workers = [cfg1, cfg2]
--     tasks   = ["task-a", "task-b", "task-c", "task-d"]
--     attach t w = w { ephemeralPrompt = Just t }
-- results <- roundRobin workers tasks attach
-- @
roundRobin
    :: Member Spawn.Spawn effs
    => [Spawn.EphemeralConfig]
    -- ^ worker pool; cycled over the task list. Empty → returns [].
    -> [task]
    -- ^ tasks to distribute; result list preserves this order.
    -> (task -> Spawn.EphemeralConfig -> Spawn.EphemeralConfig)
    -- ^ merge task payload into the per-task ephemeral config before spawning.
    -> Eff effs [Spawn.SpawnAwaitOutcome]
roundRobin []      _     _      = pure []
roundRobin workers tasks attach = do
    let assignments = zip tasks (cycle workers)
    -- Spawn all workers in parallel (fire-and-forget into the Rust registry),
    -- then batch-await in a single AwaitAll round-trip rather than awaiting
    -- each child individually (avoids N sequential sync-bridge crossings).
    spawns <- mapM (\(t, w) -> Spawn.ephemeral (attach t w)) assignments
    let ids = map Spawn.ephemeralSpawnId spawns
    Spawn.awaitAll ids
