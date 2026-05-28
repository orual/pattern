-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings, BangPatterns #-}
-- | Infinite-spin agent: a non-terminating tail-recursive loop with no
-- effect calls. Used to exercise the hard-abandon cancellation path —
-- pure-compute agents that never cooperate via handler entries cannot
-- be soft-cancelled; tidepool's external `CancelHandle` is the only
-- way to unblock the JIT thread.
--
-- The loop allocates fresh boxed `Int` thunks each iteration (even with
-- bang patterns, `Int` is boxed in Haskell), which guarantees the JIT's
-- GC safepoints fire regularly. `CancelHandle` is observed at
-- `host_fns::gc_trigger` and at `trampoline_resolve` tail-call
-- safepoints, so both allocation-driven and tail-call-driven
-- iterations get a chance to bail.
--
-- `tight_compute.hs` (the strict-fold terminating variant) is retained
-- for a separate scenario — a finite heavy computation — but cannot
-- drive hard-abandon because it terminates before the watchdog's
-- hard-abandon threshold elapses on current hardware.
module InfiniteSpin (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Search
import Pattern.Recall
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log

-- | Non-terminating tail-recursive loop. Non-trivial body so GHC can't
-- constant-fold it away. Returns `()` — unreachable by construction.
spinForever :: Int -> ()
spinForever !n = spinForever (n + 1)

agent :: Eff '[Memory, Search, Recall, Message, Display, Time, Log] ()
agent = do
  let !_ = spinForever 0
  info "unreachable"
