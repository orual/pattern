//! Spawn-config types for multi-agent session management.
//!
//! These types describe the three kinds of child sessions an agent may
//! request through the `Spawn` effect:
//!
//! - **Ephemeral** — a short-lived worker that inherits (a subset of) the
//!   parent's capabilities and produces a single result. Cancelled when the
//!   parent resolves.
//! - **Fork** — a copy of the parent's memory state, run in isolation. Phase 2
//!   delivers the lightweight path only; persistent isolation (jj workspace)
//!   lands in Phase 3.
//! - **Sibling** — a fully independent session with its own persona and
//!   `CapabilitySet`. Lives beyond the parent's lifetime.
//!
//! All structs are `#[non_exhaustive]` so that future fields can be added
//! without a major semver bump on downstream crates.

use serde::{Deserialize, Serialize};

use crate::types::ids::PersonaId;
use crate::{BlockRef, CapabilitySet};

// ── Ephemeral ────────────────────────────────────────────────────────────────

/// Config for an ephemeral child session.
///
/// An ephemeral executes `program` to completion and returns a result. Its
/// lifetime is strictly bounded by the parent session — when the parent
/// resolves, all ephemeral children are cancelled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EphemeralConfig {
    /// Haskell source to compile and run inside the child session.
    pub program: String,
    /// System-prompt override. The parent identity is retained in logs;
    /// the costume changes only the prompt presented to the model.
    pub costume: Option<String>,
    /// Capability restriction. `None` means inherit the parent's full set.
    /// Any capabilities listed here that exceed the parent's set are silently
    /// clamped to the intersection at spawn time.
    pub capabilities: Option<CapabilitySet>,
    /// Execution time limit. `None` falls back to the runtime default.
    pub timeout: Option<jiff::Span>,
    /// Caller-supplied tags forwarded to the structured log sink.
    pub metadata: serde_json::Value,
}

impl EphemeralConfig {
    /// Construct an ephemeral config with sensible defaults.
    ///
    /// Sets `costume`, `capabilities`, and `timeout` to `None`; `metadata`
    /// to `serde_json::Value::Null`.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            costume: None,
            capabilities: None,
            timeout: None,
            metadata: serde_json::Value::Null,
        }
    }

    /// Override the system prompt with a costume string.
    pub fn with_costume(mut self, costume: impl Into<String>) -> Self {
        self.costume = Some(costume.into());
        self
    }

    /// Restrict capabilities to the given set.
    ///
    /// At spawn time the runtime further clamps this to the parent's own set,
    /// so escalation is impossible even if the caller passes a broad set here.
    pub fn with_capabilities(mut self, caps: CapabilitySet) -> Self {
        self.capabilities = Some(caps);
        self
    }

    /// Set an execution time limit.
    pub fn with_timeout(mut self, span: jiff::Span) -> Self {
        self.timeout = Some(span);
        self
    }

    /// Attach caller-supplied metadata for log correlation.
    pub fn with_metadata(mut self, meta: serde_json::Value) -> Self {
        self.metadata = meta;
        self
    }
}

// ── Fork ─────────────────────────────────────────────────────────────────────

/// Config for a forked child session.
///
/// A fork inherits the parent's memory state and runs a separate program
/// with it. Phase 2 supports `ForkIsolation::Lightweight` only; the
/// `Persistent` variant wires through in Phase 3.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ForkConfig {
    /// Haskell source to run in the forked context.
    pub program: String,
    /// Isolation mode for the fork's memory state.
    pub isolation: ForkIsolation,
    /// Optional capability restriction, clamped to parent at spawn time.
    pub capabilities: Option<CapabilitySet>,
    /// Advisory timeout. Phase 3 uses this for jj commit naming heuristics.
    pub timeout_hint: Option<jiff::Span>,
    /// Memory-block reference used for jj bookmark naming in Phase 3.
    /// No effect in Phase 2.
    pub task_ref: Option<BlockRef>,
}

