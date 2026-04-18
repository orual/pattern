{-# LANGUAGE GADTs #-}
-- | Pattern.Log — agent-facing structured logging.
--
-- Fully implemented in Phase 3. The runtime handler `LogHandler` routes each
-- variant through `tracing` at the matching level with structured
-- @session@ and @source@ fields so tests / telemetry / CLI can observe
-- agent-originated log events.
module Pattern.Log where

import Control.Monad.Freer (Eff, Member)
import qualified Control.Monad.Freer as Freer
import Data.Text (Text)

-- | Effect algebra.
data Log a where
  Debug :: Text -> Log ()
  Info  :: Text -> Log ()
  Warn  :: Text -> Log ()
  Error :: Text -> Log ()

debug :: Member Log effs => Text -> Eff effs ()
debug msg = Freer.send (Debug msg)

info :: Member Log effs => Text -> Eff effs ()
info msg = Freer.send (Info msg)

warn :: Member Log effs => Text -> Eff effs ()
warn msg = Freer.send (Warn msg)

error :: Member Log effs => Text -> Eff effs ()
error msg = Freer.send (Error msg)
