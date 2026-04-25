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
    -- | Optional initial human-role prompt seeded into the child's first
    --   turn. @Nothing@ leaves the child to open on @costume@/system-prompt
    --   alone with no human turn.
  , ephemeralPrompt       :: Maybe Text
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

-- | Typed handle returned by 'sibling'. Each constructor encodes a distinct
--   outcome so agents can pattern-match without inspecting optional fields:
--
--   * 'SiblingExistingActive' — an existing registered persona was adopted.
--     Always authorised for live session-open (Phase 6). No draft KDL path.
--   * 'SiblingNewActive'      — a new identity was minted AND the parent held
--     @SpawnNewIdentities@; Phase 6 promotes to a live session. Carries the
--     on-disk KDL draft path.
--   * 'SiblingNewDraft'       — a new identity was minted but the parent lacked
--     @SpawnNewIdentities@; pending human-driven promote. Carries the on-disk
--     KDL draft path.
--
--   Mirrors @WireSiblingSpawn@ in @crates\/pattern_runtime\/src\/sdk\/requests\/spawn.rs@.
--
--   The old flat-record shape permitted meaningless states such as
--   @SiblingActive@ with a @kdlPath = Just _@ (existing adoptions have no
--   draft file) or @SiblingDraft@ with @kdlPath = Nothing@ (drafts always
--   have a path). The sum type makes those impossible to construct.
data SiblingSpawn
  = SiblingExistingActive PersonaId
    -- ^ Existing persona adopted; always authorised for live session-open.
  | SiblingNewActive PersonaId Text
    -- ^ New persona minted and authorised (parent held SpawnNewIdentities).
    --   Second field is the on-disk KDL draft path.
  | SiblingNewDraft PersonaId Text
    -- ^ New persona minted but pending human-driven promote.
    --   Second field is the on-disk KDL draft path.

-- ── Result types ──────────────────────────────────────────────────────────────

-- | Typed handle returned by 'ephemeral'. Pairs the spawn id with the
--   constellation-scoped progress-log block label so the parent can read
--   live progress without waiting for the child to complete.
--
--   Mirrors @WireEphemeralSpawn@ in @crates\/pattern_runtime\/src\/sdk\/requests\/spawn.rs@.
data EphemeralSpawn = EphemeralSpawn
  { ephemeralSpawnId       :: SpawnId
  , ephemeralSpawnLogLabel :: Text
  }

-- | Why an ephemeral child stopped running.
--
--   Mirrors @WireTerminationReason@. The @Term@ prefix avoids clashing
--   with other constructors.
data TerminationReason
  = TermEndTurn    -- ^ Model produced final text (normal completion).
  | TermToolUse    -- ^ Stopped at a tool boundary.
  | TermMaxTurns   -- ^ Hit the per-ephemeral max-turns ceiling.
  | TermTimeout    -- ^ Exceeded the configured timeout.
  | TermCancelled  -- ^ Parent cancelled the child.
  | TermError      -- ^ Child failed with a runtime error.

-- | Result returned when a child session completes.
--
--   Mirrors @WireSpawnResult@.
data SpawnResult = SpawnResult
  { spawnResultChildId          :: SpawnId
  , spawnResultFinalText        :: Maybe Text
  , spawnResultTurns            :: Int
  , spawnResultTerminated       :: TerminationReason
  , spawnResultProgressLogLabel :: Maybe Text
  }

-- | Per-id outcome from 'awaitAll'. Partial failure is preserved so
--   ensemble \/ voting patterns can inspect individual results.
--
--   Mirrors @WireSpawnAwaitOutcome@.
data SpawnAwaitOutcome
  = SpawnOk SpawnResult
  | SpawnFail Text

-- | Typed handle referencing an in-progress fork.
--
--   Phase 2: scaffold only; @forkHandleId@ and @forkHandleChildId@ are
--   generated ids but no computation is running. Resolution helpers
--   ('mergeBack', 'discardFork', 'promoteFork') landed in Phase 3 Task 8.3.
--
--   Mirrors @WireForkHandle@.
data ForkHandle = ForkHandle
  { forkHandleId      :: SpawnId
  , forkHandleChildId :: SpawnId
  }

-- | Operation to perform on a fork. Passed as the second argument to
--   'ForkOp'.
--
--   Three resolution paths (no @AwaitResult@ — lightweight forks are
--   memory snapshots, not running sessions; there is nothing to await):
--
--   * 'ForkOpMergeBack' — import the fork's CRDT state into the parent.
--     The handle STAYS in the registry after merge so callers may merge
--     multiple times or follow up with 'ForkOpDiscard'.
--   * 'ForkOpDiscard'   — drop the fork without propagating. Handle is
--     REMOVED from the registry.
--   * 'ForkOpPromote'   — mint a draft persona from the fork's memory
--     state. Handle is REMOVED. Requires @SpawnNewIdentities@ capability
--     on the spawner's snapshot.
--
--   The @ForkOp@ constructor prefix mirrors the @Cat@\/@Flag@ convention:
--   it prevents namespace collisions with other constructors imported in
--   the same scope.
--
--   Mirrors @WireForkOpKind@ in
--   @crates\/pattern_runtime\/src\/sdk\/requests\/spawn.rs@.
data ForkOpKind
  = ForkOpMergeBack
    -- ^ Import the fork's CRDT state into the parent; handle stays in
    --   registry.
  | ForkOpDiscard
    -- ^ Drop the fork without propagating; handle removed from registry.
  | ForkOpPromote PersonaConfig
    -- ^ Promote the fork to a draft persona; handle removed; requires
    --   @SpawnNewIdentities@.