impl ForkConfig {
    /// Construct a fork config with lightweight isolation.
    ///
    /// Sets `capabilities`, `timeout_hint`, and `task_ref` to `None`.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            isolation: ForkIsolation::Lightweight,
            capabilities: None,
            timeout_hint: None,
            task_ref: None,
        }
    }

    /// Use persistent (jj workspace) isolation. Phase 3 wires the full
    /// semantics; Phase 2 returns a "not yet wired" handler error.
    pub fn persistent(mut self) -> Self {
        self.isolation = ForkIsolation::Persistent;
        self
    }

    /// Restrict capabilities to the given set.
    pub fn with_capabilities(mut self, caps: CapabilitySet) -> Self {
        self.capabilities = Some(caps);
        self
    }

    /// Set an advisory timeout hint for jj bookmark naming.
    pub fn with_timeout_hint(mut self, span: jiff::Span) -> Self {
        self.timeout_hint = Some(span);
        self
    }

    /// Associate a memory block reference for jj bookmark naming.
    pub fn with_task_ref(mut self, block_ref: BlockRef) -> Self {
        self.task_ref = Some(block_ref);
        self
    }
}

/// Memory isolation mode for a forked session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ForkIsolation {
    /// In-memory copy via `LoroDoc::fork()`. Fast; no disk writes.
    Lightweight,
    /// jj workspace on disk. Enables `merge_back` / `promote`. Phase 3 only.
    Persistent,
}

// ── Sibling ──────────────────────────────────────────────────────────────────

/// Config for spawning a sibling session with an independent persona.
///
/// Unlike ephemeral and fork children, a sibling is NOT tracked by the
/// spawner's `SpawnRegistry`. It lives beyond the parent's lifetime and
/// carries its own `CapabilitySet` derived from the persona config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SiblingConfig {
    /// Which persona to open the sibling as.
    pub persona: SiblingPersona,
    /// The semantic relationship between the spawning agent and the sibling.
    pub relationship: RelationshipKind,
    /// Labels of memory blocks the sibling may read from the spawner.
    ///
    /// Memory ACL enforcement for these references lands in Phase 6; the
    /// list is recorded here so Phase 6 can honour it without a schema change.
    pub shared_blocks: Vec<String>,
}

impl SiblingConfig {
    /// Construct a minimal sibling config with no shared blocks.
    pub fn new(persona: SiblingPersona, relationship: RelationshipKind) -> Self {
        Self {
            persona,
            relationship,
            shared_blocks: Vec::new(),
        }
    }

    /// Add memory block labels that the sibling may read.
    pub fn with_shared_blocks(
        mut self,
        labels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.shared_blocks = labels.into_iter().map(Into::into).collect();
        self
    }
}

/// Discriminates whether the sibling uses an existing persona or creates one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SiblingPersona {
    /// Open a session for a persona that already exists in the registry.
    Existing(PersonaId),
    /// Create a new persona. Whether a live session is immediately opened
    /// depends on whether the spawner holds the `SpawnNewIdentities`
    /// `CapabilityFlag` (see Phase 2 Task 7).
    New(PersonaConfig),
}

/// Minimal persona descriptor used when spawning a new sibling identity.
///
/// The full `PersonaSnapshot` used at runtime is a superset of this struct;
/// additional fields (memory blocks, wake conditions, etc.) are populated
/// by Phase 6 registry work. `PersonaConfig` is only the seed a spawning
/// agent provides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PersonaConfig {
    /// Human-readable persona name.
    pub name: String,
    /// System prompt for the new persona.
    pub system_prompt: String,
    /// Initial capability set. The spawner cannot grant capabilities it does
    /// not itself hold (enforcement in Phase 2 Task 7 spawn handler).
    pub capabilities: CapabilitySet,
    // Further fields deferred to Phase 6 registry work.
}

impl PersonaConfig {
    /// Construct a persona config with the minimal required fields.
    pub fn new(
        name: impl Into<String>,
        system_prompt: impl Into<String>,
        capabilities: CapabilitySet,
    ) -> Self {
        Self {
            name: name.into(),
            system_prompt: system_prompt.into(),
            capabilities,
        }
    }
}

// ── RelationshipKind ─────────────────────────────────────────────────────────

