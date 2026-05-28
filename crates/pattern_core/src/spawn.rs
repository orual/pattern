// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
use smol_str::SmolStr;

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
    /// Haskell helper source compiled into a synthesized lib module the
    /// child can `import` from its eval-tool snippets. Treated as a
    /// `Pattern.SpawnHelpers` module — the runner writes it into a temp
    /// directory and adds that directory to the child's include path.
    /// Empty / blank values cause the runner to skip lib synthesis
    /// entirely.
    pub program: String,
    /// System-prompt override. The parent identity is retained in logs;
    /// the costume changes only the prompt presented to the model.
    pub costume: Option<String>,
    /// Capability restriction. `None` means inherit the parent's full set.
    /// Any capabilities listed here that exceed the parent's set are
    /// rejected as `SpawnError::CapabilityEscalation` at spawn time.
    pub capabilities: Option<CapabilitySet>,
    /// Execution time limit. `None` falls back to the runtime default.
    pub timeout: Option<jiff::Span>,
    /// Optional initial human-role prompt. When `Some`, the child's first
    /// `TurnInput` carries this as a single user message; when `None`,
    /// the child opens on `costume`/system-prompt alone with no human
    /// turn.
    pub prompt: Option<String>,
    /// Model override. When `Some`, the child uses this model instead of
    /// inheriting the parent's. When `None`, inherits.
    pub model_id: Option<smol_str::SmolStr>,
    /// Optional caller-supplied label for the spawn. Used as the suffix
    /// of the child's namespaced execution agent_id
    /// (`<parent>:spawn:<name>`). When `None` or blank, the suffix
    /// falls back to the auto-generated `spawn_id`.
    ///
    /// Multiple spawns sharing a name intentionally share an agent_id —
    /// name = group, spawn_id = instance. Distinct batch_ids preserve
    /// per-turn routing on the wire; storage aggregates same-named
    /// spawns under one history thread.
    ///
    /// Tidy values (matching `[a-zA-Z0-9_-]+`, ≤64 chars) recommended for
    /// readability in TUI sidebars and storage queries; the runtime does
    /// not currently enforce a regex.
    pub name: Option<String>,
}

impl EphemeralConfig {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            costume: None,
            capabilities: None,
            timeout: None,
            prompt: None,
            model_id: None,
            name: None,
        }
    }

    /// Set the spawn name (used as suffix of `<parent>:spawn:<name>`).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Override the system prompt with a costume string.
    pub fn with_costume(mut self, costume: impl Into<String>) -> Self {
        self.costume = Some(costume.into());
        self
    }

    /// Set the initial human-role prompt seeded into the child's first
    /// turn input.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
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
    /// Model override for the forked session.
    pub model_id: Option<smol_str::SmolStr>,
}

impl ForkConfig {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            isolation: ForkIsolation::Lightweight,
            capabilities: None,
            timeout_hint: None,
            task_ref: None,
            model_id: None,
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

    /// Set the model ID for the forked session.
    pub fn with_model(mut self, model: Option<SmolStr>) -> Self {
        self.model_id = model;
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
    /// Initial capability set.
    pub capabilities: CapabilitySet,
    /// Model override for the new persona.
    pub model_id: Option<smol_str::SmolStr>,
}

impl PersonaConfig {
    pub fn new(
        name: impl Into<String>,
        system_prompt: impl Into<String>,
        capabilities: CapabilitySet,
    ) -> Self {
        Self {
            name: name.into(),
            system_prompt: system_prompt.into(),
            capabilities,
            model_id: None,
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
            prompt: Some("hello".to_string()),
            model_id: None,
            name: Some("retrieval-helper".to_string()),
        };

        let json = serde_json::to_string(&original).expect("serialise must succeed");
        let recovered: EphemeralConfig =
            serde_json::from_str(&json).expect("deserialise must succeed");

        assert_eq!(recovered.program, original.program);
        assert_eq!(recovered.costume, original.costume);
        assert_eq!(recovered.capabilities, original.capabilities);
        assert!(recovered.timeout.is_none());
        assert_eq!(recovered.prompt.as_deref(), Some("hello"));
    }

    #[test]
    fn ephemeral_config_minimal_new() {
        let cfg = EphemeralConfig::new("pure ()");
        assert_eq!(cfg.program, "pure ()");
        assert!(cfg.costume.is_none());
        assert!(cfg.capabilities.is_none());
        assert!(cfg.timeout.is_none());
        assert!(cfg.prompt.is_none());
    }

    #[test]
    fn ephemeral_config_builder_methods() {
        let caps = sample_capability_set();
        let cfg = EphemeralConfig::new("pure ()")
            .with_costume("be terse")
            .with_capabilities(caps.clone())
            .with_prompt("focus on this task");

        assert_eq!(cfg.costume.as_deref(), Some("be terse"));
        assert_eq!(cfg.capabilities.as_ref(), Some(&caps));
        assert_eq!(cfg.prompt.as_deref(), Some("focus on this task"));
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
            model_id: None,
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
            model_id: None,
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

// ── SpawnSource ──────────────────────────────────────────────────────────────

/// Origin of a turn-event in the spawn graph.
///
/// Lives in `pattern_core` (rather than the wire-protocol crate) because
/// the runtime needs to talk about it: when a child session is spawned,
/// the parent's [`SpawnSinkFactory`](crate::traits::SpawnSinkFactory) is
/// consulted to mint the child's tagged turn-sink, and that requires
/// passing a `SpawnSource` through APIs that are below the wire layer.
///
/// The wire-protocol crate (`pattern_server`) re-exports this type so
/// existing call sites continue to spell it as
/// `pattern_server::protocol::SpawnSource`.
///
/// `Main` is the default for back-compat: any tagged event that doesn't
/// explicitly carry a source is treated as primary-conversation output.
/// Bridges for ephemeral / sibling / fork batches set the appropriate
/// variant at construction time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SpawnSource {
    /// Top-level agent turn — the primary conversation transcript.
    #[default]
    Main,
    /// Ephemeral worker spawned via `Pattern.Spawn.Ephemeral`. Lives
    /// for one bounded task and disappears.
    ///
    /// The execution agent_id of an ephemeral is namespaced as
    /// `<parent_agent_id>:spawn:<spawn_id>` so storage and routing
    /// naturally distinguish spawn batches from parent batches without
    /// schema migrations or special-cased queries.
    Ephemeral {
        /// Stable id of the ephemeral child for this turn.
        spawn_id: String,
        /// Agent id of the parent that spawned this child. Used by
        /// TUI consumers to group spawn entries under the correct
        /// parent's sidebar.
        parent_agent_id: String,
        /// Memory block label where the child's progress log is being
        /// appended; lets the TUI link the sidebar entry to the
        /// persistent record after the spawn completes.
        progress_log_label: String,
    },
    /// Sibling persona — a peer agent in the same constellation.
    Sibling {
        /// Persona id of the sibling whose turn this event belongs to.
        persona_id: String,
        /// Agent id of the parent that spawned this sibling.
        parent_agent_id: String,
    },
    /// Fork — an isolated copy of an agent that runs concurrently.
    Fork {
        /// Stable id of the fork.
        fork_id: String,
        /// Agent id of the parent the fork was branched from.
        parent_agent_id: String,
    },
}
