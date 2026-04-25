//! Persona KDL loader for `pattern-test-cli`.
//!
//! Reads a `.kdl` file on disk and converts it into a `PersonaSnapshot`
//! (from `pattern_core::types::agent`) ready to hand to the
//! `open_with_agent_loop` method of `TidepoolSession` (from `crate::session`).
//!
//! ## KDL schema
//!
//! ```kdl
//! name "orual-smoke-test"
//! agent-id "orual-smoke-test"   // optional; defaults to `name` if omitted
//!
//! // Optional slot[1] override.
//! system-prompt "You are a helpful test assistant."
//! // OR: system-prompt-path "./system_prompt.txt"
//!
//! model provider="anthropic" model-id="claude-sonnet-4-6" {
//!     temperature 0.7
//!     max-tokens 4096
//!     // reasoning-effort "medium"   // none | low | medium | high | xhigh | max
//! }
//!
//! context {
//!     compress-check-message-floor 50
//!     compress-token-threshold 150000
//!     // "include_self_edits" (default) or "filter_self_edits"
//!     mid-batch "filter_self_edits"
//!
//!     compression type="recursive_summarization" {
//!         chunk-size 20
//!         summarization-model "claude-haiku-4-5"
//!     }
//! }
//!
//! budgets {
//!     wall-ms 30000
//!     cpu-ms 10000
//! }
//!
//! memory {
//!     persona content="I am a minimal smoke-test persona." {
//!         memory-type "core"
//!         permission "read_write"
//!         pinned true
//!     }
//!     scratchpad content-path="./scratchpad.txt" {
//!         memory-type "working"
//!         permission "read_write"
//!     }
//! }
//! ```
//!
//! Unknown top-level or section keys are rejected with an error that names the
//! offending key. Missing required fields (`name`) produce an error that names
//! the field.

use std::path::Path;

use genai::adapter::AdapterKind;
use genai::chat::{ChatOptions, ReasoningEffort};
use knus::Decode;
use miette::Diagnostic;
use pattern_core::types::compression::CompressionStrategy;
use pattern_core::types::memory_types::{MemoryPermission, MemoryType};
use pattern_core::types::message::MidBatchDeltaBehavior;
use pattern_core::types::snapshot::{
    ContextPolicy, MemoryBlockSpec, ModelChoice, ModelSpec, PersonaSnapshot,
};
use smol_str::SmolStr;
use thiserror::Error;

// ==========================================================================
// Public error type
// ==========================================================================

/// Errors that can occur while loading a persona KDL file.
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum PersonaLoadError {
    /// The file could not be read from disk.
    #[error("could not read persona file at {path}: {source}")]
    #[diagnostic(code(persona::io_error))]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// The file content is not valid KDL, or has unknown fields.
    #[error("error parsing persona KDL at {path}: {message}")]
    #[diagnostic(
        code(persona::parse_error),
        help("check that all keys are valid; unknown fields are not allowed")
    )]
    Parse { path: String, message: String },

    /// Two mutually exclusive fields were both set (e.g. `content` and
    /// `content_path` on the same memory block, or `system_prompt` and
    /// `system_prompt_path`).
    #[error(
        "persona file at {path}: `{field_a}` and `{field_b}` are mutually exclusive — set only one"
    )]
    #[diagnostic(code(persona::conflicting_fields))]
    ConflictingFields {
        path: String,
        field_a: String,
        field_b: String,
    },

    /// A `content_path` or `system_prompt_path` reference could not be read.
    #[error("persona file at {path}: could not read referenced file `{referenced}`: {source}")]
    #[diagnostic(code(persona::referenced_file_io))]
    ReferencedFileIo {
        path: String,
        referenced: String,
        #[source]
        source: std::io::Error,
    },

    /// An unknown provider string in `model`.
    #[error(
        "persona file at {path}: unknown provider `{provider}` in model; \
        expected one of: anthropic, gemini, openai, openai_resp, ollama, ollama_cloud, \
        fireworks, together, groq, deepseek, xai, cohere, vertex, nebius, \
        mimo, zai, bigmodel, aliyun, github_copilot"
    )]
    #[diagnostic(
        code(persona::unknown_provider),
        help(
            "use a lowercase provider name such as \"anthropic\", \"gemini\", or \"openai\"; \
            ollama and openai-compatible providers are also supported"
        )
    )]
    UnknownProvider { path: String, provider: String },

    /// An unknown `reasoning_effort` string.
    #[error(
        "persona file at {path}: unknown reasoning_effort `{value}`; expected: none, low, medium, high, xhigh, max"
    )]
    #[diagnostic(code(persona::unknown_reasoning_effort))]
    UnknownReasoningEffort { path: String, value: String },

    /// An unknown `mid_batch` string in `context`.
    #[error(
        "persona file at {path}: unknown mid_batch `{value}`; expected: include_self_edits, filter_self_edits"
    )]
    #[diagnostic(code(persona::unknown_mid_batch))]
    UnknownMidBatch { path: String, value: String },

    /// Persona discovery failed (I/O or other error scanning directories).
    #[error("persona discovery failed: {0}")]
    #[diagnostic(code(persona::discovery))]
    Discovery(#[from] pattern_memory::persona::PersonaDiscoveryError),

    /// The requested persona name was not found in any scanned directory.
    #[error("persona `{name}` not found; searched directories contained: {searched:?}")]
    #[diagnostic(
        code(persona::not_found),
        help(
            "ensure a directory named @{name} with a persona.kdl exists in ~/.pattern/personas/ or <mount>/personas/"
        )
    )]
    NotFound { name: String, searched: Vec<String> },

    /// An unknown compression type string.
    #[error(
        "persona file at {path}: unknown compression type `{value}`; expected: truncate, recursive_summarization, importance_based, time_decay"
    )]
    #[diagnostic(code(persona::unknown_compression_type))]
    UnknownCompressionType { path: String, value: String },

    /// A required field was missing from the compression node.
    #[error(
        "persona file at {path}: compression type `{compression_type}` requires field `{field}`"
    )]
    #[diagnostic(code(persona::missing_compression_field))]
    MissingCompressionField {
        path: String,
        compression_type: String,
        field: String,
    },

    /// An unknown memory_type string.
    #[error(
        "persona file at {path}: unknown memory_type `{value}` for block `{label}`; expected: core, working, archival"
    )]
    #[diagnostic(code(persona::unknown_memory_type))]
    UnknownMemoryType {
        path: String,
        label: String,
        value: String,
    },

    /// An unknown permission string.
    #[error(
        "persona file at {path}: unknown permission `{value}` for block `{label}`; expected: read_only, partner, human, append, read_write, admin"
    )]
    #[diagnostic(code(persona::unknown_permission))]
    UnknownPermission {
        path: String,
        label: String,
        value: String,
    },
}

