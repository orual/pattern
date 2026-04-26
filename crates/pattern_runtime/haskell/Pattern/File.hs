{-# LANGUAGE GADTs #-}
-- | Pattern.File — filesystem access (sandboxed).
--
-- Phase 2 (v3-sandbox-io) expanded the algebra:
--   * 'ListDir' gained a 'GlobPattern' argument (empty means \"*\").
--   * 'Open' / 'Close' / 'Watch' added for LoroSyncedFile lifecycle.
--   * 'Reload' / 'ForceWrite' added as agent recourse on 'FileConflict'
--     system reminders from stale-base external writes (Phase 1 amendment).
--
-- 'ListDir' is named to avoid colliding with 'Pattern.Sources.List'.
-- 'read' uses qualified import ('File.read') to avoid shadowing 'Prelude.read'.
module Pattern.File where

import Control.Monad.Freer (Eff, Member)
import qualified Control.Monad.Freer as Freer
import Data.Text (Text)

type Path        = Text
type Content     = Text
-- | Empty glob is treated as @"*"@ (match all entries).
type GlobPattern = Text
-- | JSON-encoded file metadata: @{path:Path, size:Int, mtime:Text, is_dir:Bool}@.
type FileInfo    = Text

-- | File effect algebra.
data File a where
  Read       :: Path -> File Content
  Write      :: Path -> Content -> File ()
  -- | List directory entries. Empty 'GlobPattern' means @"*"@.
  ListDir    :: Path -> GlobPattern -> File [FileInfo]
  -- | Open a file, creating a LoroSyncedFile and auto-subscribing to
  -- external change notifications. Returns current file content.
  Open       :: Path -> File Content
  -- | Close an open file, dropping its LoroSyncedFile and unsubscribing
  -- from change notifications.
  Close      :: Path -> File ()
  -- | Subscribe to change notifications without creating a LoroSyncedFile
  -- (lighter weight than 'Open').
  Watch      :: Path -> File ()
  -- | Drop the in-memory doc state and reload from disk. Returns the reloaded
  -- content. Use after a FileConflict reminder to accept the external version.
  Reload     :: Path -> File Content
  -- | Write through to disk, bypassing ConflictPolicy. Use after a
  -- FileConflict reminder to overwrite with the agent's version.
  ForceWrite :: Path -> Content -> File ()

read :: Member File effs => Path -> Eff effs Content
read p = Freer.send (Read p)

write :: Member File effs => Path -> Content -> Eff effs ()
write p c = Freer.send (Write p c)

-- | List entries of a directory matching the given glob pattern.
-- Pass an empty string to match all entries.
listDir :: Member File effs => Path -> GlobPattern -> Eff effs [FileInfo]
listDir p g = Freer.send (ListDir p g)

-- | Open a file for tracked editing with change notifications.
open :: Member File effs => Path -> Eff effs Content
open p = Freer.send (Open p)

-- | Close a previously opened file.
close :: Member File effs => Path -> Eff effs ()
close p = Freer.send (Close p)

-- | Subscribe to change notifications for a path (no LoroSyncedFile created).
watch :: Member File effs => Path -> Eff effs ()
watch p = Freer.send (Watch p)

-- | Reload file from disk, discarding in-memory doc state.
reload :: Member File effs => Path -> Eff effs Content
reload p = Freer.send (Reload p)

-- | Force-write content to disk, bypassing conflict policy.
forceWrite :: Member File effs => Path -> Content -> Eff effs ()
forceWrite p c = Freer.send (ForceWrite p c)
