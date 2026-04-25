//! Sibling persona spawn: open an existing persona's session or create a new
//! identity draft.
//!
//! A sibling is an independent session with its own `CapabilitySet` loaded
//! from its own KDL config file — it is NOT registered in the parent's
//! `SpawnRegistry` and does NOT inherit the parent's capability set.
//!
//! # Phase 2 scope
//!
//! Phase 2 delivers:
//! - Resolver trait + `StubSiblingResolver` (backed by `DashMap`) for tests.
//! - `spawn_sibling_existing` — validates the persona is reachable via the
//!   resolver, loads its snapshot, and returns its agent_id as the `PersonaId`.
//!   The actual session-open lifecycle (provider, turn sink, full
//!   `open_with_agent_loop`) is deferred to Phase 6.
//! - `spawn_sibling_new` — writes a draft persona KDL to disk and returns the
//!   new id. When the parent has `CapabilityFlag::SpawnNewIdentities` the draft
//!   is approved; otherwise it is a pending draft awaiting Phase 6 registry
//!   ingestion.
//!
//! # Resolver contract
//!
//! `SiblingPersonaResolver` is the seam that Phase 6 replaces with a
//! `pattern_db`-backed implementation. Phase 2 ships `UnconfiguredSiblingResolver`
//! as the production default (every lookup fails) and `StubSiblingResolver`
//! for tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use smol_str::SmolStr;

use pattern_core::spawn::{PersonaConfig, SiblingConfig};
use pattern_core::types::ids::PersonaId;

use crate::persona_loader;
use crate::session::SessionContext;
use crate::spawn::SpawnError;

// ── Registry error ────────────────────────────────────────────────────────────

/// Errors that the `SiblingPersonaResolver` may return.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum RegistryError {
    /// The requested persona id is not known to this resolver.
    #[error("persona not found: {0}")]
    PersonaNotFound(SmolStr),
}

// ── Resolver trait ────────────────────────────────────────────────────────────

/// Resolves a `PersonaId` to the path of its KDL file.
///
/// Phase 2 ships two implementations:
/// - [`UnconfiguredSiblingResolver`] — production default; every lookup fails
///   with [`RegistryError::PersonaNotFound`].
/// - [`StubSiblingResolver`] — test implementation backed by an in-memory map.
///
/// Phase 6 will supply a `pattern_db`-backed implementation that queries the
/// persona registry table.
pub trait SiblingPersonaResolver: Send + Sync + std::fmt::Debug {
    /// Resolve `id` to its KDL file path.
    ///
    /// Returns [`RegistryError::PersonaNotFound`] if the id is unknown.
    fn resolve_path(&self, id: &PersonaId) -> Result<PathBuf, RegistryError>;
}

// ── Production default ────────────────────────────────────────────────────────

/// Production default resolver: every lookup fails with
/// [`RegistryError::PersonaNotFound`].
///
/// Phase 6 replaces this with a `pattern_db`-backed resolver. Until then,
/// production sessions that do not explicitly wire a resolver will surface a
/// clear `PersonaNotFound` error rather than silently doing nothing.
#[derive(Debug, Default)]
pub struct UnconfiguredSiblingResolver;

impl SiblingPersonaResolver for UnconfiguredSiblingResolver {
    fn resolve_path(&self, id: &PersonaId) -> Result<PathBuf, RegistryError> {
        Err(RegistryError::PersonaNotFound(id.clone()))
    }
}

// ── Test stub ─────────────────────────────────────────────────────────────────

/// In-memory resolver backed by a `HashMap` for use in integration tests.
///
/// Call [`StubSiblingResolver::register`] to pre-populate the map before
/// handing it to the function under test.
///
/// # Thread safety
///
/// Uses `parking_lot::Mutex` so the resolver can be shared across async test
/// tasks without needing a `tokio::sync::Mutex` in the sync trait method.
#[derive(Debug, Default)]
pub struct StubSiblingResolver {
    entries: parking_lot::Mutex<HashMap<SmolStr, PathBuf>>,
}

impl StubSiblingResolver {
    /// Create an empty stub resolver.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a mapping from `id` to `path`.
    pub fn register(&self, id: impl Into<SmolStr>, path: impl Into<PathBuf>) {
        self.entries.lock().insert(id.into(), path.into());
    }
}

impl SiblingPersonaResolver for StubSiblingResolver {
    fn resolve_path(&self, id: &PersonaId) -> Result<PathBuf, RegistryError> {
        self.entries
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| RegistryError::PersonaNotFound(id.clone()))
    }
}

// ── spawn_sibling_existing ────────────────────────────────────────────────────