// ==========================================================================
// Public entry point
// ==========================================================================

/// Load a [`PersonaSnapshot`] from a KDL file at `path`.
///
/// # Errors
///
/// Returns a [`PersonaLoadError`] (wrapped in [`miette::Report`]) if the file
/// cannot be read, contains invalid KDL, uses unknown fields, is missing the
/// required `name` field, or has conflicting / unresolvable content references.
pub fn load_persona(path: &Path) -> miette::Result<PersonaSnapshot> {
    load_persona_inner(path).map_err(miette::Report::new)
}

/// Discover a persona by name across global and project scopes, then load it.
///
/// Scans `<paths.base()>/personas/` (global) and `<project_mount>/personas/`
/// (project-scoped) for a directory named `@<name>` (or `<name>`) containing
/// `persona.kdl`. Project-scoped takes precedence on collision.
///
/// # Errors
///
/// Returns [`PersonaLoadError::NotFound`] if no matching persona is found,
/// [`PersonaLoadError::Discovery`] if the directory scan fails, or a parse
/// error if the KDL is invalid.
pub fn discover_and_load(
    name: &str,
    paths: &pattern_memory::PatternPaths,
    project_mount: Option<&Path>,
) -> miette::Result<PersonaSnapshot> {
    discover_and_load_inner(name, paths, project_mount).map_err(miette::Report::new)
}

fn discover_and_load_inner(
    name: &str,
    paths: &pattern_memory::PatternPaths,
    project_mount: Option<&Path>,
) -> Result<PersonaSnapshot, PersonaLoadError> {
    let personas = pattern_memory::persona::discover_personas(paths, project_mount)?;
    let path = personas
        .get(name)
        .ok_or_else(|| PersonaLoadError::NotFound {
            name: name.to_owned(),
            searched: personas.keys().cloned().collect(),
        })?;
    load_persona_inner(path)
}

fn load_persona_inner(path: &Path) -> Result<PersonaSnapshot, PersonaLoadError> {
    let path_str = path.display().to_string();

    // Read raw bytes.
    let raw = std::fs::read_to_string(path).map_err(|e| PersonaLoadError::Io {
        path: path_str.clone(),
        source: e,
    })?;

    // Parse into our DTO, rejecting unknown fields.
    let file: PersonaFile =
        knus::parse::<PersonaFile>(&path_str, &raw).map_err(|e| PersonaLoadError::Parse {
            path: path_str.clone(),
            message: format_knus_error(&e),
        })?;

    // The directory the KDL file lives in — used to resolve relative paths.
    let base_dir = path.parent().unwrap_or(Path::new("."));

    convert(file, base_dir, &path_str)
}

/// Format a knus parse error including related sub-errors.
///
/// The top-level `knus::errors::Error` displays as the terse "error parsing KDL".
/// Detailed field-level diagnostics live in its `#[related]` errors. This
/// function concatenates them so the `PersonaLoadError::Parse` message
/// contains actionable information.
fn format_knus_error(err: &knus::errors::Error) -> String {
    use miette::Diagnostic;
    let mut parts = vec![err.to_string()];
    if let Some(related) = err.related() {
        for sub in related {
            parts.push(sub.to_string());
        }
    }
    parts.join("; ")
}

// ==========================================================================
// KDL DTO types (parsed via knus derive)
// ==========================================================================

/// Top-level structure of a persona KDL file.
///
/// Each top-level KDL node maps to a field. knus automatically converts
/// snake_case Rust field names to kebab-case KDL node names.
///
/// Required: `name` node.
/// Optional: `agent-id`, `system-prompt`, `system-prompt-path`, `model`,
/// `context`, `budgets`, `memory`.
#[derive(Debug, Decode)]
struct PersonaFile {
    /// Display name and (if `agent_id` is absent) the agent identifier.
    ///
    /// KDL: `name "orual-smoke-test"`
    #[knus(child, unwrap(argument))]
    name: String,

    /// Stable agent identifier. Defaults to `name` when absent.
    ///
    /// KDL: `agent-id "orual-smoke-test"`
    #[knus(child, unwrap(argument), default)]
    agent_id: Option<String>,

    // -- System prompt (mutually exclusive) --
    /// Inline slot-[1] system prompt override.
    ///
    /// KDL: `system-prompt "You are a helpful test assistant."`
    #[knus(child, unwrap(argument), default)]
    system_prompt: Option<String>,

    /// Path to a file whose content becomes the slot-[1] system prompt.
    /// Resolved relative to the persona KDL's directory.
    ///
    /// KDL: `system-prompt-path "./system_prompt.txt"`
    #[knus(child, unwrap(argument), default)]
    system_prompt_path: Option<String>,

    // -- Sub-sections --
    /// `model` node — provider, model ID, and sampling knobs.
    #[knus(child, default)]
    model: ModelSection,

    /// `context` node — compression and snapshot policies.
    #[knus(child, default)]
    context: ContextSection,

    /// `budgets` node — runtime resource limits.
    #[knus(child, default)]
    budgets: BudgetsSection,

    /// `memory` node containing named memory block children.
    #[knus(child, default)]
    memory: MemorySection,

    /// `capabilities` node — effect-category visibility + flags.
    /// Optional; absent means "full power" (back-compat).
    #[knus(child)]
    capabilities: Option<CapabilitiesSection>,

    /// `policy` node — list of policy rules layered with
    /// `Precedence::KdlConfig` over Rust defaults at session open.
    #[knus(child)]
    policy: Option<PolicySectionDoc>,
}

/// `capabilities` section.
///
/// KDL:
/// ```text
/// capabilities {
///     effects {
///         memory
///         message
///         tasks
///     }
///     flags {
///         spawn-new-identities
///     }
/// }
/// ```
#[derive(Debug, Decode, Default)]
struct CapabilitiesSection {
    /// `effects` block: each child node's name is an effect category
    /// (case-insensitive). Absent → empty effect set.
    #[knus(child)]
    effects: Option<EffectsBlock>,

