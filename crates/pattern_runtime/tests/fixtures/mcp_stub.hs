{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal Mcp-only agent for `tests/stub_effects.rs` — calls
-- `Pattern.Mcp.use`, stubbed in Phase 3.
module McpStub (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Mcp

agent :: Eff '[Mcp] ()
agent = use "server-a" "tool.method"
