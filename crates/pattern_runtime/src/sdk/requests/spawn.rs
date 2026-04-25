//! Mirror of `Pattern.Spawn` (`haskell/Pattern/Spawn.hs`).
//!
//! Configs cross the Haskell/Rust boundary as typed Core values — each
//! wire struct derives [`FromCore`] and converts to the corresponding
//! `pattern_core::spawn` domain type via a `From<Wire*>` impl. This keeps
//! `pattern_core` free of any Tidepool-VM dependency while delivering a
//! fully-typed wire format end-to-end (no JSON-over-string).
//!
//! Naming:
//!
//! - Wire structs that map 1:1 onto a single Haskell record use the
//!   `Wire*` prefix on the Rust side and the unprefixed name on the
//!   Haskell side (e.g. `WireEphemeralConfig` ↔ Haskell `EphemeralConfig`).
//! - Unit-variant enums use the `Cat` / `Flag` / etc. prefix on the
//!   Haskell ctors to avoid namespace collisions with effect GADT ctors
//!   (matches `Pattern.Memory`'s `BlockCore` / `SchemaText` precedent).

use jiff::Span;
use smol_str::SmolStr;
use tidepool_bridge_derive::{FromCore, ToCore};

use pattern_core::types::ids::PersonaId;
use pattern_core::{
    BlockRef, CapabilityFlag, CapabilitySet, EffectCategory,
    spawn::{
        EphemeralConfig, ForkConfig, ForkIsolation, PersonaConfig, RelationshipKind, SiblingConfig,
        SiblingPersona,
    },
};

use crate::spawn::{SpawnResult, TerminationReason};

// ── BlockRef ─────────────────────────────────────────────────────────────────

/// Wire mirror of [`pattern_core::BlockRef`].
#[derive(Debug, FromCore)]
#[core(module = "Pattern.Spawn", name = "BlockRef")]
pub struct WireBlockRef {
    pub label: String,
    pub block_id: String,
    pub agent_id: String,
}

impl From<WireBlockRef> for BlockRef {
    fn from(w: WireBlockRef) -> Self {
        BlockRef::with_owner(w.label, w.block_id, w.agent_id)
    }
}

// ── EffectCategory ───────────────────────────────────────────────────────────

/// Wire mirror of [`pattern_core::EffectCategory`].
///
/// Constructor names are `Cat`-prefixed on the Haskell side to avoid
/// clashing with effect GADT type names visible in the same import
/// scope.
#[derive(Debug, FromCore)]
pub enum WireEffectCategory {
    #[core(module = "Pattern.Spawn", name = "CatMemory")]
    Memory,
    #[core(module = "Pattern.Spawn", name = "CatSearch")]
    Search,
    #[core(module = "Pattern.Spawn", name = "CatRecall")]
    Recall,
    #[core(module = "Pattern.Spawn", name = "CatTasks")]
    Tasks,
    #[core(module = "Pattern.Spawn", name = "CatSkills")]
    Skills,
    #[core(module = "Pattern.Spawn", name = "CatMessage")]
    Message,
    #[core(module = "Pattern.Spawn", name = "CatDisplay")]
    Display,
    #[core(module = "Pattern.Spawn", name = "CatTime")]
    Time,
    #[core(module = "Pattern.Spawn", name = "CatLog")]
    Log,
    #[core(module = "Pattern.Spawn", name = "CatShell")]
    Shell,
    #[core(module = "Pattern.Spawn", name = "CatFile")]
    File,
    #[core(module = "Pattern.Spawn", name = "CatSources")]
    Sources,
    #[core(module = "Pattern.Spawn", name = "CatMcp")]
    Mcp,
    #[core(module = "Pattern.Spawn", name = "CatRpc")]
    Rpc,
    #[core(module = "Pattern.Spawn", name = "CatSpawn")]
    Spawn,
    #[core(module = "Pattern.Spawn", name = "CatDiagnostics")]
    Diagnostics,
    #[core(module = "Pattern.Spawn", name = "CatWake")]
    Wake,
}