    /// `flags` block: each child node's name is a capability flag
    /// (kebab-case). Absent → empty flag set.
    #[knus(child)]
    flags: Option<FlagsBlock>,
}

#[derive(Debug, Decode, Default)]
struct EffectsBlock {
    /// Each child node's name is an effect-category identifier.
    #[knus(children)]
    items: Vec<NamedNode>,
}

#[derive(Debug, Decode, Default)]
struct FlagsBlock {
    /// Each child node's name is a flag identifier (kebab-case).
    #[knus(children)]
    items: Vec<NamedNode>,
}

/// Empty-payload node used to encode "the name itself is the value"
/// — e.g. `memory` inside `effects { ... }`.
#[derive(Debug, Decode, Default)]
struct NamedNode {
    #[knus(node_name)]
    name: String,
}

/// `policy` section.
///
/// KDL:
/// ```text
/// policy {
///     rule "allow-git-push" effect="shell" action="allow" {
///         matcher "shell-command" pattern="git push*"
///     }
///     rule "gate-all-file-writes" effect="file" action="require-approval" {
///         matcher "file-path" pattern="**/*"
///         reason "all file writes gated for this persona"
///     }
/// }
/// ```
#[derive(Debug, Decode, Default)]
struct PolicySectionDoc {
    #[knus(children(name = "rule"))]
    rules: Vec<RuleDoc>,
}

/// One `rule` child of `policy`. Name is the rule's first positional
/// argument (used for diagnostics; not stored on `PolicyRule` itself).
#[derive(Debug, Decode)]
struct RuleDoc {
    /// Rule name — diagnostics only.
    #[knus(argument)]
    #[allow(dead_code)]
    name: String,

    /// Effect category the rule applies to (case-insensitive).
    #[knus(property)]
    effect: String,

    /// Action: "allow", "require-approval", or "deny".
    #[knus(property)]
    action: String,

    /// Optional reason — surfaced to the partner when the gate prompts.
    #[knus(child, unwrap(argument), default)]
    reason: Option<String>,

    /// Matcher specifying when the rule fires.
    #[knus(child)]
    matcher: MatcherDoc,
}

/// `matcher` child of `rule`. The first positional argument selects
/// the matcher kind; subsequent properties carry the predicate data.
#[derive(Debug, Decode)]
struct MatcherDoc {
    /// Matcher kind: "always", "shell-command", "file-path".
    /// `scope` and `file-write-shape` are runtime-only and cannot be
    /// constructed via KDL.
    #[knus(argument)]
    kind: String,

    /// Glob pattern for `shell-command` and `file-path` matchers.
    #[knus(property, default)]
    pattern: Option<String>,
}

/// `model` node.
///
/// KDL:
/// ```text
/// model provider="anthropic" model-id="claude-sonnet-4-6" {
///     temperature 0.7
///     max-tokens 4096
///     reasoning-effort "medium"
///     top-p 0.9
///     seed 42
/// }
/// ```
///
/// Provider and model ID are properties on the node itself. Sampling knobs
/// are child nodes with single arguments, matching the knus pattern for
/// scalar child values.
#[derive(Debug, Decode, Default)]
struct ModelSection {
    /// Provider name — case-insensitive lowercase, e.g. `"anthropic"`.
    #[knus(property, default)]
    provider: Option<String>,

    /// Provider-specific model identifier.
    #[knus(property, default)]
    model_id: Option<String>,

    // -- ChatOptions fields as children --
    #[knus(child, unwrap(argument), default)]
    temperature: Option<f64>,

    #[knus(child, unwrap(argument), default)]
    max_tokens: Option<u32>,

    #[knus(child, unwrap(argument), default)]
    top_p: Option<f64>,

    /// Reasoning effort level: "none", "low", "medium", "high", "xhigh", "max".
    #[knus(child, unwrap(argument), default)]
    reasoning_effort: Option<String>,

    #[knus(child, unwrap(argument), default)]
    seed: Option<u64>,
}

/// `context` node.
///
/// KDL:
/// ```text
/// context {
///     compress-check-message-floor 50
///     compress-token-threshold 150000
///     mid-batch "filter_self_edits"
///     compression type="recursive_summarization" {
///         chunk-size 20
///         summarization-model "claude-haiku-4-5"
///     }
/// }
/// ```
#[derive(Debug, Decode, Default)]
struct ContextSection {
    /// Cheap short-circuit floor for the compression gate.
    #[knus(child, unwrap(argument), default)]
    compress_check_message_floor: Option<usize>,

    /// Real token threshold above which compression fires.
    #[knus(child, unwrap(argument), default)]
    compress_token_threshold: Option<usize>,

    /// Mid-batch delta snapshot behaviour. Accepted values:
    /// - `"include_self_edits"` (default)
    /// - `"filter_self_edits"`
    #[knus(child, unwrap(argument), default)]
    mid_batch: Option<String>,

    /// Compression strategy node. Parsed as an intermediate DTO because
    /// `CompressionStrategy` uses serde tagged unions which knus cannot
    /// derive directly. The `type` property selects the strategy variant;
    /// variant-specific fields are children.
    #[knus(child)]
    compression: Option<CompressionSection>,
}

/// `compression` child of `context`.
///
/// KDL:
/// ```text
/// compression type="recursive_summarization" {
///     chunk-size 20
///     summarization-model "claude-haiku-4-5"
///     summarization-prompt "Custom prompt for summarizer"
/// }
/// ```
/// or:
/// ```text
/// compression type="truncate" {
///     keep-recent 100
/// }
/// ```
#[derive(Debug, Decode)]
struct CompressionSection {
    /// Strategy discriminator: "truncate", "recursive_summarization",
    /// "importance_based", "time_decay".
    #[knus(property(name = "type"))]
    strategy_type: String,

    // -- Fields for various strategy variants --
    #[knus(child, unwrap(argument), default)]
    keep_recent: Option<usize>,

    #[knus(child, unwrap(argument), default)]
    chunk_size: Option<usize>,

    #[knus(child, unwrap(argument), default)]
    summarization_model: Option<String>,

    #[knus(child, unwrap(argument), default)]
    summarization_prompt: Option<String>,
}

