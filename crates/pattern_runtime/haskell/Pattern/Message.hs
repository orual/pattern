-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
--
-- v3-multi-agent Phase 4 (Task 5) adds 'Delegate' for task-pinning
-- delegation. The delegated task's block reference is included in the
-- routed message's @block_refs@, causing the recipient's snapshot
-- composer to pin the task into working memory for that turn.
module Pattern.Message where

import Control.Monad.Freer (Eff, Member)
import qualified Control.Monad.Freer as Freer
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

-- | Wire record for 'Delegate'. Carries the task's block reference (label,
-- block-id, agent-id) plus the routing target and message body.
--
-- Record selectors carry the @delegate@ prefix to avoid name collisions
-- when multiple records are in scope without @DuplicateRecordFields@.
data DelegateReq = DelegateReq
  { delegateTaskLabel   :: Text
  -- ^ Human-readable label for the task block (shown in snapshot display).
  , delegateTaskBlockId :: Text
  -- ^ Storage block ID of the task to pin into the recipient's context.
  , delegateTaskAgentId :: Text
  -- ^ Agent ID that owns the task block.
  , delegateRecipient   :: Text
  -- ^ Routing target — typically @"agent:<persona-id>"@.
  , delegateBody        :: Text
  -- ^ Message body sent to the recipient alongside the task pin.
  }

-- | Message effect algebra.
data Message a where
  Ask      :: Request -> Message (MessageContent, Usage)
  Send     :: Recipient -> Body -> Message ()
  Reply    :: MessageId -> Body -> Message ()
  Notify   :: ChannelId -> Body -> Message ()
  -- | Delegate a task to another agent.
  --
  -- The runtime constructs a message to 'delegateRecipient' with
  -- 'delegateBody' as the body text and the task referenced by
  -- 'delegateTaskBlockId' pinned into @block_refs@. The recipient's
  -- snapshot composer then includes the task block in working memory
  -- for that turn (AC6.3).
  Delegate :: DelegateReq -> Message ()

ask :: Member Message effs => Request -> Eff effs (MessageContent, Usage)
ask r = Freer.send (Ask r)

-- | Send a message to another agent or endpoint. Caller identity is
-- attached by the runtime from the session's agent_id; agents only
-- specify the recipient.
send :: Member Message effs => Recipient -> Body -> Eff effs ()
send r b = Freer.send (Send r b)

reply :: Member Message effs => MessageId -> Body -> Eff effs ()
reply m b = Freer.send (Reply m b)

notify :: Member Message effs => ChannelId -> Body -> Eff effs ()
notify c b = Freer.send (Notify c b)

-- | Delegate a task to another agent.
--
-- Constructs a task-pinning message: the task's block reference is embedded
-- in @block_refs@ of the outgoing message so the recipient's snapshot
-- composer pins it into working memory for the incoming turn.
--
-- Usage:
--
-- @
-- delegate (DelegateReq
--   { delegateTaskLabel   = "clean-up-backlog"
--   , delegateTaskBlockId = taskRef
--   , delegateTaskAgentId = myAgentId
--   , delegateRecipient   = "agent:worker-persona"
--   , delegateBody        = "please handle the backlog task"
--   })
-- @
delegate :: Member Message effs => DelegateReq -> Eff effs ()
delegate d = Freer.send (Delegate d)
