{-# LANGUAGE GADTs #-}
-- | Pattern.Recall — archival-entry CRUD with optional scope.
--
-- Provides insert\/search\/get operations over the archival storage
-- backend. Search takes an optional scope ('Maybe Scope'); when
-- absent it defaults to the current agent's archival entries.
--
-- Constructor names use the @Recall@-prefix to avoid collisions with
-- @Pattern.Memory@ constructors (@Get@, @Search@).
--
-- Note: @RecallDelete@ / @delete@ were removed in v3-memory-rework
-- Phase 3 (AC4.9). 'MemoryStore::delete_archival' is retained on the
-- Rust side for human-operator tooling (CLI / TUI) but is not
-- reachable via the agent SDK.
module Pattern.Recall where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Archival content payload.
type ArchivalContent = Text

-- | Archival entry identifier (returned by 'RecallInsert').
type EntryId = Text

-- | Search query string.
type RecallQuery = Text

-- | Search scope — same scheme as 'Pattern.Search.Scope'.
type Scope = Text

-- | Archival result entry (structured by the runtime).
type ArchivalHit = Text

-- | Recall effect algebra.
data Recall a where
  RecallInsert :: ArchivalContent -> Recall EntryId
  RecallSearch :: RecallQuery -> Maybe Scope -> Recall [ArchivalHit]

-- | Insert a new archival entry, returning its id.
insert :: Member Recall effs => ArchivalContent -> Eff effs EntryId
insert c = send (RecallInsert c)

-- | Search archival entries. Scope defaults to current agent when
-- 'Nothing'.
search :: Member Recall effs => RecallQuery -> Maybe Scope -> Eff effs [ArchivalHit]
search q s = send (RecallSearch q s)
