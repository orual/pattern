-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Agent that emits a structured Log.info event carrying a unique marker
-- string. Used by `tests/time_log_effects.rs::log_info_observable_via_tracing`
-- to assert a tracing subscriber on the Rust side observes the event.
module LogInfoMarker (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Log

agent :: Eff '[Log] ()
agent = info "structured-log-assertion-marker"
