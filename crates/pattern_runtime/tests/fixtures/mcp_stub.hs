-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal Mcp-only agent for `tests/stub_effects.rs` — calls
-- `Pattern.Mcp.use`, stubbed in Phase 3.
module McpStub (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Mcp

agent :: Eff '[Mcp] ()
agent = use "server-a" "tool.method"
