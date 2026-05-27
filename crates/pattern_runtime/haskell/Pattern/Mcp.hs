-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
  Call        :: Server -> Method -> Payload -> Mcp Text
  Introspect  :: Server -> Mcp Text
  ListServers :: Mcp Text
  Unload      :: Server -> Mcp ()

-- | Call a tool on an MCP server. Args is a JSON string.
call :: Member Mcp effs => Server -> Method -> Payload -> Eff effs Text
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
