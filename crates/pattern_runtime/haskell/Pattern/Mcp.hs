{-# LANGUAGE GADTs #-}
-- | Pattern.Mcp — Model-Context-Protocol tool calls.
--
-- Stubbed in Phase 3. The runtime currently returns NotImplemented. Real
-- implementation lives in the post-foundation plugin-system plan.
--
-- @Use@ rather than @Call@ to avoid colliding with 'Pattern.Rpc.Call'
-- (generic RPC) and to match AI-agent parlance — "the agent uses the
-- search tool".
module Pattern.Mcp where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

type Server = Text
type Method = Text

-- | Effect algebra.
data Mcp a where
  Use :: Server -> Method -> Mcp ()

-- | Use a tool on an MCP server by name.
use :: Member Mcp effs => Server -> Method -> Eff effs ()
use s m = send (Use s m)