/// The semantic relationship between the spawning agent and a sibling.
///
/// Used for display and structured logging; no behavioural semantics are
/// attached in Phase 2. Phase 6 may use these to drive coordination routing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RelationshipKind {
    /// The spawner acts as supervisor over the sibling.
    SupervisorOf,
    /// The sibling is a specialist called in to handle a narrow task.
    SpecialistFor,
    /// Both are peers collaborating on equal footing.
    PeerWith,
    /// The sibling observes but does not act.
    ObserverOf,
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::capability::{CapabilityFlag, EffectCategory};

    fn sample_capability_set() -> CapabilitySet {
        [EffectCategory::Memory, EffectCategory::Spawn]
            .into_iter()
            .collect::<CapabilitySet>()
    }

    // ── EphemeralConfig ──────────────────────────────────────────────────────

    #[test]
    fn ephemeral_config_serde_round_trip() {
        let original = EphemeralConfig {
            program: "pure ()".to_string(),
            costume: Some("be terse".to_string()),
            capabilities: Some(sample_capability_set()),
            timeout: None,
            metadata: json!({"source": "test"}),
        };

        let json = serde_json::to_string(&original).expect("serialise must succeed");
        let recovered: EphemeralConfig =
            serde_json::from_str(&json).expect("deserialise must succeed");

        assert_eq!(recovered.program, original.program);
        assert_eq!(recovered.costume, original.costume);
        assert_eq!(recovered.capabilities, original.capabilities);
        assert!(recovered.timeout.is_none());
        assert_eq!(recovered.metadata, original.metadata);
    }

    #[test]
    fn ephemeral_config_minimal_new() {
        let cfg = EphemeralConfig::new("pure ()");
        assert_eq!(cfg.program, "pure ()");
        assert!(cfg.costume.is_none());
        assert!(cfg.capabilities.is_none());
        assert!(cfg.timeout.is_none());
        assert_eq!(cfg.metadata, serde_json::Value::Null);
    }

    #[test]
    fn ephemeral_config_builder_methods() {
        let caps = sample_capability_set();
        let cfg = EphemeralConfig::new("pure ()")
            .with_costume("be terse")
            .with_capabilities(caps.clone())
            .with_metadata(json!({"tag": "v1"}));

        assert_eq!(cfg.costume.as_deref(), Some("be terse"));
        assert_eq!(cfg.capabilities.as_ref(), Some(&caps));
        assert_eq!(cfg.metadata, json!({"tag": "v1"}));
    }

    #[test]
    fn ephemeral_config_null_metadata_round_trip() {
        let cfg = EphemeralConfig::new("pure ()");
        let json = serde_json::to_string(&cfg).expect("serialise must succeed");
        let back: EphemeralConfig = serde_json::from_str(&json).expect("deserialise must succeed");
        assert_eq!(back.metadata, serde_json::Value::Null);
    }

    // ── ForkConfig ───────────────────────────────────────────────────────────

    #[test]
    fn fork_config_serde_round_trip() {
        let original = ForkConfig {
            program: "pure ()".to_string(),
            isolation: ForkIsolation::Lightweight,
            capabilities: None,
            timeout_hint: None,
            task_ref: Some(BlockRef::new("planning", "block-abc")),
        };

        let json = serde_json::to_string(&original).expect("serialise must succeed");
        let recovered: ForkConfig = serde_json::from_str(&json).expect("deserialise must succeed");

        assert_eq!(recovered.program, original.program);
        assert_eq!(recovered.isolation, ForkIsolation::Lightweight);
        assert_eq!(
            recovered.task_ref.as_ref().map(|r| r.label.as_str()),
            Some("planning")
        );
    }

    #[test]
    fn fork_config_persistent_isolation_round_trip() {
        let cfg = ForkConfig::new("pure ()").persistent();
        let json = serde_json::to_string(&cfg).expect("serialise must succeed");
        let back: ForkConfig = serde_json::from_str(&json).expect("deserialise must succeed");
        assert_eq!(back.isolation, ForkIsolation::Persistent);
    }

    // ── ForkIsolation ────────────────────────────────────────────────────────

    #[test]
    fn fork_isolation_debug_and_partial_eq() {
        assert_eq!(ForkIsolation::Lightweight, ForkIsolation::Lightweight);
        assert_ne!(ForkIsolation::Lightweight, ForkIsolation::Persistent);
        // Debug is derived; sanity check it produces something reasonable.
        let s = format!("{:?}", ForkIsolation::Persistent);
        assert!(s.contains("Persistent"), "debug output was: {s}");
    }

    #[test]
    fn fork_isolation_serde_round_trip() {
        for variant in [ForkIsolation::Lightweight, ForkIsolation::Persistent] {
            let json = serde_json::to_string(&variant).expect("serialise must succeed");
            let back: ForkIsolation =
                serde_json::from_str(&json).expect("deserialise must succeed");
            assert_eq!(back, variant);
        }
    }

    // ── SiblingConfig ────────────────────────────────────────────────────────

    #[test]
    fn sibling_config_existing_persona_round_trip() {
        let original = SiblingConfig {
            persona: SiblingPersona::Existing("orual".into()),
            relationship: RelationshipKind::PeerWith,
            shared_blocks: vec!["planning".to_string()],
        };

        let json = serde_json::to_string(&original).expect("serialise must succeed");
        let recovered: SiblingConfig =
            serde_json::from_str(&json).expect("deserialise must succeed");

        assert_eq!(recovered.relationship, RelationshipKind::PeerWith);
        assert_eq!(recovered.shared_blocks, vec!["planning"]);
        match recovered.persona {
            SiblingPersona::Existing(id) => assert_eq!(id.as_str(), "orual"),
            SiblingPersona::New(_) => panic!("expected Existing variant"),
        }
    }

    #[test]
    fn sibling_config_new_persona_round_trip() {
        let persona_cfg = PersonaConfig::new(
            "helper",
            "you are a helpful assistant",
            sample_capability_set(),
        );
        let original = SiblingConfig::new(
            SiblingPersona::New(persona_cfg),
            RelationshipKind::SpecialistFor,
        );

        let json = serde_json::to_string(&original).expect("serialise must succeed");
        let recovered: SiblingConfig =
            serde_json::from_str(&json).expect("deserialise must succeed");

        assert_eq!(recovered.relationship, RelationshipKind::SpecialistFor);
        assert!(recovered.shared_blocks.is_empty());
        match recovered.persona {
            SiblingPersona::New(cfg) => {
                assert_eq!(cfg.name, "helper");
                assert_eq!(cfg.system_prompt, "you are a helpful assistant");
            }
            SiblingPersona::Existing(_) => panic!("expected New variant"),
        }
    }

    // ── PersonaConfig ────────────────────────────────────────────────────────

    #[test]
    fn persona_config_serde_round_trip() {
        let original = PersonaConfig {
            name: "orual".to_string(),
            system_prompt: "you are an executive function assistant".to_string(),
            capabilities: sample_capability_set(),
        };

        let json = serde_json::to_string(&original).expect("serialise must succeed");
        let recovered: PersonaConfig =
            serde_json::from_str(&json).expect("deserialise must succeed");

        assert_eq!(recovered.name, original.name);
        assert_eq!(recovered.system_prompt, original.system_prompt);
        assert_eq!(recovered.capabilities, original.capabilities);
    }

    #[test]
    fn persona_config_with_flag_round_trip() {
        let caps = std::iter::once(EffectCategory::Spawn)
            .collect::<CapabilitySet>()
            .with_flags([CapabilityFlag::SpawnNewIdentities]);
        let cfg = PersonaConfig::new("identity-agent", "spawn identities freely", caps.clone());

        let json = serde_json::to_string(&cfg).expect("serialise must succeed");
        let back: PersonaConfig = serde_json::from_str(&json).expect("deserialise must succeed");

        assert!(
            back.capabilities
                .has_flag(CapabilityFlag::SpawnNewIdentities),
            "SpawnNewIdentities flag must survive round-trip"
        );
    }

    // ── RelationshipKind ─────────────────────────────────────────────────────

    #[test]
    fn relationship_kind_debug_and_partial_eq() {
        assert_eq!(
            RelationshipKind::SupervisorOf,
            RelationshipKind::SupervisorOf
        );
        assert_ne!(RelationshipKind::SupervisorOf, RelationshipKind::PeerWith);

        let s = format!("{:?}", RelationshipKind::ObserverOf);
        assert!(s.contains("ObserverOf"), "debug output was: {s}");
    }

    #[test]
    fn relationship_kind_all_variants_round_trip() {
        for variant in [
            RelationshipKind::SupervisorOf,
            RelationshipKind::SpecialistFor,
            RelationshipKind::PeerWith,
            RelationshipKind::ObserverOf,
        ] {
            let json = serde_json::to_string(&variant).expect("serialise must succeed");
            let back: RelationshipKind =
                serde_json::from_str(&json).expect("deserialise must succeed");
            assert_eq!(back, variant);
        }
    }
}
