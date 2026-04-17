{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal Shell-only agent for `tests/stub_effects.rs` — calls
-- `Pattern.Shell.execute`, which the runtime handler rejects with a
-- `not implemented` EffectError.
module ShellStub (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Shell

agent :: Eff '[Shell] ()
agent = do
  _ <- execute "ls"
  pure ()
