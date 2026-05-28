-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Memory read agent using the Prelude-7 effect list.
--
-- Pattern.Memory is qualified to avoid ambiguity with Pattern.Recall:
-- both modules now expose `get` at the top level since `recallGet` was
-- renamed to `get` in the hybrid scheme.
module MemoryRead (agent) where

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
  v <- Memory.get "scratchpad"
  info v
