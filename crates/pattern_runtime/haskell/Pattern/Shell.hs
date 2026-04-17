{-# LANGUAGE GADTs #-}
-- | Pattern.Shell — shell command execution.
--
-- Stubbed in Phase 3. runtime handler returns NotImplemented; real
-- implementation will reuse the preserved PTY backend (see
-- `docs/plans/` for the shell-tool / ProcessSource plan).
module Pattern.Shell where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

type Command = Text
type Pid     = Integer

-- | Effect algebra.
data Shell a where
  Execute :: Command -> Shell Text
  Spawn   :: Command -> Shell Pid
  Kill    :: Pid -> Shell ()
  Status  :: Pid -> Shell Text

execute :: Member Shell effs => Command -> Eff effs Text
execute c = send (Execute c)

spawn :: Member Shell effs => Command -> Eff effs Pid
spawn c = send (Spawn c)

kill :: Member Shell effs => Pid -> Eff effs ()
kill p = send (Kill p)

status :: Member Shell effs => Pid -> Eff effs Text
status p = send (Status p)
