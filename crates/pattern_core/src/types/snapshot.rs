// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Persona snapshot — unified type consumed by
//! [`crate::traits::AgentRuntime::open_session`] and returned by
//! `Session::checkpoint`.
//!
//! Earlier drafts of the foundation plan distinguished `PersonaConfig`
//! (spawn-time) from `PersonaSnapshot` (restore-time). In practice both
//! carry the same bag of persona state; the only difference was a
//! checkpoint cursor. [`PersonaSnapshot`] is now the single type; fresh
//! spawns construct it with `as_of_turn = None`, post-turn checkpoints
//! overwrite with `Some(turn_id)`.
//!
//! ## Agent programs
//!
//! Agent programs are not stored in `PersonaSnapshot`. Code-tool snippets are
//! compiled on demand per turn by the `EvalWorker` inside the agent loop.
//! The legacy static-program field was removed in Phase 6 Task B.
//!
//! ## Structured content lives in memory blocks
//!
//! Custom per-persona content — persona text, instructions, working notes
//! — is carried as [`MemoryBlockSpec`] entries under `memory_blocks`, not
//! as top-level fields. The persona's identity paragraph, for example,
//! is typically a memory block at the label `"persona"` with type
//! [`MemoryType::Core`].
//!
//! Exception: [`PersonaSnapshot::system_prompt`] is first-class because
//! it replaces [`pattern_provider`'s `DEFAULT_BASE_INSTRUCTIONS`](../../../../pattern_provider/shaper/fn.build_system_prompt.html)
//! in slot \[1\] of the three-segment cache layout when `Some`, and needs
//! to be a distinct field so the shaper can see it without walking memory.
//!
//! ## Forward compatibility
//!
//! Most nested structs carry `#[non_exhaustive]` so future fields can be
//! added without breaking external construction. [`MemoryBlockSpec`] reserves
//! a `crdt_snapshot: Option<Vec<u8>>` slot for future full-CRDT checkpoint
//! restore; it's always `None` in the current code path (the `MemoryStore`
//! synthesizes a fresh `LoroDoc` from `content` on restore).
//!
//! ## What's out of scope for foundation
//!
//! - Archival entries. They live in `pattern_db`'s archival table and are
//!   reopened transparently when the store attaches to the same `data_dir`.
//!   Full-state export to a portable format (future `CAR`-file work) is a
//!   separate plan.
//! - Tool rules. v2 had a 13-variant rule enum; the granularity wasn't
//!   useful and code execution doesn't fit that model. Dropped; revisit
//!   only if a clear need surfaces.
//! - Data sources / plugin-scope fields (`bluesky_handle`, Discord, file
//!   watchers). They'll land once the plugin system does.
//! - Model routing. [`PersonaSnapshot::router`] is a reserved opaque slot;
//!   shape is intentionally unspecified until we have a concrete routing
//!   story.

use std::collections::HashMap;
use std::path::PathBuf;

use genai::adapter::AdapterKind;
use genai::chat::ChatOptions;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::types::compression::CompressionStrategy;
use crate::types::ids::{AgentId, MemoryId};
use crate::types::memory_types::{BlockSchema, MemoryPermission, MemoryType};
use crate::types::message::SnapshotPolicy;
use crate::types::turn::TurnId;

// ==========================================================================
// Top-level PersonaSnapshot
// ==========================================================================

