{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Time + Log agent using the Prelude-5 effect list so its tag ordering
-- aligns with `pattern_runtime::sdk::bundle::SdkBundle`
-- (`Memory, Message, Display, Time, Log` prefix of the 11-handler bundle).
module TimeLog (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log

agent :: Eff '[Memory, Message, Display, Time, Log] ()
agent = do
  _t <- now
  info "time+log agent turn"