impl From<WireEffectCategory> for EffectCategory {
    fn from(w: WireEffectCategory) -> Self {
        match w {
            WireEffectCategory::Memory => EffectCategory::Memory,
            WireEffectCategory::Search => EffectCategory::Search,
            WireEffectCategory::Recall => EffectCategory::Recall,
            WireEffectCategory::Tasks => EffectCategory::Tasks,
            WireEffectCategory::Skills => EffectCategory::Skills,
            WireEffectCategory::Message => EffectCategory::Message,
            WireEffectCategory::Display => EffectCategory::Display,
            WireEffectCategory::Time => EffectCategory::Time,
            WireEffectCategory::Log => EffectCategory::Log,
            WireEffectCategory::Shell => EffectCategory::Shell,
            WireEffectCategory::File => EffectCategory::File,
            WireEffectCategory::Sources => EffectCategory::Sources,
            WireEffectCategory::Mcp => EffectCategory::Mcp,
            WireEffectCategory::Rpc => EffectCategory::Rpc,
            WireEffectCategory::Spawn => EffectCategory::Spawn,
            WireEffectCategory::Diagnostics => EffectCategory::Diagnostics,
            WireEffectCategory::Wake => EffectCategory::Wake,
        }
    }
}

// ── CapabilityFlag ───────────────────────────────────────────────────────────

/// Wire mirror of [`pattern_core::CapabilityFlag`]. Haskell ctors carry a
/// `Flag` prefix.
#[derive(Debug, FromCore)]
pub enum WireCapabilityFlag {
    #[core(module = "Pattern.Spawn", name = "FlagSpawnNewIdentities")]
    SpawnNewIdentities,
    #[core(module = "Pattern.Spawn", name = "FlagWakeConditionRegistration")]
    WakeConditionRegistration,
    #[core(module = "Pattern.Spawn", name = "FlagFrontingControl")]
    FrontingControl,
}

impl From<WireCapabilityFlag> for CapabilityFlag {
    fn from(w: WireCapabilityFlag) -> Self {
        match w {
            WireCapabilityFlag::SpawnNewIdentities => CapabilityFlag::SpawnNewIdentities,
            WireCapabilityFlag::WakeConditionRegistration => {
                CapabilityFlag::WakeConditionRegistration
            }
            WireCapabilityFlag::FrontingControl => CapabilityFlag::FrontingControl,
        }
    }
}

// ── CapabilitySet ────────────────────────────────────────────────────────────

/// Wire mirror of [`pattern_core::CapabilitySet`].
///
/// `categories` and `flags` are lists on the wire; the conversion to the
/// `BTreeSet`-backed domain type dedups silently.
#[derive(Debug, FromCore)]
#[core(module = "Pattern.Spawn", name = "CapabilitySet")]
pub struct WireCapabilitySet {
    pub categories: Vec<WireEffectCategory>,
    pub flags: Vec<WireCapabilityFlag>,
}

impl From<WireCapabilitySet> for CapabilitySet {
    fn from(w: WireCapabilitySet) -> Self {
        let set = w
            .categories
            .into_iter()
            .map(EffectCategory::from)
            .collect::<CapabilitySet>();
        set.with_flags(w.flags.into_iter().map(CapabilityFlag::from))
    }
}

// ── ForkIsolation ────────────────────────────────────────────────────────────

#[derive(Debug, FromCore)]
pub enum WireForkIsolation {
    #[core(module = "Pattern.Spawn", name = "Lightweight")]
    Lightweight,
    #[core(module = "Pattern.Spawn", name = "Persistent")]
    Persistent,
}

impl From<WireForkIsolation> for ForkIsolation {
    fn from(w: WireForkIsolation) -> Self {
        match w {
            WireForkIsolation::Lightweight => ForkIsolation::Lightweight,
            WireForkIsolation::Persistent => ForkIsolation::Persistent,
        }
    }
}