/// Everything the runtime needs to open (or resume) a single agent's
/// session.
///
/// Construct a fresh spawn via [`PersonaSnapshot::new`] plus builder-style
/// setters. `as_of_turn` is `None` for fresh spawns and overwritten when
/// a session is checkpointed.
///
/// # Examples
///
/// ```
/// use pattern_core::types::snapshot::PersonaSnapshot;
///
/// let snap = PersonaSnapshot::new("orual-companion", "Companion")
///     .with_wall_budget_ms(30_000);
/// assert_eq!(snap.agent_id.as_str(), "orual-companion");
/// assert!(snap.as_of_turn.is_none());
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaSnapshot {
    /// Stable identifier for this agent.
    pub agent_id: AgentId,

    /// Human-readable name for logs / display.
    pub name: SmolStr,

    /// Checkpoint cursor. `None` for fresh spawn; `Some(turn_id)` after
    /// the first turn of a restored session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_turn: Option<TurnId>,

    /// Wall-clock time this snapshot was captured. For fresh spawns,
    /// the construction time.
    #[serde(default = "Timestamp::now")]
    pub captured_at: Timestamp,

    /// Schema version for forward-compatibility checks. Starts at `1`.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    // -- Content ---------------------------------------------------------
    /// Slot \[1\] content override. When `Some`, replaces
    /// [`pattern_provider`]'s `DEFAULT_BASE_INSTRUCTIONS` in the
    /// three-segment cache layout's base-instructions slot. Cache-friendly
    /// because slot \[1\] is latched at session open and doesn't change
    /// mid-session.
    ///
    /// [`pattern_provider`]: ../../../../pattern_provider/index.html
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    /// Initial memory blocks, keyed by label. Persona text, custom
    /// instructions, working notes — all live here as
    /// [`MemoryBlockSpec`] entries.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub memory_blocks: HashMap<SmolStr, MemoryBlockSpec>,

    // -- Runtime policy --------------------------------------------------
    /// Which model the runtime should dial per request, plus sampling
    /// and reasoning parameters.
    #[serde(default)]
    pub model: ModelSpec,

    /// Reserved slot for a future model-router type. Shape is
    /// intentionally unspecified here; when routing lands, this field
    /// will become typed. Today, populating it is a no-op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<serde_json::Value>,

    /// Message history and snapshot policy for this persona.
    #[serde(default)]
    pub context: ContextPolicy,

    /// Tidepool JIT budgets and nursery size. `None` fields fall back
    /// to runtime defaults.
    #[serde(default)]
    pub budgets: RuntimeBudgets,

    // -- Capabilities + policy ------------------------------------------
    /// Capability scoping for this persona's session — which effect
    /// categories the agent's prelude exposes, plus orthogonal flag
    /// gates. `None` means "full power" (back-compat for personas that
    /// pre-date capability scoping).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<crate::CapabilitySet>,

    /// KDL-loaded policy rules (Phase 1 Task 13). Layered with
    /// `Precedence::KdlConfig` over the runtime's Rust defaults at
    /// session open. Rules constructed via the `PolicyRule::new`
    /// builder; the runtime guarantees these arrive at the correct
    /// precedence regardless of what the KDL author writes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_rules: Vec<crate::PolicyRule>,

    // -- Escape hatch ----------------------------------------------------
    /// Free-form extra metadata that hasn't earned a first-class field
    /// yet. Intended for experiments and plugin-scope configuration.
    /// Should not be load-bearing for foundation code paths.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extra: serde_json::Value,

    // -- Session-state serialization ------------------------------------
    /// MCP server configs loaded from persona KDL. These are merged with
    /// plugin-sourced configs at session open and fed to McpRegistry.
    ///
    /// Feature-gated on `mcp-client` to match the `crate::mcp` module's
    /// gating; on builds without that feature, snapshots serialize without
    /// this field (and skip-if-empty keeps existing snapshots compatible).
    #[cfg(feature = "mcp-client")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<crate::mcp::McpServerConfig>,

    /// File paths the agent had open at snapshot time. On restore, these
    /// are re-opened with fresh LoroDocs — no LoroDoc state persists
    /// across snapshot boundaries (loro docs are ephemeral per design).
    ///
    /// Uses `#[serde(default)]` so old snapshots that pre-date this
    /// field deserialize cleanly with an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_files: Vec<PathBuf>,
}

fn default_schema_version() -> u32 {
    1
}

impl PersonaSnapshot {
    /// Build a minimal snapshot with only the required fields.
    ///
    /// Agent programs are not stored here — code-tool snippets are compiled
    /// on demand per turn by the `EvalWorker` inside the agent loop.
    pub fn new(agent_id: impl Into<AgentId>, name: impl Into<SmolStr>) -> Self {
        Self {
            agent_id: agent_id.into(),
            name: name.into(),
            as_of_turn: None,
            captured_at: Timestamp::now(),
            schema_version: 1,
            system_prompt: None,
            memory_blocks: HashMap::new(),
            model: ModelSpec::default(),
            router: None,
            context: ContextPolicy::default(),
            budgets: RuntimeBudgets::default(),
            capabilities: None,
            policy_rules: Vec::new(),
            extra: serde_json::Value::Null,
            #[cfg(feature = "mcp-client")]
            mcp_servers: Vec::new(),
            open_files: Vec::new(),
        }
    }

