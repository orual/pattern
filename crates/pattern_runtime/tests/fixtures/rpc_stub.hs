{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal Rpc-only agent for `tests/stub_effects.rs` — calls
-- `Pattern.Rpc.call`, stubbed in Phase 3.
module RpcStub (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Rpc

agent :: Eff '[Rpc] ()
agent = do
  _ <- call "http://localhost/rpc" "{}"
  pure ()
