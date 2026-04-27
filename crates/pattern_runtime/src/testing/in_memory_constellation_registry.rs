//! In-memory `ConstellationRegistry` test double.
//!
//! DashMap-backed implementation used by Phase 5 fronting tests to exercise
//! `DefaultPersona` / `SystemDefault` outcomes deterministically without
//! requiring a real database.
//!
//! # Usage
//!
//! ```rust,ignore
//! use pattern_runtime::testing::InMemoryConstellationRegistry;
//! use pattern_core::{PersonaRecord, PersonaStatus};
//!
//! let registry = InMemoryConstellationRegistry::new();
//! registry.seed(PersonaRecord::new("alice", "Alice", PersonaStatus::Active));
//! registry.seed(PersonaRecord::new("bob", "Bob", PersonaStatus::Draft));
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use pattern_core::PersonaId;
use pattern_core::constellation::{
    ConstellationRegistry, EdgeDirection, PersonaGroup, PersonaRecord, PersonaStatus,
    RegistryError, RegistryScope, RelationshipEdge, RelationshipSpec,
};
use pattern_core::spawn::RelationshipKind;
use pattern_core::types::ids::{GroupId, new_id};

/// Thread-safe in-memory `ConstellationRegistry` implementation.
///
/// All methods are non-blocking. Use `seed` to populate before tests and
/// `get_record` / `remove` to inspect or mutate state mid-test.
#[derive(Debug, Default, Clone)]
pub struct InMemoryConstellationRegistry {
    records: Arc<DashMap<PersonaId, PersonaRecord>>,
    groups: Arc<DashMap<GroupId, PersonaGroup>>,
}

impl InMemoryConstellationRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a persona record.
    pub fn seed(&self, record: PersonaRecord) {
        self.records.insert(record.id.clone(), record);
    }

    /// Remove a persona record by id. Returns `true` if the record existed.
    pub fn remove(&self, id: &PersonaId) -> bool {
        self.records.remove(id).is_some()
    }

    /// Read a persona record without going through the async trait.
    pub fn get_record(&self, id: &PersonaId) -> Option<PersonaRecord> {
        self.records.get(id).map(|r| r.clone())
    }

    /// Number of records currently in the registry.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// `true` if the registry contains no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[async_trait]
impl ConstellationRegistry for InMemoryConstellationRegistry {
    async fn list(&self, scope: RegistryScope) -> Result<Vec<PersonaRecord>, RegistryError> {
        let records: Vec<_> = match &scope {
            RegistryScope::All => self.records.iter().map(|r| r.value().clone()).collect(),
            RegistryScope::Project(p) => self
                .records
                .iter()
                .filter(|r| r.value().project_attachments.contains(p))
                .map(|r| r.value().clone())
                .collect(),
        };
        Ok(records)
    }

    async fn get(&self, id: &PersonaId) -> Result<Option<PersonaRecord>, RegistryError> {
        Ok(self.records.get(id).map(|r| r.clone()))
    }

    async fn find(
        &self,
        project: Option<&std::path::Path>,
        kind: Option<RelationshipKind>,
    ) -> Result<Vec<PersonaRecord>, RegistryError> {
        let records: Vec<_> = self
            .records
            .iter()
            .filter(|r| {
                let rec = r.value();
                if let Some(p) = project
                    && !rec.project_attachments.iter().any(|a| a == p)
                {
                    return false;
                }
                if let Some(k) = kind
                    && !rec.relationships.iter().any(|edge| edge.kind == k)
                {
                    return false;
                }
                true
            })
            .map(|r| r.value().clone())
            .collect();
        Ok(records)
    }

    async fn register(&self, record: PersonaRecord) -> Result<(), RegistryError> {
        if self.records.contains_key(&record.id) {
            return Err(RegistryError::DuplicatePersona(record.id));
        }
        self.records.insert(record.id.clone(), record);
        Ok(())
    }

    async fn set_status(
        &self,
        id: &PersonaId,
        status: PersonaStatus,
    ) -> Result<(), RegistryError> {
        let mut entry = self
            .records
            .get_mut(id)
            .ok_or_else(|| RegistryError::PersonaNotFound(id.clone()))?;
        entry.value_mut().status = status;
        Ok(())
    }