    /// Set the persona's capability scoping. Pass `None` for "full
    /// power" — the back-compat default.
    pub fn with_capabilities(mut self, capabilities: Option<crate::CapabilitySet>) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Replace the persona-level policy rule list. Rules are merged
    /// over Rust defaults at session open with `Precedence::KdlConfig`.
    pub fn with_policy_rules<I: IntoIterator<Item = crate::PolicyRule>>(
        mut self,
        rules: I,
    ) -> Self {
        self.policy_rules = rules.into_iter().collect();
        self
    }

    /// Set the per-turn wall-clock budget in milliseconds.
    pub fn with_wall_budget_ms(mut self, ms: u64) -> Self {
        self.budgets.wall_ms = Some(ms);
        self
    }

    /// Set the per-turn CPU budget in milliseconds.
    pub fn with_cpu_budget_ms(mut self, ms: u64) -> Self {
        self.budgets.cpu_ms = Some(ms);
        self
    }

    /// Set the additional milliseconds of runaway compute tolerated
    /// beyond the CPU budget before hard-abandonment fires.
    pub fn with_hard_abandon_ms(mut self, ms: u64) -> Self {
        self.budgets.hard_abandon_ms = Some(ms);
        self
    }

    /// Set the post-hard-abandon grace window in milliseconds.
    pub fn with_cancel_grace_ms(mut self, ms: u64) -> Self {
        self.budgets.cancel_grace_ms = Some(ms);
        self
    }

    /// Set the JIT nursery size in bytes.
    pub fn with_nursery_size(mut self, bytes: usize) -> Self {
        self.budgets.nursery_size = Some(bytes);
        self
    }

    /// Attach free-form extra metadata.
    pub fn with_extra(mut self, extra: serde_json::Value) -> Self {
        self.extra = extra;
        self
    }

    /// Set the open-file paths to restore when this snapshot is loaded.
    ///
    /// Each path will be re-opened via the session's `FileManager` on
    /// restore. Files that no longer exist are skipped with a warning.
    pub fn with_open_files(mut self, paths: Vec<PathBuf>) -> Self {
        self.open_files = paths;
        self
    }

    /// Set the custom slot-\[1\] system prompt.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Add a memory block to the initial block set.
    pub fn with_memory_block(mut self, label: impl Into<SmolStr>, spec: MemoryBlockSpec) -> Self {
        self.memory_blocks.insert(label.into(), spec);
        self
    }

    /// Override the model specification.
    pub fn with_model(mut self, model: ModelSpec) -> Self {
        self.model = model;
        self
    }

    /// Override the context policy.
    pub fn with_context_policy(mut self, context: ContextPolicy) -> Self {
        self.context = context;
        self
    }
}

// ==========================================================================
// Memory block spec
// ==========================================================================

/// Initial specification for one memory block. At session open time, the
/// runtime constructs a [`StructuredDocument`] from this spec, feeding
/// `content` through [`StructuredDocument::import_from_json`] which
/// dispatches by schema:
///
/// - [`BlockSchema::Text`] — `content` as `String` (or object with
///   `content` key).
/// - [`BlockSchema::Map`] — `content` as object with field values.
/// - [`BlockSchema::List`] — `content` as array (or object with `items`).
/// - [`BlockSchema::Log`] — `content` as array of entries.
/// - [`BlockSchema::Composite`] — `content` as object with section keys.
///
/// Hence `content: serde_json::Value` rather than `String` — the block
/// isn't flat text unless the schema says so.
///
/// [`StructuredDocument`]: crate::memory::StructuredDocument
/// [`StructuredDocument::import_from_json`]: crate::memory::StructuredDocument::import_from_json
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlockSpec {
    /// Initial content, shape-dispatched by `schema`. Ignored when
    /// `crdt_snapshot` is `Some` (snapshot wins).
    #[serde(default)]
    pub content: serde_json::Value,

    /// Memory tier. See [`MemoryType`].
    #[serde(default)]
    pub memory_type: MemoryType,

    /// Permission level applied to this block.
    #[serde(default)]
    pub permission: MemoryPermission,

    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Whether the block stays in context unconditionally (pinned) vs.
    /// being eligible for eviction.
    #[serde(default)]
    pub pinned: bool,

    /// Maximum content size in characters. `None` = use runtime default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_limit: Option<usize>,

    /// Structural schema. `None` defaults to `BlockSchema::text()` at
    /// load time — fine for the simple inline-text case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<BlockSchema>,

    /// When `Some`, this block is a reference to a shared block owned
    /// by another agent. The store resolves the reference at load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_id: Option<MemoryId>,

    /// Full Loro CRDT snapshot bytes. When `Some`, restore reconstructs
    /// the `LoroDoc` verbatim (including undo/redo history) and ignores
    /// `content`. Reserved slot for future full-state checkpointing;
    /// always `None` in foundation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crdt_snapshot: Option<Vec<u8>>,
}

