{-# LANGUAGE GADTs #-}
-- | Pattern.Mcp — Model-Context-Protocol tool calls.
--
-- Four operations for interacting with MCP servers:
-- Call (invoke a tool), Introspect (list tools on a server),
-- ListServers (list connected servers), Unload (disconnect).
module Pattern.Mcp where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Pattern.Aeson (Value)

type Server = Text
type Method = Text
type Payload = Text

-- | Effect algebra.
data Mcp a where
  Call        :: Server -> Method -> Payload -> Mcp Value
  Introspect  :: Server -> Mcp Text
  ListServers :: Mcp Text
  Unload      :: Server -> Mcp ()

-- | Call a tool on an MCP server. Args is a JSON string.
call :: Member Mcp effs => Server -> Method -> Payload -> Eff effs Value
call s m args = send (Call s m args)

-- | Get tool metadata for a server (returns JSON text).
introspect :: Member Mcp effs => Server -> Eff effs Text
introspect s = send (Introspect s)

-- | List all connected MCP servers (returns JSON text).
listServers :: Member Mcp effs => Eff effs Text
listServers = send ListServers

-- | Disconnect an MCP server.
unload :: Member Mcp effs => Server -> Eff effs ()
unload s = send (Unload s)
