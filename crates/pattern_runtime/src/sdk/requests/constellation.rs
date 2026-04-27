//! Mirror of `Pattern.Constellation` (`haskell/Pattern/Constellation.hs`).
//!
//! Read-only surface: `List`, `Find`, `Groups`. Each cross the boundary as
//! typed Core values via `FromCore` (Haskell→Rust) and `ToCore` (Rust→Haskell)
//! — no JSON-string shortcut.

use tidepool_bridge_derive::{FromCore, ToCore};

use pattern_core::PersonaId;
use pattern_core::constellation::{
    EdgeDirection, PersonaGroup, PersonaRecord, PersonaStatus, RelationshipEdge,
};
use pattern_core::spawn::RelationshipKind;

// ── ConstellationReq (Haskell → Rust) ────────────────────────────────────────

/// Rust mirror of the Haskell `Constellation` GADT.
///
/// `Maybe Text` parameters cross the boundary as `Option<String>`. The handler
/// interprets:
/// - `scope`: `None` → `RegistryScope::All`; `Some(p)` → `RegistryScope::Project(p)`.
/// - `project`: same `None`/`Some(p)` semantics for `find`.
/// - `kind`: `None` → no kind filter; `Some(s)` → parse `s` as a snake_case
///   relationship kind (`supervisor_of`, `specialist_for`, `peer_with`,
///   `observer_of`).
#[derive(Debug, FromCore)]
pub enum ConstellationReq {
    #[core(module = "Pattern.Constellation", name = "List")]
    List(Option<String>),

    #[core(module = "Pattern.Constellation", name = "Find")]
    Find(Option<String>, Option<String>),

    #[core(module = "Pattern.Constellation", name = "Groups")]
    Groups(Option<String>),
}

// ── Wire types (Rust → Haskell, derive ToCore) ───────────────────────────────

/// Wire mirror of [`PersonaStatus`].
///
/// Names are prefixed `Persona` so the constructors don't collide with the
/// Haskell `Active` / `Draft` / `Inactive` from any other module that might be
/// in scope.
#[derive(Debug, ToCore)]
pub enum WirePersonaStatus {
    #[core(module = "Pattern.Constellation", name = "PersonaActive")]
    Active,
    #[core(module = "Pattern.Constellation", name = "PersonaDraft")]
    Draft,
    #[core(module = "Pattern.Constellation", name = "PersonaInactive")]
    Inactive,
}

impl From<PersonaStatus> for WirePersonaStatus {
    fn from(s: PersonaStatus) -> Self {
        match s {
            PersonaStatus::Active => Self::Active,
            PersonaStatus::Draft => Self::Draft,
            PersonaStatus::Inactive => Self::Inactive,
        }
    }
}

/// Wire mirror of [`RelationshipKind`].
///
/// Constructor names are `Rel`-prefixed to avoid collision with Spawn's
/// `WireRelationshipKind` which uses bare names (`SupervisorOf`, etc.) under
/// the `Pattern.Spawn` module.
#[derive(Debug, ToCore)]
pub enum WireRelationshipKind {
    #[core(module = "Pattern.Constellation", name = "RelSupervisorOf")]
    SupervisorOf,
    #[core(module = "Pattern.Constellation", name = "RelSpecialistFor")]
    SpecialistFor,
    #[core(module = "Pattern.Constellation", name = "RelPeerWith")]
    PeerWith,
    #[core(module = "Pattern.Constellation", name = "RelObserverOf")]
    ObserverOf,
}

impl From<RelationshipKind> for WireRelationshipKind {
    fn from(k: RelationshipKind) -> Self {
        match k {
            RelationshipKind::SupervisorOf => Self::SupervisorOf,
            RelationshipKind::SpecialistFor => Self::SpecialistFor,
            RelationshipKind::PeerWith => Self::PeerWith,
            RelationshipKind::ObserverOf => Self::ObserverOf,
        }
    }
}

/// Wire mirror of [`EdgeDirection`].
#[derive(Debug, ToCore)]
pub enum WireEdgeDirection {
    #[core(module = "Pattern.Constellation", name = "DirOutgoing")]
    Outgoing,
    #[core(module = "Pattern.Constellation", name = "DirIncoming")]
    Incoming,
}

impl From<EdgeDirection> for WireEdgeDirection {
    fn from(d: EdgeDirection) -> Self {
        match d {
            EdgeDirection::Outgoing => Self::Outgoing,
            EdgeDirection::Incoming => Self::Incoming,
        }
    }
}

/// Wire mirror of [`RelationshipEdge`].
#[derive(Debug, ToCore)]
#[core(module = "Pattern.Constellation", name = "RelationshipEdge")]
pub struct WireRelationshipEdge {
    pub other: String,
    pub kind: WireRelationshipKind,
    pub direction: WireEdgeDirection,
}

impl From<RelationshipEdge> for WireRelationshipEdge {
    fn from(e: RelationshipEdge) -> Self {
        Self {
            other: e.other.to_string(),
            kind: e.kind.into(),
            direction: e.direction.into(),
        }
    }
}

/// Wire mirror of [`PersonaRecord`].
///
/// Paths cross as `String` via `to_string_lossy()`. Group ids cross as
/// `Vec<String>`. The bare `id` field is renamed `persona_id` to avoid
/// shadowing the `DataConId` local the `ToCore` derive uses internally.
#[derive(Debug, ToCore)]
#[core(module = "Pattern.Constellation", name = "PersonaRecord")]
pub struct WirePersonaRecord {
    pub persona_id: String,
    pub name: String,
    pub status: WirePersonaStatus,
    pub config_path: Option<String>,
    pub project_attachments: Vec<String>,
    pub relationships: Vec<WireRelationshipEdge>,
    pub group_memberships: Vec<String>,
}

impl From<PersonaRecord> for WirePersonaRecord {
    fn from(r: PersonaRecord) -> Self {
        Self {
            persona_id: r.id.to_string(),
            name: r.name,
            status: r.status.into(),
            config_path: r
                .config_path
                .map(|p| p.to_string_lossy().into_owned()),
            project_attachments: r
                .project_attachments
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            relationships: r.relationships.into_iter().map(Into::into).collect(),
            group_memberships: r
                .group_memberships
                .into_iter()
                .map(|g| g.to_string())
                .collect(),
        }
    }
}

/// Wire mirror of [`PersonaGroup`].
///
/// `group_id` rather than `id` for the same `ToCore`-derive reason as
/// [`WirePersonaRecord`].
#[derive(Debug, ToCore)]
#[core(module = "Pattern.Constellation", name = "PersonaGroup")]
pub struct WirePersonaGroup {
    pub group_id: String,
    pub name: String,
    pub project_id: Option<String>,
    pub members: Vec<String>,
}

impl From<PersonaGroup> for WirePersonaGroup {
    fn from(g: PersonaGroup) -> Self {
        Self {
            group_id: g.id.to_string(),
            name: g.name,
            project_id: g.project_id,
            members: g
                .members
                .into_iter()
                .map(|p: PersonaId| p.to_string())
                .collect(),
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Parse a snake_case relationship-kind identifier sent from agent code.
pub fn parse_relationship_kind(s: &str) -> Option<RelationshipKind> {
    match s {
        "supervisor_of" => Some(RelationshipKind::SupervisorOf),
        "specialist_for" => Some(RelationshipKind::SpecialistFor),
        "peer_with" => Some(RelationshipKind::PeerWith),
        "observer_of" => Some(RelationshipKind::ObserverOf),
        _ => None,
    }
}
