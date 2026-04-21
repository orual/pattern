{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal Diagnostics-only agent for `tests/sdk_diagnostics.rs`.
--
-- Calls `Pattern.Diagnostics.diagnostics` and returns the JSON-encoded
-- result. Effect row is `Eff '[Diagnostics] Text`, matching a single-handler
-- bundle with `DiagnosticsHandler` at position 0.
module DiagnosticsQuery (agent) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import Pattern.Diagnostics

agent :: Eff '[Diagnostics] Text
agent = diagnostics