-- | Result returned by 'ForkOp'. Each constructor corresponds to a
--   distinct outcome:
--
--   * 'ForkOpUnit'        — @Discard@ succeeded; no payload.
--   * 'ForkOpMergeReport' — @MergeBack@ succeeded; payload is an opaque
--     text summary of the merge. Phase 7+ may add structured accessors.
--   * 'ForkOpPersonaId'   — @Promote@ succeeded; payload is the new
--     persona id (same shape as 'PersonaId').
--
--   Mirrors @WireForkOpResult@ in
--   @crates\/pattern_runtime\/src\/sdk\/requests\/spawn.rs@.
data ForkOpResult
  = ForkOpUnit
    -- ^ Returned by 'ForkOpDiscard'.
  | ForkOpMergeReport Text
    -- ^ Returned by 'ForkOpMergeBack'. Opaque text summary.
  | ForkOpPersonaId PersonaId
    -- ^ Returned by 'ForkOpPromote'. The newly-minted persona id.

-- ── Effect algebra ────────────────────────────────────────────────────────────

-- | Effect algebra.
data Spawn a where
  Ephemeral  :: EphemeralConfig -> Spawn EphemeralSpawn
  AwaitSpawn :: SpawnId -> Spawn SpawnResult
  AwaitAll   :: [SpawnId] -> Spawn [SpawnAwaitOutcome]
  Fork       :: ForkConfig -> Spawn ForkHandle
  Sibling    :: SiblingConfig -> Spawn SiblingSpawn
  Stop       :: SpawnId -> Spawn ()
  ForkOp     :: SpawnId -> ForkOpKind -> Spawn ForkOpResult

-- ── Helpers ───────────────────────────────────────────────────────────────────

-- | Spawn an ephemeral worker; returns an 'EphemeralSpawn' immediately.
--   The child runs in the background; use 'awaitSpawn' to block on the
--   result.
ephemeral :: Member Spawn effs => EphemeralConfig -> Eff effs EphemeralSpawn
ephemeral cfg = send (Ephemeral cfg)

-- | Block until the given ephemeral completes; return its 'SpawnResult'.
awaitSpawn :: Member Spawn effs => SpawnId -> Eff effs SpawnResult
awaitSpawn sid = send (AwaitSpawn sid)

-- | Await many ephemerals concurrently in a single sync-bridge round
--   trip. Order of results matches the input list; per-id failure is
--   preserved so ensemble \/ voting patterns can inspect partial outcomes.
awaitAll :: Member Spawn effs => [SpawnId] -> Eff effs [SpawnAwaitOutcome]
awaitAll ids = send (AwaitAll ids)

-- | Spawn a fork. Returns a typed 'ForkHandle'; use 'mergeBack',
--   'discardFork', or 'promoteFork' to resolve.
fork :: Member Spawn effs => ForkConfig -> Eff effs ForkHandle
fork cfg = send (Fork cfg)

-- | Spawn a sibling persona. Returns the sibling's 'PersonaId'.
sibling :: Member Spawn effs => SiblingConfig -> Eff effs SiblingSpawn
sibling cfg = send (Sibling cfg)

-- | Cancel an in-flight spawn by id. Idempotent.
stop :: Member Spawn effs => SpawnId -> Eff effs ()
stop sid = send (Stop sid)

-- | Import the fork's CRDT state into the parent.
--
--   The fork handle STAYS in the registry after merge so callers may
--   continue operating on it (e.g. merge again, then 'discardFork').
--   Use 'forkHandleId' to obtain the 'SpawnId' from a 'ForkHandle'.
mergeBack :: Member Spawn effs => SpawnId -> Eff effs ForkOpResult
mergeBack fid = send (ForkOp fid ForkOpMergeBack)

-- | Drop the fork without propagating its state to the parent.
--
--   The handle is REMOVED from the registry. The name @discardFork@
--   avoids a collision with the @stop@ helper (which cancels in-flight
--   ephemeral spawns, a distinct concept).
discardFork :: Member Spawn effs => SpawnId -> Eff effs ForkOpResult
discardFork fid = send (ForkOp fid ForkOpDiscard)

-- | Promote the fork to a draft persona identity.
--
--   The handle is REMOVED from the registry. The spawner must hold the
--   @SpawnNewIdentities@ capability flag or the handler returns a
--   capability-denied error. The @PersonaId@ in the result can be used
--   to reference the new draft in subsequent 'Sibling' spawn calls.
promoteFork :: Member Spawn effs => SpawnId -> PersonaConfig -> Eff effs ForkOpResult
promoteFork fid cfg = send (ForkOp fid (ForkOpPromote cfg))