// ── RelationshipKind ─────────────────────────────────────────────────────────

#[derive(Debug, FromCore)]
pub enum WireRelationshipKind {
    #[core(module = "Pattern.Spawn", name = "SupervisorOf")]
    SupervisorOf,
    #[core(module = "Pattern.Spawn", name = "SpecialistFor")]
    SpecialistFor,
    #[core(module = "Pattern.Spawn", name = "PeerWith")]
    PeerWith,
    #[core(module = "Pattern.Spawn", name = "ObserverOf")]
    ObserverOf,
}

impl From<WireRelationshipKind> for RelationshipKind {
    fn from(w: WireRelationshipKind) -> Self {
        match w {
            WireRelationshipKind::SupervisorOf => RelationshipKind::SupervisorOf,
            WireRelationshipKind::SpecialistFor => RelationshipKind::SpecialistFor,
            WireRelationshipKind::PeerWith => RelationshipKind::PeerWith,
            WireRelationshipKind::ObserverOf => RelationshipKind::ObserverOf,
        }
    }
}

// ── PersonaConfig ────────────────────────────────────────────────────────────

#[derive(Debug, FromCore)]
#[core(module = "Pattern.Spawn", name = "PersonaConfig")]
pub struct WirePersonaConfig {
    pub name: String,
    pub system_prompt: String,
    pub capabilities: WireCapabilitySet,
}

impl From<WirePersonaConfig> for PersonaConfig {
    fn from(w: WirePersonaConfig) -> Self {
        PersonaConfig::new(w.name, w.system_prompt, w.capabilities.into())
    }
}

// ── SiblingPersona ───────────────────────────────────────────────────────────

#[derive(Debug, FromCore)]
pub enum WireSiblingPersona {
    #[core(module = "Pattern.Spawn", name = "ExistingPersona")]
    Existing(String),
    #[core(module = "Pattern.Spawn", name = "NewPersona")]
    New(WirePersonaConfig),
}

impl From<WireSiblingPersona> for SiblingPersona {
    fn from(w: WireSiblingPersona) -> Self {
        match w {
            WireSiblingPersona::Existing(id) => {
                SiblingPersona::Existing(PersonaId::from(SmolStr::from(id)))
            }
            WireSiblingPersona::New(cfg) => SiblingPersona::New(cfg.into()),
        }
    }
}

// ── EphemeralConfig ──────────────────────────────────────────────────────────

#[derive(Debug, FromCore)]
#[core(module = "Pattern.Spawn", name = "EphemeralConfig")]
pub struct WireEphemeralConfig {
    pub program: String,
    pub costume: Option<String>,
    pub capabilities: Option<WireCapabilitySet>,
    /// Timeout in milliseconds; converted to `jiff::Span` at the handler boundary.
    pub timeout_ms: Option<i64>,
    /// Optional initial human-role prompt seeded into the child's first
    /// turn input.
    pub prompt: Option<String>,
}

impl From<WireEphemeralConfig> for EphemeralConfig {
    fn from(w: WireEphemeralConfig) -> Self {
        let mut cfg = EphemeralConfig::new(w.program);
        if let Some(c) = w.costume {
            cfg = cfg.with_costume(c);
        }
        if let Some(caps) = w.capabilities {
            cfg = cfg.with_capabilities(caps.into());
        }
        if let Some(ms) = w.timeout_ms {
            cfg = cfg.with_timeout(Span::new().milliseconds(ms));
        }
        if let Some(p) = w.prompt {
            cfg = cfg.with_prompt(p);
        }
        cfg
    }
}

// ── ForkConfig ───────────────────────────────────────────────────────────────

#[derive(Debug, FromCore)]
#[core(module = "Pattern.Spawn", name = "ForkConfig")]
pub struct WireForkConfig {
    pub program: String,
    pub isolation: WireForkIsolation,
    pub capabilities: Option<WireCapabilitySet>,
    pub timeout_hint_ms: Option<i64>,
    pub task_ref: Option<WireBlockRef>,
}

