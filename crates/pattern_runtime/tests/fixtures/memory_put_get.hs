-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
