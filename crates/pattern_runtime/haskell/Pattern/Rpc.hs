{-# LANGUAGE GADTs #-}
-- | Pattern.Rpc — remote procedure calls to external services and other
-- processes. Covers the general RPC shape (sync request/response) and
-- degenerates to simple IPC when combined with 'Recv' for mailbox patterns.
--
-- This is NOT for agent-to-agent communication — that's
-- 'Pattern.Message.Send' (which carries the agent's own identity from
-- session context automatically).
--
-- Stubbed in Phase 3. Real implementation in a later phase will route
-- to local sockets / HTTP / whatever the target protocol demands.
module Pattern.Rpc where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | RPC target descriptor. Shape firms up in a later phase;
-- scheme-prefixed strings like @"unix:/run/foo.sock"@,
-- @"http://localhost:8080/rpc"@, or @"proc:some-daemon"@.
type Target = Text

-- | Request / response payload (serialised JSON / bytes / whatever the
-- target protocol expects).
type Payload = Text

-- | Rpc effect algebra.
--
-- @Call@ is named to avoid colliding with 'Pattern.Message.Send' (agent
-- messaging is a distinct concern — see module docs).
data Rpc a where
  Call :: Target -> Payload -> Rpc Payload
  Recv :: Target -> Rpc Payload

-- | Synchronous request/response to an external service.
call :: Member Rpc effs => Target -> Payload -> Eff effs Payload
call t p = send (Call t p)

-- | Passive receive from a target channel/endpoint.
recv :: Member Rpc effs => Target -> Eff effs Payload
recv t = send (Recv t)