/// `budgets` node.
///
/// KDL:
/// ```text
/// budgets {
///     wall-ms 30000
///     cpu-ms 10000
/// }
/// ```
#[derive(Debug, Decode, Default)]
struct BudgetsSection {
    #[knus(child, unwrap(argument), default)]
    wall_ms: Option<u64>,

    #[knus(child, unwrap(argument), default)]
    cpu_ms: Option<u64>,

    #[knus(child, unwrap(argument), default)]
    hard_abandon_ms: Option<u64>,

    #[knus(child, unwrap(argument), default)]
    cancel_grace_ms: Option<u64>,

    #[knus(child, unwrap(argument), default)]
    nursery_size: Option<usize>,
}

/// `memory` node containing named memory block children.
///
/// KDL:
/// ```text
/// memory {
///     persona content="I am a minimal persona." {
///         memory-type "core"
///         permission "read_write"
///         pinned true
///     }
///     scratchpad content-path="./scratchpad.txt" {
///         memory-type "working"
///         permission "read_write"
///     }
/// }
/// ```
///
/// Each child node inside `memory` is a memory block. The node name is the
/// block label. Content is provided via the `content` or `content-path`
/// property on the node itself.
#[derive(Debug, Decode, Default)]
struct MemorySection {
    /// Each child is a [`MemoryBlockNode`] whose KDL node name is the label.
    #[knus(children)]
    blocks: Vec<MemoryBlockNode>,
}

/// One named memory block inside the `memory` section.
///
/// The KDL node name is captured as the `label` field. Content source is
/// a property on the node (`content="..."` or `content-path="./file.txt"`).
/// Block metadata fields are children.
#[derive(Debug, Decode)]
struct MemoryBlockNode {
    /// The block label, taken from the KDL node name.
    #[knus(node_name)]
    label: String,

    /// Inline text content.
    #[knus(property, default)]
    content: Option<String>,

    /// Path to a file whose text content is used.
    /// Resolved relative to the persona KDL's directory.
    #[knus(property, default)]
    content_path: Option<String>,

    /// Memory tier. Serialised as "core", "working", "archival".
    #[knus(child, unwrap(argument), default)]
    memory_type: Option<String>,

    /// Permission level. Serialised as "read_write", "read_only", etc.
    #[knus(child, unwrap(argument), default)]
    permission: Option<String>,

    /// Human-readable description.
    #[knus(child, unwrap(argument), default)]
    description: Option<String>,

    /// Whether the block is pinned in context unconditionally.
    #[knus(child, unwrap(argument), default)]
    pinned: Option<bool>,

    /// Maximum content size in characters.
    #[knus(child, unwrap(argument), default)]
    char_limit: Option<usize>,
}

// ==========================================================================
// Conversion: PersonaFile → PersonaSnapshot
// ==========================================================================

fn convert(
    file: PersonaFile,
    base_dir: &Path,
    path_str: &str,
) -> Result<PersonaSnapshot, PersonaLoadError> {
    // -- agent_id / name --
    let name = file.name;
    let agent_id = file.agent_id.unwrap_or_else(|| name.clone());

    // -- system_prompt (mutually exclusive) --
    let system_prompt = resolve_string_or_path(
        file.system_prompt,
        file.system_prompt_path,
        "system_prompt",
        "system_prompt_path",
        base_dir,
        path_str,
    )?;

    // -- model --
    let model = convert_model(file.model, path_str)?;

    // -- context --
    let context = convert_context(file.context, path_str)?;

    // -- budgets --
    let b = file.budgets;
    let mut snap = PersonaSnapshot::new(agent_id, name);

    if let Some(ms) = b.wall_ms {
        snap = snap.with_wall_budget_ms(ms);
    }
    if let Some(ms) = b.cpu_ms {
        snap = snap.with_cpu_budget_ms(ms);
    }
    if let Some(ms) = b.hard_abandon_ms {
        snap = snap.with_hard_abandon_ms(ms);
    }
    if let Some(ms) = b.cancel_grace_ms {
        snap = snap.with_cancel_grace_ms(ms);
    }
    if let Some(sz) = b.nursery_size {
        snap = snap.with_nursery_size(sz);
    }

    snap = snap.with_model(model);
    snap = snap.with_context_policy(context);

    if let Some(prompt) = system_prompt {
        snap = snap.with_system_prompt(prompt);
    }

    // -- memory blocks --
    for block_node in file.memory.blocks {
        let label = block_node.label.clone();
        let spec = convert_memory_block(block_node, base_dir, path_str)?;
        snap = snap.with_memory_block(SmolStr::from(label), spec);
    }

    // -- capabilities --
    if let Some(caps_section) = file.capabilities {
        let caps = convert_capabilities(caps_section, path_str)?;
        snap = snap.with_capabilities(Some(caps));
    }

    // -- policy --
    if let Some(policy_section) = file.policy {
        let rules = convert_policy(policy_section, path_str)?;
        snap = snap.with_policy_rules(rules);
    }

    Ok(snap)
}

/// Convert a parsed `capabilities {}` section into a [`CapabilitySet`].
///
/// An empty `capabilities {}` block (no `effects`, no `flags`) decodes
/// to [`CapabilitySet::empty`] — pure-computation persona. Unknown
/// effect or flag identifiers produce a parse error naming the field.
fn convert_capabilities(
    section: CapabilitiesSection,
    path_str: &str,
) -> Result<pattern_core::CapabilitySet, PersonaLoadError> {
    use pattern_core::{CapabilityFlag, CapabilitySet, EffectCategory};
    use std::str::FromStr;

    let mut categories = std::collections::BTreeSet::new();
    if let Some(effects) = section.effects {
        for node in effects.items {
            let cat =
                EffectCategory::from_str(&node.name).map_err(|_| PersonaLoadError::Parse {
                    path: path_str.into(),
                    message: format!("unknown effect category {:?} in capabilities", node.name),
                })?;
            categories.insert(cat);
        }
    }

    let mut flags = std::collections::BTreeSet::new();
    if let Some(flag_block) = section.flags {
        for node in flag_block.items {
            let flag =
                CapabilityFlag::from_str(&node.name).map_err(|_| PersonaLoadError::Parse {
                    path: path_str.into(),
                    message: format!("unknown capability flag {:?} in capabilities", node.name),
                })?;
            flags.insert(flag);
        }
    }

    let mut caps = CapabilitySet::empty();
    caps.categories = categories;
    caps.flags = flags;
    Ok(caps)
}

