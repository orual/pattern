{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal Message-only agent for `tests/stub_effects.rs` — calls
-- `Pattern.Message.send_`, stubbed in Phase 3 (Phase 4 wires the
-- pattern_provider backing).
module MessageStub (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Message

agent :: Eff '[Message] ()
agent = send_ "agent:other" "hello"