/// Validate that an existing persona is reachable and return its `PersonaId`.
///
/// # Phase 2 contract
///
/// - Resolves the persona path via `resolver.resolve_path(persona_id)`.
/// - Loads the persona snapshot via
///   [`persona_loader::load_persona`].
/// - Returns the loaded snapshot's `agent_id` as the `PersonaId`.
///
/// The actual session-open lifecycle (provider, turn sink, eval worker) is
/// deferred to Phase 6, when the daemon-driven sibling lifecycle lands. Phase 2
/// verifies AC5.1 (persona reachable), AC5.4 (caps from own config, not
/// inherited), and AC5.6 (PersonaNotFound on unknown id).
///
/// Siblings are NOT registered in the parent's `SpawnRegistry` — they live
/// independently of the parent's lifetime.
pub async fn spawn_sibling_existing(
    _parent: &SessionContext,
    _cfg: &SiblingConfig,
    persona_id: &PersonaId,
    resolver: Arc<dyn SiblingPersonaResolver>,
) -> Result<PersonaId, SpawnError> {
    // Step 1: resolve path via the resolver.
    let path = resolver.resolve_path(persona_id).map_err(|e| match e {
        RegistryError::PersonaNotFound(id) => SpawnError::PersonaNotFound { id },
    })?;

    // Step 2: load the persona snapshot to validate the KDL and read its
    // agent_id. The capabilities are in `snap.capabilities` — AC5.4 verifies
    // these come from the sibling's own config, not from the spawner.
    let snap =
        persona_loader::load_persona(&path).map_err(|e| SpawnError::Runtime(e.to_string()))?;

    // Step 3: return the persona's own agent_id as the PersonaId. The caller
    // may cache this id to communicate with the sibling when Phase 6 opens
    // the live session.
    Ok(SmolStr::from(snap.agent_id.as_str()))
}

// ── spawn_sibling_new ─────────────────────────────────────────────────────────

/// Result status of a sibling spawn — distinguishes whether the new persona
/// is authorised for a live session (`Active`) or sits as a pending draft
/// awaiting human promotion (`Draft`).
///
/// Phase 2 defers the actual session-open to Phase 6 for both branches, but
/// the structural distinction is wired through the wire grammar so agents
/// (and Phase 6's promote workflow) can branch without re-deriving the
/// capability check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiblingStatus {
    /// Parent held [`pattern_core::CapabilityFlag::SpawnNewIdentities`];
    /// the draft is authorised for live session-open. Phase 6 promotes it
    /// to a running session.
    Active,
    /// Parent did NOT hold `SpawnNewIdentities`; the draft is pending
    /// human-driven promote. Phase 6's promote workflow gates on this.
    Draft,
}

/// Outcome of `spawn_sibling_new` — pairs the new persona id with the status
/// (`Active` or `Draft`) and the on-disk path of the written KDL draft.
///
/// The handler arm flattens this to the wire-level [`WireSiblingSpawn`]
/// (`SiblingSpawn` on the Haskell side); internal callers consume the typed
/// outcome directly.
#[derive(Debug, Clone)]
pub struct SiblingNewOutcome {
    /// Slugified persona id (used as the KDL filename stem).
    pub persona_id: PersonaId,
    /// Status — `Active` or `Draft`.
    pub status: SiblingStatus,
    /// On-disk path of the written draft KDL.
    pub kdl_path: std::path::PathBuf,
}

