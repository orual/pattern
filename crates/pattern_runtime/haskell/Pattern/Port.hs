-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE GADTs #-}
-- | Pattern.Port — external-service ports (call/subscribe/list).
--
-- Ports are the agent's unified interface to external services. Each port
-- is registered at runtime startup (or plugin load time) and identified by
-- a PortId string. Agents discover available ports via List, call them via
-- Call, and subscribe to event streams via Subscribe.
module Pattern.Port where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

type PortId     = Text
type Method     = Text
type Payload    = Text    -- JSON
type ConfigJson = Text    -- JSON
type PortInfo   = Text    -- JSON: {id, description, version, methods, capabilities}

-- | Effect algebra.
data Port a where
  List        :: Port [PortInfo]
  Call        :: PortId -> Method -> Payload -> Port Payload
  Subscribe   :: PortId -> ConfigJson -> Port ()
  Unsubscribe :: PortId -> Port ()

listPorts :: Member Port effs => Eff effs [PortInfo]
listPorts = send List

call :: Member Port effs => PortId -> Method -> Payload -> Eff effs Payload
call pid m p = send (Call pid m p)

subscribe :: Member Port effs => PortId -> ConfigJson -> Eff effs ()
subscribe pid c = send (Subscribe pid c)

unsubscribe :: Member Port effs => PortId -> Eff effs ()
unsubscribe pid = send (Unsubscribe pid)
