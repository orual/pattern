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

    /// Construct a fully-populated `PersonaRecord`. Used by registry backends
    /// that load all fields from storage in one pass.
    pub fn from_parts(
        id: PersonaId,
        name: String,
        status: PersonaStatus,
        config_path: Option<std::path::PathBuf>,
        project_attachments: Vec<std::path::PathBuf>,
        relationships: Vec<RelationshipEdge>,
        group_memberships: Vec<GroupId>,
    ) -> Self {
        Self {
            id,
            name,
            status,
            config_path,
            project_attachments,
            relationships,
            group_memberships,
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

// ── PersonaGroup ─────────────────────────────────────────────────────────────

/// A named group of personas, scoped optionally to a project.
///
/// Groups are organisational only — they do not gate cross-agent search or any
/// other permission decision (see `pattern_runtime::sdk::handlers::scope`).
/// They exist to let humans label cooperating personas for UI / CLI surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct PersonaGroup {
    /// Unique identifier for the group.
    pub id: GroupId,
    /// Human-readable name. Unique within `project_id` (or globally if `None`).
    pub name: String,
    /// Project this group belongs to. `None` means a constellation-wide group.
    pub project_id: Option<String>,
    /// Persona ids that are members of this group.
    pub members: Vec<PersonaId>,
}

impl PersonaGroup {
    /// Construct a `PersonaGroup` with no members.
    pub fn new(
        id: impl Into<GroupId>,
        name: impl Into<String>,
        project_id: Option<String>,
    ) -> Self {
        Self::with_members(id, name, project_id, Vec::new())
    }

    /// Construct a `PersonaGroup` with a pre-populated member list.
    pub fn with_members(
        id: impl Into<GroupId>,
        name: impl Into<String>,
        project_id: Option<String>,
        members: Vec<PersonaId>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            project_id,
            members,
        }
    }
}

// ── RelationshipSpec ─────────────────────────────────────────────────────────

/// Construction spec for a directed relationship edge.
///
/// `RelationshipEdge` is the *view* (one-sided, persona-relative). `RelationshipSpec`
/// is the *write*: identifies both endpoints and the kind, leaving direction
/// implicit (always `from -> to`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct RelationshipSpec {
    /// Source persona of the edge.
    pub from: PersonaId,
    /// Target persona of the edge.
    pub to: PersonaId,
    /// Semantic label.
    pub kind: RelationshipKind,
}

impl RelationshipSpec {
    /// Construct a new relationship spec.
    pub fn new(from: impl Into<PersonaId>, to: impl Into<PersonaId>, kind: RelationshipKind) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            kind,
        }
    }
}

// ── RegistryError ────────────────────────────────────────────────────────────

/// Errors returned by `ConstellationRegistry` operations.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[non_exhaustive]
pub enum RegistryError {
    /// The requested persona does not exist in the registry.
    #[error("persona not found: {0}")]
    #[diagnostic(
        code(pattern_core::registry::persona_not_found),
        help("ensure the persona id is correct and the persona has been registered")
    )]
    PersonaNotFound(PersonaId),

    /// A persona with the given id is already registered.
    #[error("duplicate persona: {0}")]
    #[diagnostic(
        code(pattern_core::registry::duplicate_persona),
        help("use set_status or update methods to modify an existing persona")
    )]
    DuplicatePersona(PersonaId),

    /// The requested group does not exist in the registry.
    #[error("group not found: {0}")]
    #[diagnostic(
        code(pattern_core::registry::group_not_found),
        help("ensure the group id is correct and the group has been created")
    )]
    GroupNotFound(GroupId),

    /// A group with the given (name, project_id) is already registered.
    #[error("duplicate group: name={name:?} project={project_id:?}")]
    #[diagnostic(
        code(pattern_core::registry::duplicate_group),
        help("group names must be unique within a project (or globally if project is None)")
    )]
    DuplicateGroup {
        name: String,
        project_id: Option<String>,
    },

    /// The registry backend is unavailable (connection failure, lock poisoned,
    /// etc.).
    #[error("registry backend unavailable")]
    #[diagnostic(code(pattern_core::registry::backend_unavailable))]
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
pub trait ConstellationRegistry: Send + Sync + std::fmt::Debug {
    /// List all personas matching `scope`, in an unspecified but stable order.
    ///
    /// `RegistryScope::All` returns every persona. `RegistryScope::Project(p)`
    /// filters to personas whose `project_attachments` contains `p`.
    async fn list(&self, scope: RegistryScope) -> Result<Vec<PersonaRecord>, RegistryError>;

