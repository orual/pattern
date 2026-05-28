-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
