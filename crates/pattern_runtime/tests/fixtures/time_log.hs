-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Time + Log agent using the Prelude-7 effect list so its tag ordering
-- aligns with `pattern_runtime::sdk::bundle::SdkBundle`
-- (`Memory, Search, Recall, Message, Display, Time, Log` prefix of the
-- 13-handler bundle).
module TimeLog (agent) where

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
  _t <- now
  info "time+log agent turn"
