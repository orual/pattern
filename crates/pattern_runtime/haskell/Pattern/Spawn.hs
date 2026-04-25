{-# LANGUAGE GADTs #-}
-- | Pattern.Spawn — subagent / child-agent lifecycle.
--
-- Six constructors covering the v3-multi-agent spawn surface:
-- ephemeral workers (non-blocking + await), forks (lightweight in
-- Phase 2; persistent in Phase 3), sibling personas, and stop.
--
-- Configs cross the Haskell/Rust boundary as typed Core values — every
-- record below has a Rust mirror in
-- @crates\/pattern_runtime\/src\/sdk\/requests\/spawn.rs@ that derives
-- @FromCore@ and converts to the corresponding @pattern_core::spawn@
-- domain type. No JSON-over-string crossings.
--
-- Naming notes:
--
-- * @EffectCategory@ ctors carry a @Cat@ prefix to avoid clashing with
--   effect GADT type names imported in the same scope.
-- * @CapabilityFlag@ ctors carry a @Flag@ prefix for the same reason.
-- * @SiblingPersona@ uses @ExistingPersona@ \/ @NewPersona@ to avoid
--   colliding with other modules' constructors.
-- * Record selectors are prefix-disambiguated (@ephemeralProgram@,
--   @forkProgram@, etc.) so multiple records can be in scope without
--   needing @DuplicateRecordFields@.
module Pattern.Spawn where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Opaque handle returned by 'ephemeral'. Pass to 'awaitSpawn' or 'stop'.
type SpawnId = Text

-- | Persona identifier returned by 'sibling'. Same wire shape as the
--   host runtime's @AgentId@.
type PersonaId = Text

-- | JSON-encoded payload returned by 'awaitSpawn' for a completed
--   ephemeral. Phase 2 surfaces this opaquely; Phase 3 Task 8 may add
--   field accessors.
type SpawnResult = Text

-- | JSON-encoded @[Either SpawnError SpawnResult]@ returned by 'awaitAll'.
--   Order matches the input id list. Partial failure is preserved.
type AwaitAllResult = Text

-- | Opaque token referencing a fork. Resolution helpers
--   (@awaitResult@ \/ @mergeBack@ \/ @discard@ \/ @promote@) land in
--   Phase 3 Task 8.
type ForkHandle = Text

-- | Reference to a memory block (label + storage id + owning agent).
data BlockRef = BlockRef
  { blockRefLabel    :: Text
  , blockRefBlockId  :: Text
  , blockRefAgentId  :: Text
  }

-- | Effect category. Mirrors @pattern_core::EffectCategory@. The @Cat@
--   prefix avoids namespace clashes with effect GADT type names.
data EffectCategory
  = CatMemory
  | CatSearch
  | CatRecall
  | CatTasks
  | CatSkills
  | CatMessage
  | CatDisplay
  | CatTime
  | CatLog
  | CatShell
  | CatFile
  | CatSources
  | CatMcp
  | CatRpc
  | CatSpawn
  | CatDiagnostics
  | CatWake

-- | Capability flag. Mirrors @pattern_core::CapabilityFlag@. The @Flag@
--   prefix avoids namespace clashes.
data CapabilityFlag
  = FlagSpawnNewIdentities
  | FlagWakeConditionRegistration
  | FlagFrontingControl

-- | Capability set: which effect categories the holder may invoke and
--   which orthogonal flags it carries.
data CapabilitySet = CapabilitySet
  { capabilityCategories :: [EffectCategory]
  , capabilityFlags      :: [CapabilityFlag]
  }

-- | Memory isolation mode for a forked session.
data ForkIsolation
  = Lightweight   -- ^ in-memory @LoroDoc::fork()@; no disk writes.
  | Persistent    -- ^ jj workspace (Phase 3 only).

-- | Semantic relationship between a spawning persona and a sibling.
data RelationshipKind
  = SupervisorOf
  | SpecialistFor
  | PeerWith
  | ObserverOf

-- | Minimal seed for a new sibling identity. Phase 6 registry work
--   adds more fields; the wire shape stays additive.
data PersonaConfig = PersonaConfig
  { personaName         :: Text
  , personaSystemPrompt :: Text
  , personaCapabilities :: CapabilitySet
  }

-- | Discriminates whether the sibling uses an existing persona id or
--   creates one from a fresh @PersonaConfig@.
data SiblingPersona
  = ExistingPersona PersonaId
  | NewPersona PersonaConfig

-- | Config for an ephemeral child session.
--
-- Lifetime is bounded by the spawning session — when the parent
-- resolves, all ephemeral children are cancelled.
data EphemeralConfig = EphemeralConfig
  { ephemeralProgram      :: Text
  , ephemeralCostume      :: Maybe Text
  , ephemeralCapabilities :: Maybe CapabilitySet
  , ephemeralTimeoutMs    :: Maybe Int
  }

-- | Config for a forked child session.
--
-- @forkIsolation = Lightweight@ is supported in Phase 2; @Persistent@
-- returns a clear "Phase 3" handler error until Phase 3 Task 4 wires
-- the jj workspace path.
data ForkConfig = ForkConfig
  { forkProgram       :: Text
  , forkIsolation     :: ForkIsolation
  , forkCapabilities  :: Maybe CapabilitySet
  , forkTimeoutHintMs :: Maybe Int
  , forkTaskRef       :: Maybe BlockRef
  }

-- | Config for a sibling spawn.
--
-- Unlike ephemeral and fork children, a sibling is not tracked by the
-- spawner's registry — it lives independently of parent lifetime and
-- carries its own @CapabilitySet@.
data SiblingConfig = SiblingConfig
  { siblingPersona      :: SiblingPersona
  , siblingRelationship :: RelationshipKind
  , siblingSharedBlocks :: [Text]
  }

-- | Effect algebra.
data Spawn a where
  Ephemeral  :: EphemeralConfig -> Spawn SpawnId
  AwaitSpawn :: SpawnId -> Spawn SpawnResult
  AwaitAll   :: [SpawnId] -> Spawn AwaitAllResult
  Fork       :: ForkConfig -> Spawn ForkHandle
  Sibling    :: SiblingConfig -> Spawn PersonaId
  Stop       :: SpawnId -> Spawn ()

-- | Spawn an ephemeral worker; returns a 'SpawnId' immediately. The
--   child runs in the background; use 'awaitSpawn' to block on the
--   result.
ephemeral :: Member Spawn effs => EphemeralConfig -> Eff effs SpawnId
ephemeral cfg = send (Ephemeral cfg)

-- | Block until the given ephemeral completes; return its result.
awaitSpawn :: Member Spawn effs => SpawnId -> Eff effs SpawnResult
awaitSpawn sid = send (AwaitSpawn sid)

-- | Await many ephemerals concurrently in a single sync-bridge round
--   trip. Order of results matches the input list; per-id failure is
--   preserved so ensemble \/ voting patterns can inspect partial outcomes.
awaitAll :: Member Spawn effs => [SpawnId] -> Eff effs AwaitAllResult
awaitAll ids = send (AwaitAll ids)

-- | Spawn a fork. Returns an opaque @ForkHandle@; resolution helpers
--   land in Phase 3.
fork :: Member Spawn effs => ForkConfig -> Eff effs ForkHandle
fork cfg = send (Fork cfg)

-- | Spawn a sibling persona. Returns the sibling's 'PersonaId'.
sibling :: Member Spawn effs => SiblingConfig -> Eff effs PersonaId
sibling cfg = send (Sibling cfg)

-- | Cancel an in-flight spawn by id. Idempotent.
stop :: Member Spawn effs => SpawnId -> Eff effs ()
stop sid = send (Stop sid)