impl Default for MemoryBlockSpec {
    fn default() -> Self {
        Self {
            content: serde_json::Value::Null,
            memory_type: MemoryType::default(),
            permission: MemoryPermission::default(),
            description: None,
            pinned: false,
            char_limit: None,
            schema: None,
            shared_id: None,
            crdt_snapshot: None,
        }
    }
}

impl MemoryBlockSpec {
    /// Convenience: construct a text block with inline string content.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: serde_json::Value::String(content.into()),
            ..Self::default()
        }
    }

    pub fn with_memory_type(mut self, ty: MemoryType) -> Self {
        self.memory_type = ty;
        self
    }

    pub fn with_permission(mut self, p: MemoryPermission) -> Self {
        self.permission = p;
        self
    }

    pub fn with_description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    pub fn with_pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    pub fn with_char_limit(mut self, limit: usize) -> Self {
        self.char_limit = Some(limit);
        self
    }

    pub fn with_schema(mut self, schema: BlockSchema) -> Self {
        self.schema = Some(schema);
        self
    }

    pub fn with_shared_id(mut self, id: impl Into<MemoryId>) -> Self {
        self.shared_id = Some(id.into());
        self
    }
}

// ==========================================================================
// Model spec
// ==========================================================================

/// Per-persona model selection plus sampling / reasoning parameters.
///
/// Reuses `genai`'s [`ChatOptions`] for the sampling-and-reasoning surface
/// so we don't redefine `temperature` / `top_p` / `reasoning_effort` /
/// `verbosity` / etc. Streaming-capture fields on `ChatOptions`
/// (`capture_usage` and friends) and `extra_headers` are owned by the
/// runtime and shaper respectively; settings on them here are ignored.
///
/// Capability flags that currently live on [`ShaperConfig`]
/// (`enable_interleaved_thinking`, `enable_extended_cache_ttl`,
/// `enable_1m_context`, etc.) stay workspace-wide rather than per-persona
/// — the auth-tier-to-shaper relationship is 1:1 in practice, so
/// capability envelopes are instance-scoped.
///
/// [`ChatOptions`]: genai::chat::ChatOptions
/// [`ShaperConfig`]: ../../../../pattern_provider/shaper/struct.ShaperConfig.html
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Which provider + model the request routes to.
    pub choice: ModelChoice,

    /// Sampling and reasoning parameters. Passed through to `genai`
    /// per request; defaults = "use the library's defaults."
    #[serde(default)]
    pub chat_options: ChatOptions,

    /// Narrow per-provider overrides for behaviour that doesn't fit
    /// cleanly into [`ChatOptions`]. Kept as empty typed structs for
    /// now and grown on demand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_overrides: Option<AnthropicOverrides>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_overrides: Option<OpenAIOverrides>,
}

/// A single model selection (provider + model id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelChoice {
    /// Which `genai` adapter handles this model. Reuses the upstream
    /// [`AdapterKind`] enum so pattern_core doesn't redefine the same
    /// provider list.
    pub provider: AdapterKind,

    /// Provider-specific model identifier — e.g. `"claude-sonnet-4-6"`,
    /// `"gemini-2.5-flash"`, `"gpt-5"`. `genai` additionally supports
    /// namespace syntax (`vertex::claude-sonnet-4-6`) for routing
    /// through gateway adapters.
    pub model_id: SmolStr,
}

impl Default for ModelChoice {
    fn default() -> Self {
        Self {
            provider: AdapterKind::Anthropic,
            model_id: SmolStr::new_static("claude-sonnet-4-6"),
        }
    }
}

/// Typed overrides for Anthropic-only knobs not on [`ChatOptions`].
/// Starts empty; grows when concrete needs surface (e.g. forced beta
/// headers for emerging capabilities).
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnthropicOverrides {}

/// Typed overrides for OpenAI-only knobs not on [`ChatOptions`].
/// Starts empty.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenAIOverrides {}

// ==========================================================================
// Context policy
// ==========================================================================