/// Convert a parsed `policy {}` section into a `Vec<PolicyRule>` with
/// `Precedence::KdlConfig`. Rules are emitted in declaration order; the
/// runtime's `PolicySet::evaluate` is in charge of precedence-based
/// ordering at evaluation time.
fn convert_policy(
    section: PolicySectionDoc,
    path_str: &str,
) -> Result<Vec<pattern_core::PolicyRule>, PersonaLoadError> {
    use pattern_core::{EffectCategory, PolicyAction, PolicyMatcher, PolicyRule, Precedence};
    use std::str::FromStr;

    let mut out = Vec::with_capacity(section.rules.len());
    for rule_doc in section.rules {
        let effect =
            EffectCategory::from_str(&rule_doc.effect).map_err(|_| PersonaLoadError::Parse {
                path: path_str.into(),
                message: format!(
                    "unknown effect {:?} in policy rule {:?}",
                    rule_doc.effect, rule_doc.name
                ),
            })?;

        let action = match rule_doc.action.as_str() {
            "allow" => PolicyAction::Allow,
            "require-approval" => PolicyAction::RequireApproval {
                reason: rule_doc.reason,
            },
            "deny" => PolicyAction::Deny {
                reason: rule_doc.reason,
            },
            other => {
                return Err(PersonaLoadError::Parse {
                    path: path_str.into(),
                    message: format!(
                        "unknown action {other:?} in policy rule {:?}; expected \
                         \"allow\", \"require-approval\", or \"deny\"",
                        rule_doc.name
                    ),
                });
            }
        };

        let matcher = match rule_doc.matcher.kind.as_str() {
            "always" => PolicyMatcher::Always,
            "shell-command" => {
                let pattern = rule_doc
                    .matcher
                    .pattern
                    .ok_or_else(|| PersonaLoadError::Parse {
                        path: path_str.into(),
                        message: format!(
                            "shell-command matcher in rule {:?} requires a \
                             pattern=\"...\" property",
                            rule_doc.name
                        ),
                    })?;
                PolicyMatcher::ShellCommand { pattern }
            }
            "file-path" => {
                let pattern = rule_doc
                    .matcher
                    .pattern
                    .ok_or_else(|| PersonaLoadError::Parse {
                        path: path_str.into(),
                        message: format!(
                            "file-path matcher in rule {:?} requires a \
                             pattern=\"...\" property",
                            rule_doc.name
                        ),
                    })?;
                PolicyMatcher::FilePath { pattern }
            }
            other => {
                return Err(PersonaLoadError::Parse {
                    path: path_str.into(),
                    message: format!(
                        "unknown matcher kind {other:?} in rule {:?}; expected \
                         \"always\", \"shell-command\", or \"file-path\" \
                         (scope and file-write-shape are runtime-only)",
                        rule_doc.name
                    ),
                });
            }
        };

        out.push(PolicyRule::new(
            effect,
            matcher,
            action,
            Precedence::KdlConfig,
        ));
    }
    Ok(out)
}

/// Resolve a value that may be provided inline or via a file path reference.
///
/// Returns:
/// - `Ok(None)` — neither field was set.
/// - `Ok(Some(string))` — one was set; inline returned directly, path-based
///   read from disk.
/// - `Err(...)` — both were set, or the path couldn't be read.
fn resolve_string_or_path(
    inline: Option<String>,
    path_ref: Option<String>,
    inline_key: &str,
    path_key: &str,
    base_dir: &Path,
    persona_path: &str,
) -> Result<Option<String>, PersonaLoadError> {
    match (inline, path_ref) {
        (Some(_), Some(_)) => Err(PersonaLoadError::ConflictingFields {
            path: persona_path.to_string(),
            field_a: inline_key.to_string(),
            field_b: path_key.to_string(),
        }),
        (Some(s), None) => Ok(Some(s)),
        (None, Some(ref_path)) => {
            let full = base_dir.join(&ref_path);
            let content =
                std::fs::read_to_string(&full).map_err(|e| PersonaLoadError::ReferencedFileIo {
                    path: persona_path.to_string(),
                    referenced: ref_path.clone(),
                    source: e,
                })?;
            Ok(Some(content))
        }
        (None, None) => Ok(None),
    }
}

fn convert_context(
    file: ContextSection,
    path_str: &str,
) -> Result<ContextPolicy, PersonaLoadError> {
    // Resolve mid_batch string → enum before building the policy so we can
    // return an error before constructing a partial ContextPolicy.
    let mid_batch = match file.mid_batch.as_deref() {
        None | Some("include_self_edits") => MidBatchDeltaBehavior::IncludeSelfEdits,
        Some("filter_self_edits") => MidBatchDeltaBehavior::FilterSelfEdits,
        Some(other) => {
            return Err(PersonaLoadError::UnknownMidBatch {
                path: path_str.to_string(),
                value: other.to_string(),
            });
        }
    };

    // Use builder methods so the compiler catches new ContextPolicy fields
    // at the call site rather than silently dropping them.
    let mut policy = ContextPolicy::default().with_mid_batch(mid_batch);
    if let Some(floor) = file.compress_check_message_floor {
        policy = policy.with_message_floor(floor);
    }
    if let Some(threshold) = file.compress_token_threshold {
        policy = policy.with_token_threshold(threshold);
    }
    if let Some(section) = file.compression {
        let strategy = convert_compression(section, path_str)?;
        policy = policy.with_compression(Some(strategy));
    }
    Ok(policy)
}

fn convert_compression(
    section: CompressionSection,
    path_str: &str,
) -> Result<CompressionStrategy, PersonaLoadError> {
    match section.strategy_type.as_str() {
        "truncate" => {
            let keep_recent =
                section
                    .keep_recent
                    .ok_or_else(|| PersonaLoadError::MissingCompressionField {
                        path: path_str.to_string(),
                        compression_type: "truncate".to_string(),
                        field: "keep-recent".to_string(),
                    })?;
            Ok(CompressionStrategy::Truncate { keep_recent })
        }
        "recursive_summarization" => {
            let chunk_size =
                section
                    .chunk_size
                    .ok_or_else(|| PersonaLoadError::MissingCompressionField {
                        path: path_str.to_string(),
                        compression_type: "recursive_summarization".to_string(),
                        field: "chunk-size".to_string(),
                    })?;
            let summarization_model = section.summarization_model.ok_or_else(|| {
                PersonaLoadError::MissingCompressionField {
                    path: path_str.to_string(),
                    compression_type: "recursive_summarization".to_string(),
                    field: "summarization-model".to_string(),
                }
            })?;
            Ok(CompressionStrategy::RecursiveSummarization {
                chunk_size,
                summarization_model,
                summarization_prompt: section.summarization_prompt,
            })
        }
        other => Err(PersonaLoadError::UnknownCompressionType {
            path: path_str.to_string(),
            value: other.to_string(),
        }),
    }
}

