{-# LANGUAGE NoImplicitPrelude #-}
-- | Pattern.Delegation.Pipeline — sequential ephemeral pipeline combinator.
--
-- Chains ephemeral stages where each stage's output becomes the next stage's
-- input. Each stage is an @(EphemeralConfig, output-decoder)@ pair: the
-- caller knows how to turn the stage's 'Spawn.SpawnResult' into the input
-- payload for the next stage.
--
-- Pipeline stages are sequential by design: stage N+1 depends on stage N's
-- output, so 'Spawn.ephemeral' + 'Spawn.awaitSpawn' in sequence via 'foldM'
-- is correct here. Do NOT use 'Spawn.awaitAll' — that is for independent
-- concurrent work, not data-dependent pipelines.
module Pattern.Delegation.Pipeline (pipeline) where

import Pattern.Prelude
import qualified Pattern.Spawn as Spawn
import Control.Monad.Freer (Eff, Member)

-- | Chain ephemeral stages where each stage's output becomes the next's input.
--
-- Each stage in the list is a pair of:
--
--   * an 'Spawn.EphemeralConfig' template for the stage's child session, and
--   * a decoder that converts the stage's 'Spawn.SpawnResult' into the
--     accumulator value passed to the next stage.
--
-- The @attach@ function splices the previous stage's output into the next
-- stage's config — typically by embedding it as the child's
-- 'Spawn.ephemeralPrompt' or serialising it into the program text.
--
-- Returns the decoded output of the final stage.
--
-- Example (3-stage parser → transformer → formatter pipeline):
--
-- @
-- result <- pipeline rawSource stages attachOutput
-- @
pipeline
    :: Member Spawn.Spawn effs
    => a                                              -- ^ initial input
    -> [(Spawn.EphemeralConfig, Spawn.SpawnResult -> a)]
       -- ^ per-stage (config template, result decoder) pairs
    -> (a -> Spawn.EphemeralConfig -> Spawn.EphemeralConfig)
       -- ^ feed stage-N output into stage-(N+1) ephemeral config
    -> Eff effs a
pipeline initialInput stages attach =
    foldM step initialInput stages
  where
    -- Pipeline stages are sequential by design (stage N+1 depends on stage N's
    -- output), so spawn + awaitSpawn in sequence is correct here.
    step acc (cfg, decode) = do
      let cfg' = attach acc cfg
      spawned <- Spawn.ephemeral cfg'
      result  <- Spawn.awaitSpawn (Spawn.ephemeralSpawnId spawned)
      pure (decode result)
