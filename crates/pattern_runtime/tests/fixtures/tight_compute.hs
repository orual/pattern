{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings, BangPatterns #-}
-- | Tight-compute agent: a strict accumulator loop sized to run longer
-- than the hard-abandon budget without allocating heap (so the JIT's
-- bounded nursery doesn't turn this into a `RuntimeCrashed` via
-- `HeapOverflow` rather than the targeted `HardAbandon` outcome).
module TightCompute (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Search
import Pattern.Recall
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log

-- | Strict tight loop with a non-trivial body so GHC can't constant-fold
-- it away. Returns sum of i*i for i = n .. 1 accumulated strictly.
tightSum :: Int -> Int -> Int
tightSum !acc 0 = acc
tightSum !acc n = tightSum (acc + n * n) (n - 1)

agent :: Eff '[Memory, Search, Recall, Message, Display, Time, Log] ()
agent = do
  let !_ = tightSum 0 200000000
  info "done"
