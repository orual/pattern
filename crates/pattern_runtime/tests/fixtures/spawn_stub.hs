{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal Spawn-only agent for `tests/stub_effects.rs` — calls
-- `Pattern.Spawn.stop`, which Phase 2 Task 2 stubs with a per-variant
-- "not implemented in <task>" error. The full dispatch lands in
-- Phase 2 Task 4.
module SpawnStub (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Spawn

agent :: Eff '[Spawn] ()
agent = stop "some-spawn-id"
