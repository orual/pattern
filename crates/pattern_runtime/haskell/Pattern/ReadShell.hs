{-# LANGUAGE GADTs #-}
-- | Pattern.ReadShell — allow-listed shell command execution.
module Pattern.ReadShell where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

type Command = Text

-- | Timeout in seconds for 'Execute'.
type TimeoutSecs = Int

-- | Effect algebra.
--
-- 'Execute' takes an optional timeout. 'Nothing' uses the @SessionContext@
-- default (currently 30 s). On timeout the command is killed (Ctrl-C sent
-- into the PTY, output drained) and the call surfaces an error. If you
-- want a long-running command that streams output asynchronously, use
-- 'Spawn'.
data ReadShell a where
  Execute :: Command -> Maybe TimeoutSecs -> Shell Text

-- | Execute a command with the session-default timeout.
execute :: Member ReadShell effs => Command -> Eff effs Text
execute c = send (Execute c Nothing)