/// Mint a draft persona KDL for a new sibling identity and return its id +
/// status.
///
/// # Phase 2 contract
///
/// Always writes a draft KDL to `drafts_dir/<id>.kdl`. The returned
/// [`SiblingNewOutcome`] discriminates between:
///
/// - [`SiblingStatus::Active`]: parent held
///   [`pattern_core::CapabilityFlag::SpawnNewIdentities`] (or its caps were
///   `None`, meaning full power). Phase 6 will open a live session for this
///   draft.
/// - [`SiblingStatus::Draft`]: parent did NOT hold the flag. The draft sits
///   pending human promote (Phase 6 workflow).
///
/// Phase 2 does NOT actually open a session for either path — that's
/// deferred to Phase 6 alongside the registry. The status field gives Phase
/// 6 the structural signal to gate session-opening.
///
/// # Errors
///
/// Returns [`SpawnError::DraftWriteFailed`] if the draft directory cannot be
/// created or the file cannot be written.
pub async fn spawn_sibling_new(
    parent: &SessionContext,
    _cfg: &SiblingConfig,
    persona_cfg: &PersonaConfig,
    drafts_dir: &std::path::Path,
) -> Result<SiblingNewOutcome, SpawnError> {
    // Slugify the name to a safe file-stem id.
    let id: SmolStr = slug_from_name(&persona_cfg.name);

    // Write the draft KDL to disk.
    let writer = super::draft::RuntimeConfigWriter::new(drafts_dir.to_owned());
    let kdl = mint_draft_kdl(persona_cfg);
    let kdl_path = writer.write_draft(&id, &kdl)?;

    // Capability gate: parent caps `None` = full power per the
    // `CapabilitySet::all` convention. Otherwise the flag must be held
    // for the draft to be marked Active.
    let has_flag = parent
        .capabilities()
        .map(|c| c.has_flag(pattern_core::CapabilityFlag::SpawnNewIdentities))
        .unwrap_or(true);
    let status = if has_flag {
        SiblingStatus::Active
    } else {
        SiblingStatus::Draft
    };

    match status {
        SiblingStatus::Active => tracing::info!(
            persona_id = %id,
            source = "runtime.spawn.sibling",
            "draft persona written (Active; live session open deferred to Phase 6)"
        ),
        SiblingStatus::Draft => tracing::info!(
            persona_id = %id,
            source = "runtime.spawn.sibling",
            "draft persona written (Draft; SpawnNewIdentities flag not held; \
             pending human promote in Phase 6)"
        ),
    }

    Ok(SiblingNewOutcome {
        persona_id: id,
        status,
        kdl_path,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert a persona name to a filesystem-safe id slug.
///
/// Lowercases, replaces runs of non-alphanumeric characters with `-`, and
/// strips leading/trailing `-`. Falls back to `"unnamed-persona"` for empty
/// inputs.
fn slug_from_name(name: &str) -> SmolStr {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        SmolStr::new_static("unnamed-persona")
    } else {
        SmolStr::from(slug)
    }
}

/// Mint a minimal KDL string for a new persona from a `PersonaConfig`.
///
/// Uses a hand-rolled template rather than a KDL serialiser — `knus` is a
/// parser only and there is no KDL serialisation library in the workspace.
/// The template covers the fields required by `persona_loader` to produce a
/// valid `PersonaSnapshot`: `name`, `agent-id`, `system-prompt`, and a
/// default `model` block. The `capabilities` block is included when the
/// `PersonaConfig` declares a non-empty set.
pub(crate) fn mint_draft_kdl(cfg: &PersonaConfig) -> String {
    let id = slug_from_name(&cfg.name);
    let name = kdl_escape_string(&cfg.name);
    let prompt = kdl_escape_string(&cfg.system_prompt);

    let mut out = String::new();
    out.push_str(&format!("name {name}\n"));
    out.push_str(&format!("agent-id \"{id}\"\n"));
    out.push_str(&format!("system-prompt {prompt}\n"));
    out.push('\n');
    out.push_str("model provider=\"anthropic\" model-id=\"claude-sonnet-4-6\" {\n");
    out.push_str("    max-tokens 4096\n");
    out.push_str("}\n");
    out.push('\n');
    out.push_str("context {\n");
    out.push_str("    compress-check-message-floor 100\n");
    out.push_str("}\n");
    out.push('\n');
    out.push_str("budgets {\n");
    out.push_str("    wall-ms 30000\n");
    out.push_str("    cpu-ms 10000\n");
    out.push_str("}\n");

    // Write capabilities block if the set is non-empty.
    let categories: Vec<_> = cfg.capabilities.iter_categories().collect();
    if !categories.is_empty() {
        out.push('\n');
        out.push_str("capabilities {\n");
        out.push_str("    effects {\n");
        for cat in &categories {
            let cat_name = format!("{cat:?}").to_lowercase();
            out.push_str(&format!("        {cat_name}\n"));
        }
        out.push_str("    }\n");
        out.push_str("}\n");
    }

    out
}

/// Escape a string for inclusion in a KDL document.
///
/// Wraps the value in double quotes and escapes backslashes and double-quotes.
fn kdl_escape_string(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── slug_from_name ──────────────────────────────────────────────────────

    #[test]
    fn slug_lowercases_and_hyphenates() {
        assert_eq!(
            slug_from_name("My Test Persona"),
            SmolStr::from("my-test-persona")
        );
    }

    #[test]
    fn slug_handles_special_chars() {
        assert_eq!(
            slug_from_name("orual's helper!"),
            SmolStr::from("orual-s-helper")
        );
    }

    #[test]
    fn slug_empty_falls_back() {
        assert_eq!(slug_from_name(""), SmolStr::new_static("unnamed-persona"));
        assert_eq!(
            slug_from_name("---"),
            SmolStr::new_static("unnamed-persona")
        );
    }

    // ── mint_draft_kdl ──────────────────────────────────────────────────────

    #[test]
    fn mint_draft_kdl_contains_required_fields() {
        let cfg = PersonaConfig::new(
            "test-draft",
            "Draft system prompt.",
            pattern_core::CapabilitySet::from_iter([pattern_core::EffectCategory::Memory]),
        );
        let kdl = mint_draft_kdl(&cfg);
        assert!(kdl.contains("name"), "must have name");
        assert!(kdl.contains("agent-id"), "must have agent-id");
        assert!(kdl.contains("system-prompt"), "must have system-prompt");
        assert!(kdl.contains("model"), "must have model block");
        assert!(
            kdl.contains("memory"),
            "must have capabilities.effects.memory"
        );
    }

    #[test]
    fn mint_draft_kdl_skips_empty_capabilities() {
        let cfg = PersonaConfig::new(
            "no-caps",
            "no caps here",
            pattern_core::CapabilitySet::empty(),
        );
        let kdl = mint_draft_kdl(&cfg);
        assert!(
            !kdl.contains("capabilities {"),
            "should not emit empty capabilities block; got:\n{kdl}"
        );
    }
}