fn convert_model(file: ModelSection, path_str: &str) -> Result<ModelSpec, PersonaLoadError> {
    // Resolve provider.
    let provider = if let Some(ref p) = file.provider {
        AdapterKind::from_lower_str(p).ok_or_else(|| PersonaLoadError::UnknownProvider {
            path: path_str.to_string(),
            provider: p.clone(),
        })?
    } else {
        AdapterKind::Anthropic
    };

    let model_id: SmolStr = file
        .model_id
        .map(SmolStr::from)
        .unwrap_or_else(|| SmolStr::new_static("claude-sonnet-4-6"));

    // Resolve optional reasoning_effort.
    let reasoning_effort = match file.reasoning_effort.as_deref() {
        None => None,
        Some("none") => Some(ReasoningEffort::None),
        Some("low") => Some(ReasoningEffort::Low),
        Some("medium") => Some(ReasoningEffort::Medium),
        Some("high") => Some(ReasoningEffort::High),
        Some("xhigh") => Some(ReasoningEffort::XHigh),
        Some("max") => Some(ReasoningEffort::Max),
        Some(other) => {
            return Err(PersonaLoadError::UnknownReasoningEffort {
                path: path_str.to_string(),
                value: other.to_string(),
            });
        }
    };

    let chat_options = ChatOptions {
        temperature: file.temperature,
        max_tokens: file.max_tokens,
        top_p: file.top_p,
        reasoning_effort,
        seed: file.seed,
        ..ChatOptions::default()
    };

    // ModelSpec is #[non_exhaustive]; build via Default then overwrite fields.
    let mut model = ModelSpec::default();
    model.choice = ModelChoice { provider, model_id };
    model.chat_options = chat_options;

    Ok(model)
}

fn convert_memory_block(
    block: MemoryBlockNode,
    base_dir: &Path,
    persona_path: &str,
) -> Result<MemoryBlockSpec, PersonaLoadError> {
    let label = &block.label;

    // Inline content key for the error message context.
    let inline_key = format!("memory.{label}.content");
    let path_key = format!("memory.{label}.content_path");

    let content_str = resolve_string_or_path(
        block.content,
        block.content_path,
        &inline_key,
        &path_key,
        base_dir,
        persona_path,
    )?;

    // Wrap the resolved string as a JSON string value, or use Null when absent.
    let mut spec = match content_str {
        Some(s) => MemoryBlockSpec::text(s),
        None => MemoryBlockSpec::default(),
    };

    if let Some(mt_str) = block.memory_type {
        let mt = parse_memory_type(&mt_str, label, persona_path)?;
        spec = spec.with_memory_type(mt);
    }
    if let Some(perm_str) = block.permission {
        let perm = parse_permission(&perm_str, label, persona_path)?;
        spec = spec.with_permission(perm);
    }
    if let Some(desc) = block.description {
        spec = spec.with_description(desc);
    }
    if let Some(pinned) = block.pinned {
        spec = spec.with_pinned(pinned);
    }
    if let Some(limit) = block.char_limit {
        spec = spec.with_char_limit(limit);
    }

    Ok(spec)
}

/// Parse a memory type string into a [`MemoryType`].
fn parse_memory_type(s: &str, label: &str, path_str: &str) -> Result<MemoryType, PersonaLoadError> {
    match s {
        "core" => Ok(MemoryType::Core),
        "working" => Ok(MemoryType::Working),
        "archival" => Ok(MemoryType::Archival),
        _ => Err(PersonaLoadError::UnknownMemoryType {
            path: path_str.to_string(),
            label: label.to_string(),
            value: s.to_string(),
        }),
    }
}

/// Parse a permission string into a [`MemoryPermission`].
fn parse_permission(
    s: &str,
    label: &str,
    path_str: &str,
) -> Result<MemoryPermission, PersonaLoadError> {
    s.parse::<MemoryPermission>()
        .map_err(|_| PersonaLoadError::UnknownPermission {
            path: path_str.to_string(),
            label: label.to_string(),
            value: s.to_string(),
        })
}

// ==========================================================================
// Tests
// ==========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Write a file into `dir` and return its path.
    fn write_file(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
        let p = dir.path().join(name);
        fs::write(&p, content).unwrap();
        p
    }

    fn fixture_path() -> std::path::PathBuf {
        // Tests run from the workspace root or from the crate root.
        // Try both to find the fixture.
        let candidates = [
            std::path::PathBuf::from("crates/pattern_runtime/tests/fixtures/smoke_persona.kdl"),
            std::path::PathBuf::from("tests/fixtures/smoke_persona.kdl"),
        ];
        for p in &candidates {
            if p.exists() {
                return p.clone();
            }
        }
        // Fallback: cargo sets CARGO_MANIFEST_DIR to the crate root.
        if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
            let p = std::path::PathBuf::from(manifest).join("tests/fixtures/smoke_persona.kdl");
            if p.exists() {
                return p;
            }
        }
        panic!(
            "could not locate smoke_persona.kdl fixture — run tests from workspace root or crate root"
        );
    }

    // -- Load fixture successfully --

    #[test]
    fn loads_smoke_fixture_successfully() {
        let path = fixture_path();
        let snap = load_persona(&path).expect("smoke fixture should load cleanly");

        assert_eq!(snap.name.as_str(), "orual-smoke-test");
        assert_eq!(snap.agent_id.as_str(), "orual-smoke-test");
        assert!(
            snap.system_prompt.is_some(),
            "fixture should have a system_prompt"
        );
        assert!(
            !snap.memory_blocks.is_empty(),
            "fixture should have at least one memory block"
        );
    }

    #[test]
    fn smoke_fixture_model_fields() {
        let snap = load_persona(&fixture_path()).unwrap();
        assert_eq!(snap.model.choice.provider, AdapterKind::Anthropic);
        assert_eq!(snap.model.choice.model_id.as_str(), "claude-sonnet-4-6");
        // temperature is set in the fixture.
        assert!(
            snap.model.chat_options.temperature.is_some(),
            "temperature should be set"
        );
    }

    #[test]
    fn smoke_fixture_budget_fields() {
        let snap = load_persona(&fixture_path()).unwrap();
        assert!(snap.budgets.wall_ms.is_some());
        assert!(snap.budgets.cpu_ms.is_some());
    }

    // -- content_path resolution --

    #[test]
    fn content_path_resolves_relative_to_kdl_dir() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "notes.txt", "hello from notes");

        let kdl_content = r#"
