{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Memory read agent using the Prelude-5 effect list.
module MemoryRead (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log

agent :: Eff '[Memory, Message, Display, Time, Log] ()
agent = do
  v <- get "scratchpad"
  info v