/// Per-persona message-history and snapshot policy. `None` / default
/// fields fall back to the runtime's defaults.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextPolicy {
    /// Compression strategy applied when `should_compress` gate fires.
    /// `None` = compression disabled for this persona (no archival fires
    /// regardless of context growth). Most persona configurations should
    /// opt in explicitly; `CompressionStrategy::default()` is
    /// `RecursiveSummarization` with sensible chunking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression: Option<CompressionStrategy>,

    /// Cheap short-circuit floor: don't even call `should_compress` until
    /// the active turn record count reaches this value. Avoids spamming
    /// `count_tokens` on every early-session turn when history is tiny.
    /// `None` = use runtime default (100 turns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress_check_message_floor: Option<usize>,

    /// Real compression gate: when `count_tokens` reports active-context
    /// tokens above this threshold, the strategy fires. `None` = runtime
    /// derives a default from the model's advertised context window
    /// (minus `max_tokens` output reserve minus a safety buffer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress_token_threshold: Option<usize>,

    /// Snapshot selection and mid-batch delta behaviour (Phase 5's
    /// [`SnapshotPolicy`]).
    #[serde(default)]
    pub snapshot_policy: SnapshotPolicy,
}

impl ContextPolicy {
    /// Set the compression strategy. Builder-style.
    pub fn with_compression(mut self, compression: Option<CompressionStrategy>) -> Self {
        self.compression = compression;
        self
    }

    /// Set the message floor for the compression gate. Builder-style.
    pub fn with_message_floor(mut self, floor: usize) -> Self {
        self.compress_check_message_floor = Some(floor);
        self
    }

    /// Set the token threshold for the compression gate. Builder-style.
    pub fn with_token_threshold(mut self, threshold: usize) -> Self {
        self.compress_token_threshold = Some(threshold);
        self
    }

    /// Override the snapshot policy (selection filter + mid-batch delta
    /// behaviour). Builder-style.
    pub fn with_snapshot_policy(mut self, policy: crate::types::message::SnapshotPolicy) -> Self {
        self.snapshot_policy = policy;
        self
    }

    /// Override only the mid-batch delta behaviour, leaving the rest of the
    /// snapshot policy unchanged. Builder-style convenience.
    pub fn with_mid_batch(
        mut self,
        mid_batch: crate::types::message::MidBatchDeltaBehavior,
    ) -> Self {
        self.snapshot_policy.mid_batch = mid_batch;
        self
    }
}

// ==========================================================================
// Runtime budgets
// ==========================================================================

/// Tidepool JIT budgets and nursery size. `None` values fall back to
/// runtime defaults.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeBudgets {
    /// Wall-clock time-in-JIT budget per turn, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,

    /// CPU time-in-JIT budget per turn, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<u64>,

    /// Additional milliseconds of runaway compute tolerated beyond the
    /// CPU budget before hard-abandon fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hard_abandon_ms: Option<u64>,

    /// Post-hard-abandon grace window, in milliseconds. Exceeding this
    /// detaches the task and poisons the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_grace_ms: Option<u64>,

    /// JIT nursery size in bytes. `None` = runtime default (32 MiB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nursery_size: Option<usize>,
}

// ==========================================================================
// Session snapshot (aggregate of persona snapshots)
// ==========================================================================

/// A serializable snapshot of a complete session — one or more agents
/// plus session-level metadata.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Per-agent persona snapshots included in this session checkpoint.
    pub personas: Vec<PersonaSnapshot>,

    /// Wall-clock time the session snapshot was captured.
    pub captured_at: Timestamp,

    /// Schema version for forward-compatibility checks.
    pub schema_version: u32,

    /// Opaque session-level data (coordination pattern state, routing
    /// tables, etc.). Shape TBD; currently unused in foundation.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
}

