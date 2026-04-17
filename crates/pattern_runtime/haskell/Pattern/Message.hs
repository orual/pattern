{-# LANGUAGE GADTs #-}
-- | Pattern.Message — provider / inter-agent / outbound messaging.
--
-- Stubbed in Phase 3. 'Ask' returns post-streaming @(MessageContent, Usage)@
-- at the agent level; the runtime (Phase 4) orchestrates the provider
-- stream internally and forwards incremental chunks through 'Pattern.Display'.
--
-- The caller's identity (agent_id) is attached automatically by the runtime
-- from the active session — agents specify who they're TALKING TO, not who
-- they are. 'Recipient' is a flexible address (other agent, group, discord
-- channel, bluesky handle, cli, etc.) that the runtime parses.
module Pattern.Message where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Agent-supplied request payload (JSON-ish; shape stabilises in Phase 4).
type Request = Text

-- | Assembled response content for the agent.
type MessageContent = Text

-- | Token / call usage metadata returned alongside content.
type Usage = Text

-- | Endpoint descriptor for outbound sends. Scheme-prefixed string such as
-- @"agent:pattern-entropy"@, @"group:constellation-1"@,
-- @"discord:#general"@, or @"bluesky:did:plc:..."@. Shape firmed up by
-- the router in Phase 4.
type Recipient = Text

-- | Message body (e.g. reply text).
type Body = Text

-- | Reference to a prior message.
type MessageId = Text

-- | Coordination channel identifier.
type ChannelId = Text

-- | Message effect algebra.
data Message a where
  Ask    :: Request -> Message (MessageContent, Usage)
  Send   :: Recipient -> Body -> Message ()
  Reply  :: MessageId -> Body -> Message ()
  Notify :: ChannelId -> Body -> Message ()

ask :: Member Message effs => Request -> Eff effs (MessageContent, Usage)
ask r = send (Ask r)

-- | Send a message to another agent or endpoint. Caller identity is
-- attached by the runtime from the session's agent_id; agents only
-- specify the recipient.
send_ :: Member Message effs => Recipient -> Body -> Eff effs ()
send_ r b = send (Send r b)

reply :: Member Message effs => MessageId -> Body -> Eff effs ()
reply m b = send (Reply m b)

notify :: Member Message effs => ChannelId -> Body -> Eff effs ()
notify c b = send (Notify c b)
