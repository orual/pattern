{-# LANGUAGE GADTs #-}
-- | Pattern.Shell — shell command execution.
--
-- Phase 3: real PTY-backed ProcessManager implementation.
-- See v3-sandbox-io phase_03.md for architecture details.
--
-- Identifier discipline:
--
-- * 'TaskId' is an opaque handle string (UUID-prefix hex). It is unique within
--   a 'ProcessManager''s lifetime and is the value 'Spawn' returns and 'Kill' /
--   'Status' query against. Do NOT treat 'TaskId' as an OS PID — it is
--   recycle-safe and stable across the task's lifetime.
-- * If you need the OS PID (e.g. to use @ps@ / @kill -SIGNAL@ from a separate
--   shell, or to attach a debugger), parse the JSON returned by 'spawn' or
--   'status'. The JSON includes a @pid@ field alongside @task_id@.
module Pattern.Shell where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

type Command = Text

-- | Stable handle for a spawned task. Opaque hex string, NOT an OS PID.
type TaskId = Text

-- | Timeout in seconds for 'Execute'.
type TimeoutSecs = Int

-- | Effect algebra.
--
-- 'Execute' takes an optional timeout. 'Nothing' uses the @SessionContext@
-- default (currently 30 s). On timeout the command is killed (Ctrl-C sent
-- into the PTY, output drained) and the call surfaces an error. If you
-- want a long-running command that streams output asynchronously, use
-- 'Spawn'.
--
-- 'Spawn' returns JSON-encoded @{"task_id":"...","pid":N}@. Save the
-- @task_id@ for 'Kill' / 'Status'; use the @pid@ if you need to interact
-- with the process via OS-level tools.
--
-- 'Status' returns JSON-encoded
-- @[{"task_id":"...","pid":N,"command":"...","elapsed_ms":N},...]@
-- listing all currently-running spawned tasks.
data Shell a where
  Execute :: Command -> Maybe TimeoutSecs -> Shell Text
  Spawn   :: Command -> Shell Text
  Kill    :: TaskId  -> Shell ()
  Status  :: Shell Text

-- | Execute a command with the session-default timeout.
execute :: Member Shell effs => Command -> Eff effs Text
execute c = send (Execute c Nothing)

-- | Execute a command with an explicit timeout in seconds.
executeWith :: Member Shell effs => Command -> TimeoutSecs -> Eff effs Text
executeWith c t = send (Execute c (Just t))

-- | Spawn a long-running command. Returns JSON
-- @{"task_id":"...","pid":N}@. Save the @task_id@ for 'Kill' / 'Status'.
spawn :: Member Shell effs => Command -> Eff effs Text
spawn c = send (Spawn c)

-- | Kill a spawned task by its handle. Returns 'UnknownTask' on stale handles
-- (e.g. the task already exited and was cleaned up).
kill :: Member Shell effs => TaskId -> Eff effs ()
kill tid = send (Kill tid)

-- | List all currently-running spawned tasks. Returns JSON-encoded list of
-- task records. Empty list if no tasks are running.
status :: Member Shell effs => Eff effs Text
status = send Status
