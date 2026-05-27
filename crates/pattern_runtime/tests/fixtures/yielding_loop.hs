-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Yielding-loop agent: calls `now` in a tight loop so the soft-cancel
-- path (watchdog flips the flag; next effect returns the sentinel)
-- exercises cleanly.
--
-- Loop size caveat: an upstream bug in tidepool's JIT corrupts closure
-- pointers after ~200k iterations of this exact shape (manifests as
-- `[JIT] App: tag 255 (UNKNOWN)` then SIGSEGV). 100k stays under that
-- threshold with a 64 MiB nursery, which is Pattern's default.
module YieldingLoop (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Search
import Pattern.Recall
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log

loop_ :: Int -> Eff '[Memory, Search, Recall, Message, Display, Time, Log] ()
loop_ 0 = pure ()
loop_ n = do
  _ <- now
  loop_ (n - 1)

agent :: Eff '[Memory, Search, Recall, Message, Display, Time, Log] ()
agent = loop_ 100000
