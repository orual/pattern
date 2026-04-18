{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Memory read agent using the Prelude-7 effect list.
module MemoryRead (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Search
import Pattern.Recall
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log

agent :: Eff '[Memory, Search, Recall, Message, Display, Time, Log] ()
agent = do
  v <- get "scratchpad"
  info v