name "content-path-test"

memory {
    notes content-path="notes.txt" {
        memory-type "working"
    }
}
"#;
        let kdl_path = write_file(&dir, "persona.kdl", kdl_content);

        let snap = load_persona(&kdl_path).expect("should load with content_path");
        let block = snap
            .memory_blocks
            .get("notes")
            .expect("notes block missing");
        assert_eq!(
            block.content,
            serde_json::Value::String("hello from notes".to_string())
        );
    }

    #[test]
    fn system_prompt_path_resolves() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "prompt.txt", "you are a test assistant.");
        let kdl_content = r#"
name "prompt-path-test"
system-prompt-path "prompt.txt"
"#;
        let kdl_path = write_file(&dir, "persona.kdl", kdl_content);
        let snap = load_persona(&kdl_path).expect("should resolve system_prompt_path");
        assert_eq!(
            snap.system_prompt.as_deref(),
            Some("you are a test assistant.")
        );
    }

    // -- Unknown fields produce a clear error --

    #[test]
    fn unknown_top_level_field_is_rejected() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "bad"
mystery-field "this should not be accepted"
"#;
        let path = write_file(&dir, "bad.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        // The error must be a parse error that mentions the unknown key.
        assert!(
            msg.contains("parsing") || msg.contains("unknown") || msg.contains("mystery-field"),
            "expected parse/unknown error, got: {msg}"
        );
    }

    #[test]
    fn unknown_model_field_is_rejected() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "bad"

model provider="anthropic" {
    mystery-model-key 42
}
"#;
        let path = write_file(&dir, "bad.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("parsing") || msg.contains("unknown") || msg.contains("mystery-model-key"),
            "expected parse/unknown error for model section, got: {msg}"
        );
    }

    // -- Bad KDL produces an error mentioning "persona" or "parsing" --

    #[test]
    fn malformed_kdl_produces_parse_error() {
        let dir = TempDir::new().unwrap();
        let kdl_content = "name = [this is not valid kdl";
        let path = write_file(&dir, "bad.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("persona") || msg.contains("parsing"),
            "error should mention 'persona' or 'parsing', got: {msg}"
        );
    }

    // -- Missing required field errors name the field --

    #[test]
    fn missing_name_field_produces_informative_error() {
        let dir = TempDir::new().unwrap();
        // A KDL file with no `name` node.
        let kdl_content = r#"
agent-id "no-name-here"

model provider="anthropic" {
}
"#;
        let path = write_file(&dir, "no_name.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        // The error should mention the missing field in some form.
        assert!(
            msg.contains("name") || msg.contains("missing"),
            "error should mention 'name', got: {msg}"
        );
    }

    // -- Conflicting fields --

    #[test]
    fn both_content_and_content_path_is_rejected() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "stuff.txt", "content from file");
        let kdl_content = r#"
name "conflict-test"

memory {
    block content="inline content" content-path="stuff.txt" {
    }
}
"#;
        let path = write_file(&dir, "conflict.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("mutually exclusive") || msg.contains("content"),
            "error should mention mutually exclusive fields, got: {msg}"
        );
    }

    #[test]
    fn both_system_prompt_and_system_prompt_path_is_rejected() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "p.txt", "from file");
        let kdl_content = r#"
name "conflict-test"
system-prompt "inline"
system-prompt-path "p.txt"
"#;
        let path = write_file(&dir, "conflict.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("mutually exclusive") || msg.contains("system_prompt"),
            "error should mention mutually exclusive fields, got: {msg}"
        );
    }

    // -- Unknown provider --

    #[test]
    fn unknown_provider_produces_error() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "bad-provider"

model provider="notareal" model-id="some-model" {
}
"#;
        let path = write_file(&dir, "bad_provider.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("notareal") || msg.contains("provider"),
            "error should mention the bad provider, got: {msg}"
        );
    }

    // -- Agent_id defaults to name --

    #[test]
    fn agent_id_defaults_to_name_when_omitted() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"name "my-agent""#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let snap = load_persona(&path).unwrap();
        assert_eq!(snap.agent_id.as_str(), "my-agent");
        assert_eq!(snap.name.as_str(), "my-agent");
    }

    #[test]
    fn explicit_agent_id_is_used() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "Display Name"
agent-id "stable-id"
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let snap = load_persona(&path).unwrap();
        assert_eq!(snap.agent_id.as_str(), "stable-id");
        assert_eq!(snap.name.as_str(), "Display Name");
    }

    // -- Reasoning effort parsing --

    #[test]
    fn valid_reasoning_effort_is_accepted() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "reasoning-test"

model {
    reasoning-effort "medium"
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let snap = load_persona(&path).unwrap();
        assert!(
            snap.model.chat_options.reasoning_effort.is_some(),
            "reasoning_effort should be set"
        );
    }

    #[test]
    fn invalid_reasoning_effort_produces_error() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "bad-reasoning"