impl From<WireForkConfig> for ForkConfig {
    fn from(w: WireForkConfig) -> Self {
        let mut cfg = ForkConfig::new(w.program);
        cfg.isolation = w.isolation.into();
        if let Some(caps) = w.capabilities {
            cfg = cfg.with_capabilities(caps.into());
        }
        if let Some(ms) = w.timeout_hint_ms {
            cfg = cfg.with_timeout_hint(Span::new().milliseconds(ms));
        }
        if let Some(r) = w.task_ref {
            cfg = cfg.with_task_ref(r.into());
        }
        cfg
    }
}

// ── SiblingConfig ────────────────────────────────────────────────────────────

#[derive(Debug, FromCore)]
#[core(module = "Pattern.Spawn", name = "SiblingConfig")]
pub struct WireSiblingConfig {
    pub persona: WireSiblingPersona,
    pub relationship: WireRelationshipKind,
    pub shared_blocks: Vec<String>,
}

impl From<WireSiblingConfig> for SiblingConfig {
    fn from(w: WireSiblingConfig) -> Self {
        SiblingConfig::new(w.persona.into(), w.relationship.into())
            .with_shared_blocks(w.shared_blocks)
    }
}

// ── SpawnReq ─────────────────────────────────────────────────────────────────

/// Rust mirror of the Haskell `Spawn` GADT.
#[derive(Debug, FromCore)]
pub enum SpawnReq {
    /// Non-blocking spawn; returns a `SpawnId` immediately. Use
    /// [`SpawnReq::AwaitSpawn`] (or [`SpawnReq::AwaitAll`]) to block on
    /// the result.
    #[core(module = "Pattern.Spawn", name = "Ephemeral")]
    Ephemeral(WireEphemeralConfig),

    /// Block until the given ephemeral completes; return its result.
    #[core(module = "Pattern.Spawn", name = "AwaitSpawn")]
    AwaitSpawn(String /* SpawnId */),

    /// Block until every id completes; return per-id results in id-order.
    /// Handler uses `futures::future::join_all` (not `try_join_all`) so
    /// partial failures are preserved.
    #[core(module = "Pattern.Spawn", name = "AwaitAll")]
    AwaitAll(Vec<String> /* [SpawnId] */),

    /// Spawn a fork. Returns a `ForkHandle` opaque token.
    #[core(module = "Pattern.Spawn", name = "Fork")]
    Fork(WireForkConfig),

    /// Spawn a sibling persona; returns the sibling's `PersonaId`.
    #[core(module = "Pattern.Spawn", name = "Sibling")]
    Sibling(WireSiblingConfig),

    /// Cancel an in-flight spawn by id. Idempotent.
    #[core(module = "Pattern.Spawn", name = "Stop")]
    Stop(String /* SpawnId */),

    /// Resolve a fork by id. Carries the fork id and the operation to perform.
    ///
    /// This variant is NOT a GADT constructor on the Haskell side in the
    /// traditional sense — on the Haskell side the GADT ctor is `ForkOp ::
    /// SpawnId -> ForkOpKind -> Spawn ForkOpResult`. On the Rust side we
    /// carry the id as a plain `String` and the op as a typed sum.
    #[core(module = "Pattern.Spawn", name = "ForkOp")]
    ForkOp(String /* fork_id */, WireForkOpKind),
}

// ── Return-direction wire types (Rust → Haskell, derive ToCore) ──────────────
//
// The next batch of types crosses the boundary in the OTHER direction:
// the handler builds a Rust value and returns it to the Haskell caller as
// a typed Core record. Each type derives `ToCore`.

