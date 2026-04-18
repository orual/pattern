{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Memory Put + Get in a single turn. Exercises the
-- `MemoryHandler::record_exchange` wiring: the checkpoint log should
-- contain exactly two Memory exchanges (Put, Get) with tag 0.
--
-- Pattern.Memory is qualified to avoid ambiguity with Pattern.Recall:
-- both modules now expose `get` at the top level since `recallGet` was
-- renamed to `get` in the hybrid scheme.
module MemoryPutGet (agent) where

import Control.Monad.Freer (Eff)
import qualified Pattern.Memory as Memory
import Pattern.Search
import Pattern.Recall
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log

agent :: Eff '[Memory.Memory, Search, Recall, Message, Display, Time, Log] ()
agent = do
  Memory.put "kv" "hello"
  _ <- Memory.get "kv"
  pure ()
