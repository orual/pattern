{-# LANGUAGE GADTs #-}
{-# LANGUAGE DuplicateRecordFields #-}
-- | Pattern.Constellation — read-only access to the constellation persona registry.
--
-- Agents can list persona records, find by project + relationship-kind filters,
-- and list groups. Mutation paths (register, set_status, add_relationship,
-- create_group) are daemon-level RPCs and are NOT exposed here.
module Pattern.Constellation where

import Control.Monad.Freer (Eff, Member)
import qualified Control.Monad.Freer as Freer
import Data.Text (Text)

-- ── Domain types ─────────────────────────────────────────────────────────────

-- | Lifecycle state of a persona in the registry.
data PersonaStatus
  = PersonaActive
  | PersonaDraft
  | PersonaInactive

-- | Semantic label for a persona-to-persona relationship.
--
-- The @Rel@ prefix avoids collision with the @Pattern.Spawn.RelationshipKind@
-- constructors (which use the bare names).
data RelationshipKind
  = RelSupervisorOf
  | RelSpecialistFor
  | RelPeerWith
  | RelObserverOf

-- | Direction of a relationship edge relative to the owning persona.
data EdgeDirection
  = DirOutgoing
  | DirIncoming

-- | A directed relationship edge as observed from one persona's record.
data RelationshipEdge = RelationshipEdge
  { other     :: Text
  , kind      :: RelationshipKind
  , direction :: EdgeDirection
  }

-- | A persona registry record.
--
-- Paths are 'Text' on the wire (host renders 'PathBuf' to lossy UTF-8).
-- The first field is named @personaId@ (not @id@) to mirror the Rust wire
-- type, which renames it to avoid a derive-macro local shadowing.
data PersonaRecord = PersonaRecord
  { personaId          :: Text
  , name               :: Text
  , status             :: PersonaStatus
  , configPath         :: Maybe Text
  , projectAttachments :: [Text]
  , relationships      :: [RelationshipEdge]
  , groupMemberships   :: [Text]
  }

-- | A named group of personas. Groups are organisational only — they do not
-- gate cross-agent search or any other permission decision.
--
-- @groupId@ rather than @id@ for the same reason as 'PersonaRecord.personaId'.
data PersonaGroup = PersonaGroup
  { groupId   :: Text
  , name      :: Text
  , projectId :: Maybe Text
  , members   :: [Text]
  }

-- ── Effect algebra ───────────────────────────────────────────────────────────

-- | Read-only effect for the constellation registry.
--
-- 'List' returns every persona registered in the constellation. The optional
-- argument is a project-path filter: 'Nothing' returns all, @Just path@ returns
-- only personas attached to that project directory.
--
-- 'Find' takes an optional project filter and an optional relationship-kind
-- filter (snake_case identifier: @"supervisor_of"@, @"specialist_for"@,
-- @"peer_with"@, @"observer_of"@). Both filters AND together when set.
--
-- 'Groups' returns the persona groups, optionally filtered to those scoped to
-- a particular project path.
data Constellation a where
  List   :: Maybe Text -> Constellation [PersonaRecord]
  Find   :: Maybe Text -> Maybe Text -> Constellation [PersonaRecord]
  Groups :: Maybe Text -> Constellation [PersonaGroup]

-- | List persona records, optionally filtered to a project directory.
list :: Member Constellation effs => Maybe Text -> Eff effs [PersonaRecord]
list scope = Freer.send (List scope)

-- | Find persona records matching the given project and relationship-kind filters.
find
  :: Member Constellation effs
  => Maybe Text -- ^ project path filter
  -> Maybe Text -- ^ relationship-kind identifier (snake_case)
  -> Eff effs [PersonaRecord]
find project kindId = Freer.send (Find project kindId)

-- | List persona groups, optionally filtered to a project directory.
groups :: Member Constellation effs => Maybe Text -> Eff effs [PersonaGroup]
groups scope = Freer.send (Groups scope)
