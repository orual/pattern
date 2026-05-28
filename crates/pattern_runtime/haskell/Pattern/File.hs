-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE GADTs #-}
-- | Pattern.File — filesystem access (sandboxed).
--
-- Phase 2 (v3-sandbox-io) expanded the algebra:
--   * 'ListDir' gained a 'GlobPattern' argument (empty means \"*\").
--   * 'Open' / 'Close' / 'Watch' added for LoroSyncedFile lifecycle.
--   * 'Reload' / 'ForceWrite' added as agent recourse on 'FileConflict'
--     system reminders from stale-base external writes (Phase 1 amendment).
--
-- 'ListDir' is named to avoid colliding with a generic 'List' constructor in
-- other effect modules. 'read' uses qualified import ('File.read') to avoid
-- shadowing 'Prelude.read'.
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
  -- | Insert content after line @n@ (1-indexed). Line 0 inserts at the top.
  InsertLines :: Path -> Int -> Content -> File ()
  -- | Replace lines @from@..@to@ (1-indexed, inclusive) with new content.
  ReplaceLines :: Path -> Int -> Int -> Content -> File ()
  -- | Delete lines @from@..@to@ (1-indexed, inclusive).
  DeleteLines :: Path -> Int -> Int -> File ()
  -- | Read lines @from@..@to@ (1-indexed, inclusive).
  ReadLines  :: Path -> Int -> Int -> File Content
  -- | Find and replace a string in a file. Returns count of replacements.
  Replace    :: Path -> Text -> Text -> File Text

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

-- | Insert content after line @n@ (1-indexed). Line 0 inserts at the top.
insertLines :: Member File effs => Path -> Int -> Content -> Eff effs ()
insertLines p n c = Freer.send (InsertLines p n c)

-- | Replace lines @from@..@to@ (1-indexed, inclusive) with new content.
replaceLines :: Member File effs => Path -> Int -> Int -> Content -> Eff effs ()
replaceLines p from to c = Freer.send (ReplaceLines p from to c)

-- | Delete lines @from@..@to@ (1-indexed, inclusive).
deleteLines :: Member File effs => Path -> Int -> Int -> Eff effs ()
deleteLines p from to = Freer.send (DeleteLines p from to)

-- | Read lines @from@..@to@ (1-indexed, inclusive).
readLines :: Member File effs => Path -> Int -> Int -> Eff effs Content
readLines p from to = Freer.send (ReadLines p from to)

-- | Find and replace a string in a file. Returns count of replacements.
replace :: Member File effs => Path -> Text -> Text -> Eff effs Text
replace p find repl = Freer.send (Replace p find repl)
