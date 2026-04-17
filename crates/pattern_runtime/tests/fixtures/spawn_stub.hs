{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal Spawn-only agent for `tests/stub_effects.rs` — calls
-- `Pattern.Spawn.start`, stubbed in Phase 3.
module SpawnStub (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Spawn

agent :: Eff '[Spawn] ()
agent = do
  _ <- start "some-subagent"
  pure ()
