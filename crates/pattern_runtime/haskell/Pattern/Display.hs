{-# LANGUAGE GADTs #-}
-- | Pattern.Display — one-way broadcast of observable agent output to
-- UX / CLI / telemetry surfaces.
--
-- Fully implemented in Phase 3 (broadcast to registered subscribers).
--
-- From the agent's perspective this is fire-and-forget: the agent emits
-- `Chunk` / `Final` / `Note` envelopes describing what the human / UX
-- layer should see. Subscribers are registered host-side; see the
-- `DisplayHandler` in @pattern_runtime::sdk::handlers::display@.
--
-- Typical flow: the runtime handler `MessageHandler` forwards streaming provider
-- chunks through `Display.Chunk` in real time, then emits `Display.Final`
-- with the assembled content. Agent Haskell programs that want to react
-- mid-stream should register a Display subscriber rather than attempting
-- to iterate chunks in Haskell.
module Pattern.Display where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Effect algebra.
data Display a where
  -- | Incremental chunk during a streaming provider response.
  Chunk :: Text -> Display ()
  -- | Terminal assembled content for the turn's Message.Ask. Fires once.
  Final :: Text -> Display ()
  -- | Agent-visible note (typing indicator, tool-call progress, etc.).
  Note  :: Text -> Display ()

chunk :: Member Display effs => Text -> Eff effs ()
chunk t = send (Chunk t)

final :: Member Display effs => Text -> Eff effs ()
final t = send (Final t)

note :: Member Display effs => Text -> Eff effs ()
note t = send (Note t)