    async fn add_relationship(&self, edge: RelationshipSpec) -> Result<(), RegistryError> {
        if !self.records.contains_key(&edge.from) {
            return Err(RegistryError::PersonaNotFound(edge.from));
        }
        if !self.records.contains_key(&edge.to) {
            return Err(RegistryError::PersonaNotFound(edge.to));
        }

        // Append outgoing edge to `from`, incoming to `to`. Dedupe on (other, kind, direction).
        if let Some(mut from_entry) = self.records.get_mut(&edge.from) {
            let rec = from_entry.value_mut();
            let already = rec
                .relationships
                .iter()
                .any(|e| e.other == edge.to && e.kind == edge.kind && e.direction == EdgeDirection::Outgoing);
            if !already {
                rec.relationships.push(RelationshipEdge {
                    other: edge.to.clone(),
                    kind: edge.kind,
                    direction: EdgeDirection::Outgoing,
                });
            }
        }
        if let Some(mut to_entry) = self.records.get_mut(&edge.to) {
            let rec = to_entry.value_mut();
            let already = rec
                .relationships
                .iter()
                .any(|e| e.other == edge.from && e.kind == edge.kind && e.direction == EdgeDirection::Incoming);
            if !already {
                rec.relationships.push(RelationshipEdge {
                    other: edge.from.clone(),
                    kind: edge.kind,
                    direction: EdgeDirection::Incoming,
                });
            }
        }
        Ok(())
    }

    async fn groups(&self, scope: RegistryScope) -> Result<Vec<PersonaGroup>, RegistryError> {
        let groups: Vec<_> = match &scope {
            RegistryScope::All => self.groups.iter().map(|g| g.value().clone()).collect(),
            RegistryScope::Project(p) => {
                let p_str = p.to_string_lossy().into_owned();
                self.groups
                    .iter()
                    .filter(|g| g.value().project_id.as_deref() == Some(p_str.as_str()))
                    .map(|g| g.value().clone())
                    .collect()
            }
        };
        Ok(groups)
    }

