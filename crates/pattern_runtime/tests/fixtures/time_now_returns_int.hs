-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE DataKinds, TypeOperators #-}
-- | Agent that returns the raw epoch-nanoseconds Int from Time.now.
-- Used by `tests/time_log_effects.rs::time_now_returns_current_epoch_nanos`
-- to assert the value falls within a ±Δ window around the Rust-side
-- `jiff::Timestamp::now()` readings taken before and after the run.
module TimeNowInt (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Time

agent :: Eff '[Time] Int
agent = nowNanos
