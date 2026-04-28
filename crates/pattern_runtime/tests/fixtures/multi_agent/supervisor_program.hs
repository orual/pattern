{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Supervisor agent program for the multi-agent smoke test.
--
-- Delegates a computation task to the specialist and reads back the result.
-- This file is a reference fixture; the actual code is inlined in the
-- scripted MockProviderClient turns (tests/support/multi_agent_scripts.rs).
module Agent where

import Control.Monad.Freer (Eff)
import qualified Pattern.Memory as Memory
import Pattern.Message (send)
import qualified Pattern.Log as Log

-- Exchange 1: delegate to specialist.
agent :: Eff '[Memory.Memory, Message, Log.Log] ()
agent = do
  Log.info "supervisor: routing task to specialist"
  Memory.put "delegation-log" "delegated: compute 2+2"
  send "smoke-specialist" "compute 2+2"
