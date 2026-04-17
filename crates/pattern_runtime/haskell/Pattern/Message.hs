{-# LANGUAGE GADTs #-}
-- | Pattern.Message — provider / inter-agent messaging.
--
-- Stubbed in Phase 3. `Ask` returns post-streaming @(MessageContent, Usage)@
-- at the agent level; the Rust handler (Phase 4) orchestrates the provider
-- stream internally and forwards incremental chunks through Pattern.Display.
module Pattern.Message where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Agent-supplied request payload (JSON-ish; shape stabilises in Phase 4).
type Request = Text

-- | Assembled response content for the agent.
type MessageContent = Text

-- | Token / call usage metadata returned alongside content.
type Usage = Text

-- | Caller identity for outbound sends.
type Caller = Text

-- | Message body (e.g. reply text).
type Body = Text

-- | Reference to a prior message.
type MessageId = Text

-- | Coordination channel identifier.
type ChannelId = Text

-- | Message effect algebra. Variant names are mirrored by
-- @Pattern.sdk::requests::message::MessageReq@ (Rust).
data Message a where
  Ask    :: Request -> Message (MessageContent, Usage)
  Send   :: Caller -> Body -> Message ()
  Reply  :: MessageId -> Body -> Message ()
  Notify :: ChannelId -> Body -> Message ()

ask :: Member Message effs => Request -> Eff effs (MessageContent, Usage)
ask r = send (Ask r)

send_ :: Member Message effs => Caller -> Body -> Eff effs ()
send_ c b = send (Send c b)

reply :: Member Message effs => MessageId -> Body -> Eff effs ()
reply m b = send (Reply m b)

notify :: Member Message effs => ChannelId -> Body -> Eff effs ()
notify c b = send (Notify c b)
