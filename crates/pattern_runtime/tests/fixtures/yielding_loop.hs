{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Yielding-loop agent: calls `now` in a tight loop so the soft-cancel
-- path (watchdog flips the flag; next effect returns the sentinel)
-- exercises cleanly. The loop size is large enough that without
-- cancellation it would run longer than any reasonable test budget.
module YieldingLoop (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log

loop_ :: Int -> Eff '[Memory, Message, Display, Time, Log] ()
loop_ 0 = pure ()
loop_ n = do
  _ <- now
  loop_ (n - 1)

agent :: Eff '[Memory, Message, Display, Time, Log] ()
agent = loop_ 1000000
