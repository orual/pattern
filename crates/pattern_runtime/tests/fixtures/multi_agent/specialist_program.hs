{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Specialist agent program for the multi-agent smoke test.
--
-- Receives a delegated computation, writes the result, and notifies the
-- supervisor. This file is a reference fixture; the actual code is inlined
-- in the scripted MockProviderClient turns (tests/support/multi_agent_scripts.rs).
module Agent where

import Control.Monad.Freer (Eff)
import qualified Pattern.Memory as Memory
import Pattern.Message (send)
import qualified Pattern.Log as Log

-- Exchange 1: execute the computation and report back.
agent :: Eff '[Memory.Memory, Message, Log.Log] ()
agent = do
  Log.info "specialist: executing computation task"
  Memory.put "specialist-result" "4"
  send "smoke-supervisor" "result: 4"
