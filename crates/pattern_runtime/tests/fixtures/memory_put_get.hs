{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Memory Put + Get in a single turn. Exercises the
-- `MemoryHandler::record_exchange` wiring: the checkpoint log should
-- contain exactly two Memory exchanges (Put, Get) with tag 0.
module MemoryPutGet (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log

agent :: Eff '[Memory, Message, Display, Time, Log] ()
agent = do
  put "kv" "hello"
  _ <- get "kv"
  pure ()
