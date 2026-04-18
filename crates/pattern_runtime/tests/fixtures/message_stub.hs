{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal Message-only agent for `tests/stub_effects.rs` — calls
-- `Pattern.Message.ask`, which remains a candidate-for-removal stub in
-- Phase 5 Task 20 (Send/Reply/Notify were wired to the router registry,
-- but Ask doesn't fit v3's architecture: LLMs drive agent turns via
-- `run_turn` + the `code` tool, not vice versa).
module MessageStub (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Message

agent :: Eff '[Message] (MessageContent, Usage)
agent = ask "hello"