    /// Fetch a single persona by id.
    ///
    /// Returns `Ok(None)` when no persona with the given id exists.
    async fn get(&self, id: &PersonaId) -> Result<Option<PersonaRecord>, RegistryError>;

    /// Find personas matching the given filters.
    ///
    /// Both filters are optional and AND together when both are set:
    /// - `project`: only personas whose `project_attachments` contains the path.
    /// - `kind`: only personas with at least one relationship of this kind.
    async fn find(
        &self,
        project: Option<&std::path::Path>,
        kind: Option<RelationshipKind>,
    ) -> Result<Vec<PersonaRecord>, RegistryError>;

    /// Insert a new persona record.
    ///
    /// Returns `RegistryError::DuplicatePersona` if a persona with the same id
    /// is already registered.
    async fn register(&self, record: PersonaRecord) -> Result<(), RegistryError>;

    /// Update the lifecycle status of an existing persona.
    ///
    /// Returns `RegistryError::PersonaNotFound` if no persona with the given id
    /// exists.
    async fn set_status(
        &self,
        id: &PersonaId,
        status: PersonaStatus,
    ) -> Result<(), RegistryError>;

    /// Add a relationship edge between two personas.
    ///
    /// Returns `RegistryError::PersonaNotFound` if either endpoint is missing.
    /// Implementations are expected to dedupe edges with the same
    /// `(from, to, kind)` triple (DB-backed impls rely on the UNIQUE constraint
    /// from migration 0015).
    async fn add_relationship(&self, edge: RelationshipSpec) -> Result<(), RegistryError>;

    /// List all groups matching `scope`.
    ///
    /// `RegistryScope::All` returns every group. `RegistryScope::Project(p)`
    /// returns groups whose `project_id` matches `p`'s string form (or
    /// constellation-wide groups when `project_id` is `None` — implementation-
    /// defined; the DB-backed impl filters by exact project id match).
    async fn groups(&self, scope: RegistryScope) -> Result<Vec<PersonaGroup>, RegistryError>;

    /// Create a new persona group.
    ///
    /// Returns `RegistryError::DuplicateGroup` if a group with the same
    /// `(name, project_id)` already exists.
    async fn create_group(
        &self,
        name: String,
        project_id: Option<String>,
    ) -> Result<PersonaGroup, RegistryError>;
}

/// Always-empty `ConstellationRegistry` used as a Phase 5 placeholder
/// until the Phase 6 `pattern_db`-backed implementation lands.
///
/// `list` returns `Ok(vec![])` and `get` returns `Ok(None)` for every
/// id. Daemon callers wire this into `FrontingState` so the
/// empty-fronting path falls through to
/// `ResolveOutcome::SystemDefault` (the documented "no fronting
/// configured" behaviour). Phase 6 will replace this with a real
/// registry that loads persona records from the project's
/// `pattern_db`.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyConstellationRegistry;

#[async_trait]
impl ConstellationRegistry for EmptyConstellationRegistry {
    async fn list(&self, _scope: RegistryScope) -> Result<Vec<PersonaRecord>, RegistryError> {
        Ok(Vec::new())
    }

    async fn get(&self, _id: &PersonaId) -> Result<Option<PersonaRecord>, RegistryError> {
        Ok(None)
    }

    async fn find(
        &self,
        _project: Option<&std::path::Path>,
        _kind: Option<RelationshipKind>,
    ) -> Result<Vec<PersonaRecord>, RegistryError> {
        Ok(Vec::new())
    }

    async fn register(&self, _record: PersonaRecord) -> Result<(), RegistryError> {
        Err(RegistryError::BackendUnavailable)
    }

    async fn set_status(
        &self,
        _id: &PersonaId,
        _status: PersonaStatus,
    ) -> Result<(), RegistryError> {
        Err(RegistryError::BackendUnavailable)
    }

    async fn add_relationship(&self, _edge: RelationshipSpec) -> Result<(), RegistryError> {
        Err(RegistryError::BackendUnavailable)
    }

    async fn groups(&self, _scope: RegistryScope) -> Result<Vec<PersonaGroup>, RegistryError> {
        Ok(Vec::new())
    }

    async fn create_group(
        &self,
        _name: String,
        _project_id: Option<String>,
    ) -> Result<PersonaGroup, RegistryError> {
        Err(RegistryError::BackendUnavailable)
    }
}