/// Wire mirror of the typed handle returned by `Spawn.ephemeral`.
///
/// Pairs the spawn id with the constellation-scoped progress-log block
/// label so the parent can read live progress without waiting for the
/// child to complete.
#[derive(Debug, ToCore)]
#[core(module = "Pattern.Spawn", name = "EphemeralSpawn")]
pub struct WireEphemeralSpawn {
    pub spawn_id: String,
    pub progress_log_label: String,
}

/// Wire mirror of [`crate::spawn::TerminationReason`]. `Term`-prefix on
/// ctors keeps this from clashing with effect ctor names.
#[derive(Debug, ToCore)]
pub enum WireTerminationReason {
    #[core(module = "Pattern.Spawn", name = "TermEndTurn")]
    EndTurn,
    #[core(module = "Pattern.Spawn", name = "TermToolUse")]
    ToolUse,
    #[core(module = "Pattern.Spawn", name = "TermMaxTurns")]
    MaxTurns,
    #[core(module = "Pattern.Spawn", name = "TermTimeout")]
    Timeout,
    #[core(module = "Pattern.Spawn", name = "TermCancelled")]
    Cancelled,
    #[core(module = "Pattern.Spawn", name = "TermError")]
    Error,
}

impl From<TerminationReason> for WireTerminationReason {
    fn from(t: TerminationReason) -> Self {
        match t {
            TerminationReason::EndTurn => Self::EndTurn,
            TerminationReason::ToolUse => Self::ToolUse,
            TerminationReason::MaxTurns => Self::MaxTurns,
            TerminationReason::Timeout => Self::Timeout,
            TerminationReason::Cancelled => Self::Cancelled,
            TerminationReason::Error => Self::Error,
        }
    }
}

/// Wire mirror of [`crate::spawn::SpawnResult`] returned by
/// `Spawn.awaitSpawn`.
#[derive(Debug, ToCore)]
#[core(module = "Pattern.Spawn", name = "SpawnResult")]
pub struct WireSpawnResult {
    pub child_id: String,
    pub final_text: Option<String>,
    pub turns: i64,
    pub terminated: WireTerminationReason,
    pub progress_log_label: Option<String>,
}

impl From<SpawnResult> for WireSpawnResult {
    fn from(r: SpawnResult) -> Self {
        Self {
            child_id: r.child_id.to_string(),
            final_text: r.final_text,
            turns: r.turns as i64,
            terminated: r.terminated.into(),
            progress_log_label: r.progress_log_label.map(|s| s.to_string()),
        }
    }
}

/// Wire mirror of a per-id `awaitAll` outcome. Avoids reaching into
/// `Data.Either` for the Core encoding by keeping a typed sum local to
/// `Pattern.Spawn`.
#[derive(Debug, ToCore)]
pub enum WireSpawnAwaitOutcome {
    #[core(module = "Pattern.Spawn", name = "SpawnOk")]
    Ok(WireSpawnResult),
    #[core(module = "Pattern.Spawn", name = "SpawnFail")]
    Fail(String /* SpawnError display */),
}

// ── ForkOp wire types (Phase 3 Task 8.3) ────────────────────────────────────
//
// These types carry the Haskell→Rust direction for fork resolution ops
// (`FromCore`) and the Rust→Haskell direction for results (`ToCore`).

/// Wire mirror of the Haskell `ForkOpKind` sum. Incoming from Haskell.
///
/// The `ForkOp` prefix on constructors mirrors the Haskell naming convention
/// — same rationale as `Cat*` for `EffectCategory` and `Flag*` for
/// `CapabilityFlag`: avoids namespace clashes in the GADT constructor scope.
///
/// Three resolution paths (no `AwaitResult` — lightweight forks are memory
/// snapshots, not running sessions; there is nothing to await):
///
/// - `MergeBack`: import the fork's CRDT state into the parent; handle STAYS
///   in the registry so callers may merge again.
/// - `Discard`: drop the fork without propagating; handle REMOVED from registry.
/// - `Promote`: mint a draft persona from the fork; handle REMOVED from registry.
///   Requires `SpawnNewIdentities` on the spawner's capability snapshot.
#[derive(Debug, FromCore)]
pub enum WireForkOpKind {
    #[core(module = "Pattern.Spawn", name = "ForkOpMergeBack")]
    MergeBack,
    #[core(module = "Pattern.Spawn", name = "ForkOpDiscard")]
    Discard,
    #[core(module = "Pattern.Spawn", name = "ForkOpPromote")]
    Promote(WirePersonaConfig),
}

