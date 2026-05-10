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