model {
    reasoning-effort "turbo"
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("turbo") || msg.contains("reasoning_effort"),
            "error should mention the bad value, got: {msg}"
        );
    }

    /// `mid-batch "filter_self_edits"` in `context` must propagate through
    /// to `PersonaSnapshot.context.snapshot_policy.mid_batch`.
    #[test]
    fn mid_batch_filter_self_edits_is_loaded() {
        use pattern_core::types::message::MidBatchDeltaBehavior;

        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "mid-batch-test"

context {
    mid-batch "filter_self_edits"
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let snap = load_persona(&path).unwrap();
        assert_eq!(
            snap.context.snapshot_policy.mid_batch,
            MidBatchDeltaBehavior::FilterSelfEdits,
            "mid_batch should be FilterSelfEdits"
        );
    }

    /// `mid-batch "include_self_edits"` (explicit default) round-trips
    /// correctly.
    #[test]
    fn mid_batch_include_self_edits_is_loaded() {
        use pattern_core::types::message::MidBatchDeltaBehavior;

        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "mid-batch-include-test"

context {
    mid-batch "include_self_edits"
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let snap = load_persona(&path).unwrap();
        assert_eq!(
            snap.context.snapshot_policy.mid_batch,
            MidBatchDeltaBehavior::IncludeSelfEdits,
            "mid_batch should be IncludeSelfEdits"
        );
    }

    /// Omitting `mid-batch` from `context` defaults to `IncludeSelfEdits`.
    #[test]
    fn mid_batch_absent_defaults_to_include_self_edits() {
        use pattern_core::types::message::MidBatchDeltaBehavior;

        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "mid-batch-default-test"
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let snap = load_persona(&path).unwrap();
        assert_eq!(
            snap.context.snapshot_policy.mid_batch,
            MidBatchDeltaBehavior::IncludeSelfEdits,
            "absent mid_batch should default to IncludeSelfEdits"
        );
    }

    /// An unrecognised `mid-batch` string must produce a clear error.
    #[test]
    fn invalid_mid_batch_produces_error() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "bad-mid-batch"

context {
    mid-batch "aggressive"
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("aggressive") || msg.contains("mid_batch"),
            "error should mention the bad value, got: {msg}"
        );
    }

    // -- Capabilities + policy KDL parsing (Phase 1 Task 13) --------------

    #[test]
    fn capabilities_block_decodes_effects_and_flags() {
        use pattern_core::{CapabilityFlag, EffectCategory};

        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "scoped-agent"

capabilities {
    effects {
        memory
        message
        tasks
    }
    flags {
        spawn-new-identities
    }
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let snap = load_persona(&path).unwrap();
        let caps = snap.capabilities.expect("capabilities should decode");
        assert!(caps.contains(EffectCategory::Memory));
        assert!(caps.contains(EffectCategory::Message));
        assert!(caps.contains(EffectCategory::Tasks));
        assert!(!caps.contains(EffectCategory::Shell));
        assert!(caps.has_flag(CapabilityFlag::SpawnNewIdentities));
    }

    #[test]
    fn capabilities_with_only_effects_block_decodes_with_empty_flags() {
        use pattern_core::EffectCategory;

        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "effects-only"

capabilities {
    effects {
        memory
    }
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let snap = load_persona(&path).unwrap();
        let caps = snap.capabilities.expect("capabilities should decode");
        assert!(caps.contains(EffectCategory::Memory));
        assert_eq!(caps.iter_flags().count(), 0);
    }

    #[test]
    fn empty_capabilities_block_decodes_to_empty_set() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "pure"

capabilities {
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let snap = load_persona(&path).unwrap();
        let caps = snap.capabilities.expect("capabilities should decode");
        assert_eq!(caps.iter_categories().count(), 0);
        assert_eq!(caps.iter_flags().count(), 0);
    }

    #[test]
    fn no_capabilities_block_means_unset() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"name "default-caps""#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let snap = load_persona(&path).unwrap();
        assert!(
            snap.capabilities.is_none(),
            "no capabilities block → field stays None (back-compat)"
        );
    }

    #[test]
    fn unknown_effect_in_capabilities_errors() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "bad-effect"

capabilities {
    effects {
        memory
        nonsense
    }
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        assert!(
            err.to_string().contains("nonsense"),
            "error should name the bad effect, got: {err}"
        );
    }

    #[test]
    fn unknown_flag_in_capabilities_errors() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "bad-flag"

capabilities {
    flags {
        unauthorized-magic
    }
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        assert!(
            err.to_string().contains("unauthorized-magic"),
            "error should name the bad flag, got: {err}"
        );
    }

    #[test]
    fn policy_block_decodes_rules() {
        use pattern_core::{EffectCategory, PolicyAction, PolicyMatcher, Precedence};

        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "policy-test"

policy {
    rule "allow-git-push" effect="shell" action="allow" {
        matcher "shell-command" pattern="git push*"
    }
    rule "gate-all-file-writes" effect="file" action="require-approval" {
        matcher "file-path" pattern="*"
        reason "all file writes gated for this persona"
    }
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let snap = load_persona(&path).unwrap();
        assert_eq!(snap.policy_rules.len(), 2);

        let allow = &snap.policy_rules[0];
        assert_eq!(allow.effect, EffectCategory::Shell);
        assert!(matches!(allow.precedence, Precedence::KdlConfig));
        assert!(matches!(allow.action, PolicyAction::Allow));
        match &allow.matcher {
            PolicyMatcher::ShellCommand { pattern } => assert_eq!(pattern, "git push*"),
            other => panic!("expected ShellCommand matcher, got {other:?}"),
        }

        let gate = &snap.policy_rules[1];
        assert_eq!(gate.effect, EffectCategory::File);
        match &gate.action {
            PolicyAction::RequireApproval { reason } => {
                assert_eq!(
                    reason.as_deref(),
                    Some("all file writes gated for this persona")
                );
            }
            other => panic!("expected RequireApproval, got {other:?}"),
        }
        match &gate.matcher {
            PolicyMatcher::FilePath { pattern } => assert_eq!(pattern, "*"),
            other => panic!("expected FilePath matcher, got {other:?}"),
        }
    }

    #[test]
    fn policy_rule_with_unknown_action_errors() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "bad-action"

policy {
    rule "weird" effect="shell" action="meh" {
        matcher "always"
    }
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        assert!(
            err.to_string().contains("meh"),
            "error should name the bad action, got: {err}"
        );
    }

    #[test]
    fn policy_shell_command_matcher_requires_pattern_property() {
        let dir = TempDir::new().unwrap();
        let kdl_content = r#"
name "missing-pattern"

policy {
    rule "no-pattern" effect="shell" action="allow" {
        matcher "shell-command"
    }
}
"#;
        let path = write_file(&dir, "p.kdl", kdl_content);
        let err = load_persona(&path).unwrap_err();
        assert!(
            err.to_string().contains("pattern"),
            "error should mention missing pattern, got: {err}"
        );
    }
}
