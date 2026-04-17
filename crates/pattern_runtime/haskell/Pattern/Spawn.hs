{-# LANGUAGE GADTs #-}
-- | Pattern.Spawn — subagent / child-agent spawning.
--
-- Stubbed in Phase 3. runtime handler returns NotImplemented. Real
-- implementation needs the constellation-runtime orchestrator (future).
module Pattern.Spawn where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

type AgentSpec = Text
type AgentId   = Text

-- | Effect algebra.
data Spawn a where
  Start :: AgentSpec -> Spawn AgentId
  Stop  :: AgentId -> Spawn ()

start :: Member Spawn effs => AgentSpec -> Eff effs AgentId
start spec = send (Start spec)

stop :: Member Spawn effs => AgentId -> Eff effs ()
stop i = send (Stop i)
