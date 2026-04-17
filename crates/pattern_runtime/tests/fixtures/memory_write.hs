{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Memory write agent using the Prelude-5 effect list.
module MemoryWrite (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log

agent :: Eff '[Memory, Message, Display, Time, Log] ()
agent = do
  write "scratchpad" "hello from turn 1"
  info "scratchpad written"
