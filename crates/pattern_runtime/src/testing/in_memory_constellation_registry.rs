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
    ConstellationRegistry, PersonaRecord, RegistryError, RegistryScope,
};

/// Thread-safe in-memory `ConstellationRegistry` implementation.
///
/// All methods are non-blocking. Use `seed` to populate before tests and
/// `get_record` / `remove` to inspect or mutate state mid-test.
#[derive(Debug, Default, Clone)]
pub struct InMemoryConstellationRegistry {
    records: Arc<DashMap<PersonaId, PersonaRecord>>,
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
}