    async fn create_group(
        &self,
        name: String,
        project_id: Option<String>,
    ) -> Result<PersonaGroup, RegistryError> {
        let collision = self.groups.iter().any(|g| {
            let v = g.value();
            v.name == name && v.project_id == project_id
        });
        if collision {
            return Err(RegistryError::DuplicateGroup { name, project_id });
        }
        let id: GroupId = new_id().into();
        let group = PersonaGroup::new(id.clone(), name, project_id);
        self.groups.insert(id, group.clone());
        Ok(group)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use pattern_core::constellation::PersonaStatus;

    fn active_record(id: &str) -> PersonaRecord {
        PersonaRecord::new(id, format!("{id} name"), PersonaStatus::Active)
    }

    fn draft_record(id: &str) -> PersonaRecord {
        PersonaRecord::new(id, format!("{id} name"), PersonaStatus::Draft)
    }

    // ── list(All) returns every seeded record ─────────────────────────────────

    #[tokio::test]
    async fn list_all_returns_all_seeded() {
        let reg = InMemoryConstellationRegistry::new();
        reg.seed(active_record("alice"));
        reg.seed(draft_record("bob"));
        reg.seed(active_record("charlie"));

        let results = reg.list(RegistryScope::All).await.unwrap();
        assert_eq!(results.len(), 3, "all 3 seeded records must be returned");
        let ids: Vec<_> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"alice"));
        assert!(ids.contains(&"bob"));
        assert!(ids.contains(&"charlie"));
    }

    // ── list(All) on empty registry returns empty vec ─────────────────────────

    #[tokio::test]
    async fn list_all_empty_registry() {
        let reg = InMemoryConstellationRegistry::new();
        let results = reg.list(RegistryScope::All).await.unwrap();
        assert!(results.is_empty());
    }

    // ── list(Project(p)) filters by project_attachments ──────────────────────

    #[tokio::test]
    async fn list_project_filters_by_attachment() {
        let project_path = PathBuf::from("/home/user/project-a");

        let reg = InMemoryConstellationRegistry::new();

        let mut alice = active_record("alice");
        alice.project_attachments.push(project_path.clone());
        reg.seed(alice);

        // Bob is not attached to the project.
        reg.seed(active_record("bob"));

        let mut charlie = active_record("charlie");
        charlie.project_attachments.push(project_path.clone());
        reg.seed(charlie);

        let all = reg.list(RegistryScope::All).await.unwrap();
        assert_eq!(all.len(), 3, "all 3 records returned by All scope");

        let filtered = reg
            .list(RegistryScope::Project(project_path))
            .await
            .unwrap();
        assert_eq!(filtered.len(), 2, "only 2 records attached to project");
        let ids: Vec<_> = filtered.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"alice"));
        assert!(ids.contains(&"charlie"));
        assert!(!ids.contains(&"bob"));
    }

    // ── get(id) returns Some/None correctly ───────────────────────────────────

    #[tokio::test]
    async fn get_returns_some_for_existing_id() {
        let reg = InMemoryConstellationRegistry::new();
        reg.seed(active_record("alice"));

        let result = reg.get(&"alice".into()).await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id.as_str(), "alice");
    }

    #[tokio::test]
    async fn get_returns_none_for_missing_id() {
        let reg = InMemoryConstellationRegistry::new();
        reg.seed(active_record("alice"));

        let result = reg.get(&"nobody".into()).await.unwrap();
        assert!(result.is_none());
    }

    // ── seed overwrites existing record ──────────────────────────────────────

    #[tokio::test]
    async fn seed_overwrites_existing_record() {
        let reg = InMemoryConstellationRegistry::new();
        reg.seed(active_record("alice"));

        // Overwrite with Draft status.
        reg.seed(draft_record("alice"));

        let result = reg.get(&"alice".into()).await.unwrap().unwrap();
        assert_eq!(
            result.status,
            PersonaStatus::Draft,
            "second seed must overwrite the first"
        );
    }

    // ── remove deletes the record ─────────────────────────────────────────────

    #[tokio::test]
    async fn remove_deletes_record() {
        let reg = InMemoryConstellationRegistry::new();
        reg.seed(active_record("alice"));
        assert_eq!(reg.len(), 1);

        let removed = reg.remove(&"alice".into());
        assert!(removed, "remove must return true for an existing record");
        assert_eq!(reg.len(), 0);

        let result = reg.get(&"alice".into()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn remove_returns_false_for_missing_record() {
        let reg = InMemoryConstellationRegistry::new();
        let removed = reg.remove(&"nobody".into());
        assert!(!removed);
    }

    // ── len and is_empty ──────────────────────────────────────────────────────

    #[test]
    fn len_and_is_empty() {
        let reg = InMemoryConstellationRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);

        reg.seed(active_record("a"));
        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 1);

        reg.seed(active_record("b"));
        assert_eq!(reg.len(), 2);
    }

    // ── Clone shares the same underlying map ──────────────────────────────────

    #[tokio::test]
    async fn clone_shares_underlying_storage() {
        let reg = InMemoryConstellationRegistry::new();
        reg.seed(active_record("alice"));

        let clone = reg.clone();
        clone.seed(active_record("bob"));

        // Both original and clone should see both records.
        let orig_results = reg.list(RegistryScope::All).await.unwrap();
        assert_eq!(orig_results.len(), 2, "original must see both records");

        let clone_results = clone.list(RegistryScope::All).await.unwrap();
        assert_eq!(clone_results.len(), 2, "clone must see both records");
    }

    // ── Phase 6: register / set_status / add_relationship / find / groups ─────

    #[tokio::test]
    async fn register_inserts_then_rejects_duplicates() {
        let reg = InMemoryConstellationRegistry::new();
        reg.register(active_record("alice")).await.unwrap();

        let err = reg.register(active_record("alice")).await.unwrap_err();
        assert!(
            matches!(err, RegistryError::DuplicatePersona(ref id) if id.as_str() == "alice"),
            "second register of same id must error with DuplicatePersona, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn set_status_updates_existing_and_errors_on_missing() {
        let reg = InMemoryConstellationRegistry::new();
        reg.register(active_record("alice")).await.unwrap();

        reg.set_status(&"alice".into(), PersonaStatus::Inactive)
            .await
            .unwrap();
        let updated = reg.get(&"alice".into()).await.unwrap().unwrap();
        assert_eq!(updated.status, PersonaStatus::Inactive);

        let err = reg
            .set_status(&"ghost".into(), PersonaStatus::Active)
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::PersonaNotFound(ref id) if id.as_str() == "ghost"));
    }

    #[tokio::test]
    async fn add_relationship_writes_outgoing_and_incoming_edges() {
        let reg = InMemoryConstellationRegistry::new();
        reg.register(active_record("alice")).await.unwrap();
        reg.register(active_record("bob")).await.unwrap();

        reg.add_relationship(RelationshipSpec::new(
            "alice",
            "bob",
            RelationshipKind::SupervisorOf,
        ))
        .await
        .unwrap();

        let alice = reg.get(&"alice".into()).await.unwrap().unwrap();
        let bob = reg.get(&"bob".into()).await.unwrap().unwrap();

        assert_eq!(alice.relationships.len(), 1);
        assert_eq!(alice.relationships[0].other.as_str(), "bob");
        assert_eq!(alice.relationships[0].direction, EdgeDirection::Outgoing);
        assert_eq!(alice.relationships[0].kind, RelationshipKind::SupervisorOf);

        assert_eq!(bob.relationships.len(), 1);
        assert_eq!(bob.relationships[0].other.as_str(), "alice");
        assert_eq!(bob.relationships[0].direction, EdgeDirection::Incoming);
        assert_eq!(bob.relationships[0].kind, RelationshipKind::SupervisorOf);
    }

    #[tokio::test]
    async fn add_relationship_dedupes_on_repeat() {
        let reg = InMemoryConstellationRegistry::new();
        reg.register(active_record("alice")).await.unwrap();
        reg.register(active_record("bob")).await.unwrap();

        let spec = RelationshipSpec::new("alice", "bob", RelationshipKind::PeerWith);
        reg.add_relationship(spec.clone()).await.unwrap();
        reg.add_relationship(spec).await.unwrap();

        let alice = reg.get(&"alice".into()).await.unwrap().unwrap();
        let bob = reg.get(&"bob".into()).await.unwrap().unwrap();
        assert_eq!(alice.relationships.len(), 1, "alice should have one outgoing edge after dedup");
        assert_eq!(bob.relationships.len(), 1, "bob should have one incoming edge after dedup");
    }

    #[tokio::test]
    async fn add_relationship_errors_when_endpoint_missing() {
        let reg = InMemoryConstellationRegistry::new();
        reg.register(active_record("alice")).await.unwrap();

        let err = reg
            .add_relationship(RelationshipSpec::new(
                "alice",
                "ghost",
                RelationshipKind::PeerWith,
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::PersonaNotFound(ref id) if id.as_str() == "ghost"));
    }

    #[tokio::test]
    async fn find_filters_by_project_and_kind() {
        let reg = InMemoryConstellationRegistry::new();

        let project = std::path::PathBuf::from("/home/user/proj-a");

        let mut alice = active_record("alice");
        alice.project_attachments.push(project.clone());
        reg.records.insert(alice.id.clone(), alice);

        let mut bob = active_record("bob");
        bob.project_attachments.push(project.clone());
        reg.records.insert(bob.id.clone(), bob);

        // Charlie is not attached to the project.
        reg.records
            .insert("charlie".into(), active_record("charlie"));

        // alice is supervisor_of bob.
        reg.add_relationship(RelationshipSpec::new(
            "alice",
            "bob",
            RelationshipKind::SupervisorOf,
        ))
        .await
        .unwrap();

        // project filter alone: alice + bob.
        let by_proj = reg.find(Some(project.as_path()), None).await.unwrap();
        let ids: Vec<_> = by_proj.iter().map(|r| r.id.as_str().to_string()).collect();
        assert_eq!(by_proj.len(), 2);
        assert!(ids.contains(&"alice".to_string()));
        assert!(ids.contains(&"bob".to_string()));

        // project + kind filter: alice (outgoing supervisor_of) and bob (incoming).
        let combined = reg
            .find(Some(project.as_path()), Some(RelationshipKind::SupervisorOf))
            .await
            .unwrap();
        assert_eq!(combined.len(), 2);

        // kind alone, no project filter: same 2 (charlie has no relationships).
        let by_kind = reg
            .find(None, Some(RelationshipKind::SupervisorOf))
            .await
            .unwrap();
        assert_eq!(by_kind.len(), 2);

        // PeerWith filter matches nothing.
        let by_other_kind = reg
            .find(None, Some(RelationshipKind::PeerWith))
            .await
            .unwrap();
        assert!(by_other_kind.is_empty());
    }

    #[tokio::test]
    async fn create_group_and_groups_scope_filtering() {
        let reg = InMemoryConstellationRegistry::new();

        let g1 = reg
            .create_group("support".into(), Some("proj-a".into()))
            .await
            .unwrap();
        assert_eq!(g1.name, "support");
        assert_eq!(g1.project_id.as_deref(), Some("proj-a"));
        assert!(g1.members.is_empty());

        let _g2 = reg
            .create_group("support".into(), Some("proj-b".into()))
            .await
            .unwrap();

        // Same name + same project_id collides.
        let err = reg
            .create_group("support".into(), Some("proj-a".into()))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            RegistryError::DuplicateGroup { ref name, project_id: Some(ref p) }
                if name == "support" && p == "proj-a"
        ));

        // groups(All) returns both.
        let all = reg.groups(RegistryScope::All).await.unwrap();
        assert_eq!(all.len(), 2);

        // groups(Project("/proj-a")) returns only the proj-a group.
        let by_proj = reg
            .groups(RegistryScope::Project(std::path::PathBuf::from("proj-a")))
            .await
            .unwrap();
        assert_eq!(by_proj.len(), 1);
        assert_eq!(by_proj[0].project_id.as_deref(), Some("proj-a"));
    }
}
