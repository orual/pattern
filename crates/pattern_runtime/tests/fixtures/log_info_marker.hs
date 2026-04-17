{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Agent that emits a structured Log.info event carrying a unique marker
-- string. Used by `tests/time_log_effects.rs::log_info_observable_via_tracing`
-- to assert a tracing subscriber on the Rust side observes the event.
module LogInfoMarker (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Log

agent :: Eff '[Log] ()
agent = info "structured-log-assertion-marker"