impl SessionSnapshot {
    /// Build a session snapshot. `schema_version` defaults to 1.
    pub fn new(personas: Vec<PersonaSnapshot>, data: serde_json::Value) -> Self {
        Self {
            personas,
            captured_at: Timestamp::now(),
            schema_version: 1,
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_produces_minimal_valid_snapshot() {
        let snap = PersonaSnapshot::new("orual", "Orual");
        assert_eq!(snap.agent_id.as_str(), "orual");
        assert_eq!(snap.name.as_str(), "Orual");
        assert!(snap.as_of_turn.is_none());
        assert_eq!(snap.schema_version, 1);
        assert!(snap.memory_blocks.is_empty());
        assert!(snap.system_prompt.is_none());
    }

    #[test]
    fn budget_setters_apply() {
        let snap = PersonaSnapshot::new("a", "A")
            .with_wall_budget_ms(5_000)
            .with_cpu_budget_ms(2_000)
            .with_hard_abandon_ms(1_000)
            .with_cancel_grace_ms(30_000)
            .with_nursery_size(64 * 1024 * 1024);
        assert_eq!(snap.budgets.wall_ms, Some(5_000));
        assert_eq!(snap.budgets.cpu_ms, Some(2_000));
        assert_eq!(snap.budgets.hard_abandon_ms, Some(1_000));
        assert_eq!(snap.budgets.cancel_grace_ms, Some(30_000));
        assert_eq!(snap.budgets.nursery_size, Some(64 * 1024 * 1024));
    }

    #[test]
    fn memory_block_spec_text_shortcut() {
        let spec = MemoryBlockSpec::text("hello world");
        assert_eq!(
            spec.content,
            serde_json::Value::String("hello world".to_string())
        );
        assert_eq!(spec.memory_type, MemoryType::default());
    }

    #[test]
    fn memory_block_spec_builder_chain() {
        let spec = MemoryBlockSpec::text("base instructions")
            .with_memory_type(MemoryType::Core)
            .with_permission(MemoryPermission::ReadOnly)
            .with_pinned(true)
            .with_char_limit(4_096);
        assert_eq!(spec.memory_type, MemoryType::Core);
        assert_eq!(spec.permission, MemoryPermission::ReadOnly);
        assert!(spec.pinned);
        assert_eq!(spec.char_limit, Some(4_096));
        assert!(spec.crdt_snapshot.is_none());
    }

    #[test]
    fn default_model_is_anthropic_sonnet() {
        let model = ModelSpec::default();
        assert_eq!(model.choice.provider, AdapterKind::Anthropic);
        assert_eq!(model.choice.model_id.as_str(), "claude-sonnet-4-6");
    }

    #[test]
    fn round_trip_via_json() {
        let snap = PersonaSnapshot::new("orual", "Orual")
            .with_wall_budget_ms(10_000)
            .with_system_prompt("you are a helpful assistant")
            .with_memory_block(
                "persona",
                MemoryBlockSpec::text("I am Orual.")
                    .with_memory_type(MemoryType::Core)
                    .with_pinned(true),
            );
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: PersonaSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_id, snap.agent_id);
        assert_eq!(
            parsed.system_prompt.as_deref(),
            Some("you are a helpful assistant")
        );
        assert_eq!(parsed.memory_blocks.len(), 1);
        assert_eq!(parsed.budgets.wall_ms, Some(10_000));
    }

    /// Round-trip a `PersonaSnapshot` with `open_files` populated.
    ///
    /// Verifies AC2.11: the list of open file paths at snapshot time
    /// survives a serde round-trip so restore can re-open them.
    #[test]
    fn open_files_round_trip_with_paths() {
        use std::path::PathBuf;

        let snap = PersonaSnapshot::new("orual", "Orual").with_open_files(vec![
            PathBuf::from("/foo/bar.txt"),
            PathBuf::from("/baz/qux.rs"),
        ]);

        let json = serde_json::to_string(&snap).unwrap();
        let parsed: PersonaSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.open_files.len(), 2);
        assert_eq!(parsed.open_files[0], PathBuf::from("/foo/bar.txt"));
        assert_eq!(parsed.open_files[1], PathBuf::from("/baz/qux.rs"));
    }

    /// `open_files` defaults to an empty list when not present in serialized form.
    ///
    /// Ensures old snapshots that pre-date the field deserialize cleanly
    /// (forward-compatibility via `#[serde(default)]`).
    #[test]
    fn open_files_defaults_to_empty_on_round_trip() {
        // Construct a snapshot without `open_files` and verify the field is
        // absent from the JSON (skip_serializing_if = empty), then verify
        // a JSON payload without the field deserializes with an empty list.
        let snap = PersonaSnapshot::new("orual", "Orual");
        let json = serde_json::to_string(&snap).unwrap();

        // JSON must NOT contain the "open_files" key when the vec is empty.
        assert!(
            !json.contains("open_files"),
            "open_files should be absent from JSON when empty; json: {json}"
        );

        // Deserializing old JSON (no field) must give an empty vec.
        let old_json = r#"{"agent_id":"orual","name":"Orual","captured_at":"2026-01-01T00:00:00Z","schema_version":1}"#;
        let parsed: PersonaSnapshot = serde_json::from_str(old_json).unwrap();
        assert!(
            parsed.open_files.is_empty(),
            "open_files should default to empty when absent from JSON"
        );
    }
}
