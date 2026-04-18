{-# LANGUAGE GADTs #-}
-- | Pattern.Recall — archival-entry CRUD with optional scope.
--
-- Provides insert\/search\/get\/delete operations over the archival
-- storage backend. Search takes an optional scope ('Maybe Scope');
-- when absent it defaults to the current agent's archival entries.
--
-- Constructor names use the @Recall@-prefix to avoid collisions with
-- @Pattern.Memory@ constructors (@Get@, @Search@, @Archive@).
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
  RecallGet    :: EntryId -> Recall ArchivalContent
  RecallDelete :: EntryId -> Recall ()

-- | Insert a new archival entry, returning its id.
insert :: Member Recall effs => ArchivalContent -> Eff effs EntryId
insert c = send (RecallInsert c)

-- | Search archival entries. Scope defaults to current agent when
-- 'Nothing'.
search :: Member Recall effs => RecallQuery -> Maybe Scope -> Eff effs [ArchivalHit]
search q s = send (RecallSearch q s)

-- | Get a specific archival entry by id.
get :: Member Recall effs => EntryId -> Eff effs ArchivalContent
get i = send (RecallGet i)

-- | Delete an archival entry by id.
delete :: Member Recall effs => EntryId -> Eff effs ()
delete i = send (RecallDelete i)
