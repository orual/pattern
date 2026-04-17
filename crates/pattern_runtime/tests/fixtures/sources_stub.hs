{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal Sources-only agent for `tests/stub_effects.rs` — calls
-- `Pattern.Sources.list`, stubbed in Phase 3.
module SourcesStub (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Sources

agent :: Eff '[Sources] ()
agent = do
  _ <- list
  pure ()
