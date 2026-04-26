//! Constellation registry trait and supporting persona record types.
//!
//! The `ConstellationRegistry` trait is the Phase 5 seam that lets the fronting
//! resolver look up Active personas without depending on the concrete DB-backed
//! implementation (Phase 6 Task 4). Phase 5 ships an in-memory test helper
//! (`pattern_runtime::testing::InMemoryConstellationRegistry`) that implements
//! the trait over a `DashMap`.
//!
//! Phase 6 extends the trait with `find`, `register`, `set_status`,
//! `add_relationship`, and group-related methods once the full schema lands.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::spawn::RelationshipKind;
use crate::types::ids::{GroupId, PersonaId};

// ── PersonaRecord ────────────────────────────────────────────────────────────

/// A single entry in the constellation's persona registry.
///
/// Carries everything the routing layer and the UI need about a persona without
/// requiring a full KDL parse. The DB-backed implementation in Phase 6 Task 4
/// populates this from the `personas` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PersonaRecord {
    /// The persona's unique identifier.
    pub id: PersonaId,
    /// Human-readable display name.
    pub name: String,
    /// Current lifecycle status.
    pub status: PersonaStatus,
    /// Path to the persona's KDL config file, if it has one.
    pub config_path: Option<PathBuf>,
    /// Project directories this persona is attached to.
    pub project_attachments: Vec<PathBuf>,
    /// Relationship edges to other personas in the constellation.
    pub relationships: Vec<RelationshipEdge>,
    /// Group memberships (populated once Phase 6 lands group schema).
    pub group_memberships: Vec<GroupId>,
}

impl PersonaRecord {
    /// Construct a minimal `PersonaRecord` with the given id, name, and status.
    /// All optional / collection fields default to empty.
    pub fn new(id: impl Into<PersonaId>, name: impl Into<String>, status: PersonaStatus) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            status,
            config_path: None,
            project_attachments: Vec::new(),
            relationships: Vec::new(),
            group_memberships: Vec::new(),
        }
    }
}

// ── PersonaStatus ────────────────────────────────────────────────────────────

/// Lifecycle state of a persona in the registry.
///
/// The fronting layer must never add a `Draft` persona to an active fronting
/// set — the setter on `FrontingSet` rejects any `PersonaId` whose registry
/// status is `Draft`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PersonaStatus {
    /// Persona is fully configured and may be assigned to the fronting set.
    Active,
    /// Persona was created but has not yet been promoted by a human or
    /// a privileged supervisor persona. May not appear as an active front.
    Draft,
    /// Persona exists in the registry but is not currently deployable.
    Inactive,
}

// ── RelationshipEdge ─────────────────────────────────────────────────────────

/// A directed relationship between two personas.
///
/// `kind` uses the same `RelationshipKind` enum as the spawn configuration
/// (see `pattern_core::spawn`) — no duplication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipEdge {
    /// The other persona in this relationship.
    pub other: PersonaId,
    /// Semantic label for the relationship (supervisor, specialist, peer, observer).
    pub kind: RelationshipKind,
    /// Whether the edge originates from (`Outgoing`) or points to (`Incoming`)
    /// the owning persona.
    pub direction: EdgeDirection,
}

/// Direction of a relationship edge relative to the persona that owns the record.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EdgeDirection {
    /// This persona is the source of the relationship.
    Outgoing,
    /// This persona is the target of the relationship.
    Incoming,
}

// ── RegistryScope ────────────────────────────────────────────────────────────

/// Scope filter for `ConstellationRegistry::list`.
#[derive(Debug, Clone)]
pub enum RegistryScope {
    /// Return every persona in the registry regardless of project.
    All,
    /// Return only personas whose `project_attachments` include the given path.
    Project(PathBuf),
}

// ── RegistryError ────────────────────────────────────────────────────────────

/// Errors returned by `ConstellationRegistry` operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// The requested persona does not exist in the registry.
    #[error("persona not found: {0}")]
    PersonaNotFound(PersonaId),
    /// The registry backend is unavailable (connection failure, lock poisoned,
    /// etc.).
    #[error("registry backend unavailable")]
    BackendUnavailable,
}

// ── ConstellationRegistry ────────────────────────────────────────────────────

/// Trait for looking up personas in the constellation.
///
/// Phase 5 ships a minimal surface (`list` + `get`) that the fronting resolver
/// needs for the default-persona fallback path. Phase 6 extends the trait with
/// `find`, `register`, `set_status`, `add_relationship`, and group methods
/// once the full DB schema lands.
///
/// Implementations must be `Send + Sync` so they can be held behind an `Arc`
/// and shared across async tasks.
#[async_trait]
pub trait ConstellationRegistry: Send + Sync {
    /// List all personas matching `scope`, in an unspecified but stable order.
    ///
    /// `RegistryScope::All` returns every persona. `RegistryScope::Project(p)`
    /// filters to personas whose `project_attachments` contains `p`.
    async fn list(&self, scope: RegistryScope) -> Result<Vec<PersonaRecord>, RegistryError>;

    /// Fetch a single persona by id.
    ///
    /// Returns `Ok(None)` when no persona with the given id exists.
    async fn get(&self, id: &PersonaId) -> Result<Option<PersonaRecord>, RegistryError>;
}

// `GroupId` is defined in `crate::types::ids` and re-exported from the crate
// root. `PersonaRecord.group_memberships` uses that type directly; no
// re-declaration is needed in this module.