/// Wire mirror of the Haskell `ForkOpResult` sum. Outgoing to Haskell.
///
/// Three variants matching the three resolution paths:
///
/// - `Unit`: returned by `Discard` (no meaningful payload).
/// - `MergeReport`: returned by `MergeBack`; payload is a JSON-ish debug
///   rendering of the merge report. Phase 7+ may add structured accessors.
/// - `PersonaId`: returned by `Promote`; payload is the new persona id.
#[derive(Debug, ToCore)]
pub enum WireForkOpResult {
    /// `Discard` returns unit.
    #[core(module = "Pattern.Spawn", name = "ForkOpUnit")]
    Unit,
    /// `MergeBack` returns an opaque merge-report text.
    #[core(module = "Pattern.Spawn", name = "ForkOpMergeReport")]
    MergeReport(String),
    /// `Promote` returns the new persona id.
    #[core(module = "Pattern.Spawn", name = "ForkOpPersonaId")]
    PersonaId(String),
}

impl From<Result<SpawnResult, crate::spawn::SpawnError>> for WireSpawnAwaitOutcome {
    fn from(r: Result<SpawnResult, crate::spawn::SpawnError>) -> Self {
        match r {
            Ok(s) => WireSpawnAwaitOutcome::Ok(s.into()),
            Err(e) => WireSpawnAwaitOutcome::Fail(e.to_string()),
        }
    }
}

/// Wire mirror of the typed sum returned by `Spawn.sibling`.
///
/// Each variant corresponds to a distinct spawn outcome, eliminating
/// the meaningless states the old flat-record shape permitted (e.g.
/// `Active` with a `kdl_path`, or `Draft` without one).
///
/// Mirrors `SiblingSpawn` in `haskell/Pattern/Spawn.hs`:
///
/// ```haskell
/// data SiblingSpawn
///   = SiblingExistingActive PersonaId
///   | SiblingNewActive PersonaId Text
///   | SiblingNewDraft  PersonaId Text
/// ```
#[derive(Debug, ToCore)]
pub enum WireSiblingSpawn {
    /// An existing registered persona was adopted. Always authorised for
    /// live session-open (Phase 6 promotes). No draft KDL path.
    #[core(module = "Pattern.Spawn", name = "SiblingExistingActive")]
    ExistingActive(String /* PersonaId */),
    /// A new persona was minted AND the parent held `SpawnNewIdentities`.
    /// Phase 6 promotes to a live session. Carries the on-disk KDL draft path.
    #[core(module = "Pattern.Spawn", name = "SiblingNewActive")]
    NewActive(String /* PersonaId */, String /* kdl_path */),
    /// A new persona was minted but the parent lacked `SpawnNewIdentities`.
    /// Pending human-driven promote. Carries the on-disk KDL draft path.
    #[core(module = "Pattern.Spawn", name = "SiblingNewDraft")]
    NewDraft(String /* PersonaId */, String /* kdl_path */),
}

impl From<crate::spawn::sibling::SiblingNewOutcome> for WireSiblingSpawn {
    fn from(o: crate::spawn::sibling::SiblingNewOutcome) -> Self {
        let path = o.kdl_path.display().to_string();
        match o.status {
            crate::spawn::sibling::SiblingStatus::Active => {
                WireSiblingSpawn::NewActive(o.persona_id.to_string(), path)
            }
            crate::spawn::sibling::SiblingStatus::Draft => {
                WireSiblingSpawn::NewDraft(o.persona_id.to_string(), path)
            }
        }
    }
}
