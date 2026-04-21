//! Persona TOML loader for `pattern-test-cli`.
//!
//! Reads a `.toml` file on disk and converts it into a `PersonaSnapshot`
//! (from `pattern_core::types::agent`) ready to hand to the
//! `open_with_agent_loop` method of `TidepoolSession` (from `crate::session`).
//!
//! ## TOML schema
//!
//! ```toml
//! name = "orual-smoke-test"
//! agent_id = "orual-smoke-test"   # optional; defaults to `name` if omitted
//!
//! # Optional slot[1] override.
//! system_prompt = "You are a helpful test assistant."
//! # OR: system_prompt_path = "./system_prompt.txt"
//!
//! [model]
//! provider  = "anthropic"         # case-insensitive lowercase AdapterKind
//! model_id  = "claude-sonnet-4-6"
//! # Sampling knobs (all optional):
//! temperature = 0.7
//! max_tokens  = 4096
//! # reasoning_effort = "medium"   # None | Low | Medium | High | XHigh | Max
//!
//! [context]
//! compress_check_message_floor = 50
//! compress_token_threshold     = 150_000
//! # "include_self_edits" (default) or "filter_self_edits"
//! mid_batch = "filter_self_edits"
//!
//! [context.compression]
//! type                 = "recursive_summarization"
//! chunk_size           = 20
//! summarization_model  = "claude-haiku-4-5"
//!
//! [budgets]
//! wall_ms = 30_000
//! cpu_ms  = 10_000
//!
//! [memory.persona]
//! content      = "I am a minimal smoke-test persona."
//! memory_type  = "core"
//! permission   = "read_write"
//! pinned       = true
//!
//! [memory.scratchpad]
//! content_path = "./scratchpad.txt"   # resolved relative to the TOML file
//! memory_type  = "working"
//! permission   = "read_write"
//! ```
//!
//! Unknown top-level or section keys are rejected with an error that names the
//! offending key. Missing required fields (`name`) produce an error that names
//! the field.

use std::collections::HashMap;
use std::path::Path;

use genai::adapter::AdapterKind;
use genai::chat::{ChatOptions, ReasoningEffort};
use miette::Diagnostic;
use pattern_core::types::compression::CompressionStrategy;
use pattern_core::types::memory_types::{MemoryPermission, MemoryType};
use pattern_core::types::message::MidBatchDeltaBehavior;
use pattern_core::types::snapshot::{
    ContextPolicy, MemoryBlockSpec, ModelChoice, ModelSpec, PersonaSnapshot,
};
use serde::Deserialize;
use smol_str::SmolStr;
use thiserror::Error;

// ==========================================================================
// Public error type
// ==========================================================================

/// Errors that can occur while loading a persona TOML file.
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

    /// The file content is not valid TOML, or has unknown fields.
    #[error("error parsing persona TOML at {path}: {message}")]
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

    /// An unknown provider string in `[model].provider`.
    #[error(
        "persona file at {path}: unknown provider `{provider}` in [model]; \
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

    /// An unknown `mid_batch` string in `[context]`.
    #[error(
        "persona file at {path}: unknown mid_batch `{value}`; expected: include_self_edits, filter_self_edits"
    )]
    #[diagnostic(code(persona::unknown_mid_batch))]
    UnknownMidBatch { path: String, value: String },
}

// ==========================================================================
// Public entry point
// ==========================================================================

/// Load a [`PersonaSnapshot`] from a TOML file at `path`.
///
/// # Errors
///
/// Returns a [`PersonaLoadError`] (wrapped in [`miette::Report`]) if the file
/// cannot be read, contains invalid TOML, uses unknown fields, is missing the
/// required `name` field, or has conflicting / unresolvable content references.
pub fn load_persona(path: &Path) -> miette::Result<PersonaSnapshot> {
    load_persona_inner(path).map_err(miette::Report::new)
}

fn load_persona_inner(path: &Path) -> Result<PersonaSnapshot, PersonaLoadError> {
    let path_str = path.display().to_string();

    // Read raw bytes.
    let raw = std::fs::read_to_string(path).map_err(|e| PersonaLoadError::Io {
        path: path_str.clone(),
        source: e,
    })?;

    // Parse into our DTO, rejecting unknown fields.
    let file: PersonaFile = toml::from_str(&raw).map_err(|e| PersonaLoadError::Parse {
        path: path_str.clone(),
        message: e.to_string(),
    })?;

    // The directory the TOML lives in — used to resolve relative paths.
    let base_dir = path.parent().unwrap_or(Path::new("."));

    convert(file, base_dir, &path_str)
}

// ==========================================================================
// TOML DTO types
// ==========================================================================

/// Top-level structure of a persona TOML file.
///
/// `#[serde(deny_unknown_fields)]` ensures that typos or unrecognised keys
/// are caught at parse time rather than silently dropped.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonaFile {
    /// Display name and (if `agent_id` is absent) the agent identifier.
    name: String,

    /// Stable agent identifier. Defaults to `name` when absent.
    #[serde(default)]
    agent_id: Option<String>,

    // -- System prompt (mutually exclusive) --
    /// Inline slot-[1] system prompt override.
    #[serde(default)]
    system_prompt: Option<String>,

    /// Path to a file whose content becomes the slot-[1] system prompt.
    /// Resolved relative to the persona TOML's directory.
    #[serde(default)]
    system_prompt_path: Option<String>,

    // -- Sub-tables --
    #[serde(default)]
    model: ModelFile,

    #[serde(default)]
    context: ContextFile,

    #[serde(default)]
    budgets: BudgetsFile,

    /// Memory block definitions keyed by label.
    #[serde(default)]
    memory: HashMap<String, MemoryBlockFile>,
}

/// `[model]` table.
///
/// Sampling knobs are listed explicitly here (instead of `#[serde(flatten)]`
/// wrapping `ChatOptions`) because TOML's flatten support has edge-case
/// interactions with `deny_unknown_fields`. Explicit fields produce clearer
/// error messages.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ModelFile {
    /// Provider name — case-insensitive lowercase, e.g. `"anthropic"`.
    #[serde(default)]
    provider: Option<String>,

    /// Provider-specific model identifier.
    #[serde(default)]
    model_id: Option<String>,

    // -- ChatOptions fields --
    #[serde(default)]
    temperature: Option<f64>,

    #[serde(default)]
    max_tokens: Option<u32>,

    #[serde(default)]
    top_p: Option<f64>,

    /// Reasoning effort level: "none", "low", "medium", "high", "xhigh", "max".
    #[serde(default)]
    reasoning_effort: Option<String>,

    #[serde(default)]
    seed: Option<u64>,
}

/// `[context]` table.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ContextFile {
    /// Cheap short-circuit floor for the compression gate.
    #[serde(default)]
    compress_check_message_floor: Option<usize>,

    /// Real token threshold above which compression fires.
    #[serde(default)]
    compress_token_threshold: Option<usize>,

    /// Compression strategy applied when the gate fires. Accepts the
    /// `CompressionStrategy` tagged enum (`{ type = "truncate", keep_recent = 100 }`,
    /// `{ type = "recursive_summarization", ... }`, etc.). None disables
    /// compression for this persona.
    #[serde(default)]
    compression: Option<CompressionStrategy>,

    /// Mid-batch delta snapshot behaviour. Accepted values:
    /// - `"include_self_edits"` (default) — emit delta for all mid-batch
    ///   changes, including this turn's own tool-initiated writes.
    /// - `"filter_self_edits"` — emit delta only for changes NOT attributable
    ///   to this turn's own block_writes (cache-efficient; relies on
    ///   tool_result confirmation instead).
    ///
    /// Corresponds to
    /// [`pattern_core::types::message::MidBatchDeltaBehavior`].
    #[serde(default)]
    mid_batch: Option<String>,
}

/// `[budgets]` table.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct BudgetsFile {
    #[serde(default)]
    wall_ms: Option<u64>,

    #[serde(default)]
    cpu_ms: Option<u64>,

    #[serde(default)]
    hard_abandon_ms: Option<u64>,

    #[serde(default)]
    cancel_grace_ms: Option<u64>,

    #[serde(default)]
    nursery_size: Option<usize>,
}

/// One `[memory.<label>]` block.
///
/// Exactly one of `content` or `content_path` should be provided. Both
/// absent results in a null/empty block. Both present is an error caught
/// at conversion time.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct MemoryBlockFile {
    /// Inline text content.
    #[serde(default)]
    content: Option<String>,

    /// Path to a file whose text content is used.
    /// Resolved relative to the persona TOML's directory.
    #[serde(default)]
    content_path: Option<String>,

    /// Memory tier.  Serialised as "core", "working", "archival".
    #[serde(default)]
    memory_type: Option<MemoryType>,

    /// Permission level.  Serialised as "read_write", "read_only", etc.
    #[serde(default)]
    permission: Option<MemoryPermission>,

    /// Human-readable description.
    #[serde(default)]
    description: Option<String>,

    /// Whether the block is pinned in context unconditionally.
    #[serde(default)]
    pinned: Option<bool>,

    /// Maximum content size in characters.
    #[serde(default)]
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
    for (label, block_file) in file.memory {
        let spec = convert_memory_block(block_file, base_dir, path_str, &label)?;
        snap = snap.with_memory_block(SmolStr::from(label), spec);
    }

    Ok(snap)
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

fn convert_context(file: ContextFile, path_str: &str) -> Result<ContextPolicy, PersonaLoadError> {
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
    if file.compression.is_some() {
        policy = policy.with_compression(file.compression);
    }
    Ok(policy)
}

fn convert_model(file: ModelFile, path_str: &str) -> Result<ModelSpec, PersonaLoadError> {
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
    file: MemoryBlockFile,
    base_dir: &Path,
    persona_path: &str,
    label: &str,
) -> Result<MemoryBlockSpec, PersonaLoadError> {
    // Inline content key for the error message context.
    let inline_key = format!("memory.{label}.content");
    let path_key = format!("memory.{label}.content_path");

    let content_str = resolve_string_or_path(
        file.content,
        file.content_path,
        &inline_key,
        &path_key,
        base_dir,
        persona_path,
    )?;

    // Wrap the resolved string as a JSON string value, or use Null when absent.
    // MemoryBlockSpec::text() wraps a String as JsonValue::String; for the
    // absent-content case we use Default (which sets content = Null).
    let mut spec = match content_str {
        Some(s) => MemoryBlockSpec::text(s),
        None => MemoryBlockSpec::default(),
    };

    if let Some(mt) = file.memory_type {
        spec = spec.with_memory_type(mt);
    }
    if let Some(perm) = file.permission {
        spec = spec.with_permission(perm);
    }
    if let Some(desc) = file.description {
        spec = spec.with_description(desc);
    }
    if let Some(pinned) = file.pinned {
        spec = spec.with_pinned(pinned);
    }
    if let Some(limit) = file.char_limit {
        spec = spec.with_char_limit(limit);
    }

    Ok(spec)
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
            std::path::PathBuf::from("crates/pattern_runtime/tests/fixtures/smoke_persona.toml"),
            std::path::PathBuf::from("tests/fixtures/smoke_persona.toml"),
        ];
        for p in &candidates {
            if p.exists() {
                return p.clone();
            }
        }
        // Fallback: cargo sets CARGO_MANIFEST_DIR to the crate root.
        if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
            let p = std::path::PathBuf::from(manifest).join("tests/fixtures/smoke_persona.toml");
            if p.exists() {
                return p;
            }
        }
        panic!(
            "could not locate smoke_persona.toml fixture — run tests from workspace root or crate root"
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
    fn content_path_resolves_relative_to_toml_dir() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "notes.txt", "hello from notes");

        let toml_content = r#"
name = "content-path-test"

[memory.notes]
content_path = "notes.txt"
memory_type  = "working"
"#;
        let toml_path = write_file(&dir, "persona.toml", toml_content);

        let snap = load_persona(&toml_path).expect("should load with content_path");
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
        let toml_content = r#"
name = "prompt-path-test"
system_prompt_path = "prompt.txt"
"#;
        let toml_path = write_file(&dir, "persona.toml", toml_content);
        let snap = load_persona(&toml_path).expect("should resolve system_prompt_path");
        assert_eq!(
            snap.system_prompt.as_deref(),
            Some("you are a test assistant.")
        );
    }

    // -- Unknown fields produce a clear error --

    #[test]
    fn unknown_top_level_field_is_rejected() {
        let dir = TempDir::new().unwrap();
        let toml_content = r#"
name = "bad"
mystery_field = "this should not be accepted"
"#;
        let path = write_file(&dir, "bad.toml", toml_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        // The error must be a parse error that mentions the unknown key.
        assert!(
            msg.contains("parse") || msg.contains("unknown") || msg.contains("mystery_field"),
            "expected parse/unknown error, got: {msg}"
        );
    }

    #[test]
    fn unknown_model_field_is_rejected() {
        let dir = TempDir::new().unwrap();
        let toml_content = r#"
name = "bad"

[model]
provider = "anthropic"
mystery_model_key = 42
"#;
        let path = write_file(&dir, "bad.toml", toml_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("parse") || msg.contains("unknown") || msg.contains("mystery_model_key"),
            "expected parse/unknown error for model section, got: {msg}"
        );
    }

    // -- Bad TOML produces an error mentioning "persona" or "parsing" --

    #[test]
    fn malformed_toml_produces_parse_error() {
        let dir = TempDir::new().unwrap();
        let toml_content = "name = [this is not valid toml";
        let path = write_file(&dir, "bad.toml", toml_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("persona") || msg.contains("parsing") || msg.contains("parse"),
            "error should mention 'persona' or 'parsing', got: {msg}"
        );
    }

    // -- Missing required field errors name the field --

    #[test]
    fn missing_name_field_produces_informative_error() {
        let dir = TempDir::new().unwrap();
        // A TOML file with no `name` key.
        let toml_content = r#"
agent_id = "no-name-here"

[model]
provider = "anthropic"
"#;
        let path = write_file(&dir, "no_name.toml", toml_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        // The error should mention the missing field in some form.
        assert!(
            msg.contains("name") || msg.contains("missing field"),
            "error should mention 'name', got: {msg}"
        );
    }

    // -- Conflicting fields --

    #[test]
    fn both_content_and_content_path_is_rejected() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "stuff.txt", "content from file");
        let toml_content = r#"
name = "conflict-test"

[memory.block]
content      = "inline content"
content_path = "stuff.txt"
"#;
        let path = write_file(&dir, "conflict.toml", toml_content);
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
        let toml_content = r#"
name = "conflict-test"
system_prompt      = "inline"
system_prompt_path = "p.txt"
"#;
        let path = write_file(&dir, "conflict.toml", toml_content);
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
        let toml_content = r#"
name = "bad-provider"

[model]
provider = "notareal"
model_id = "some-model"
"#;
        let path = write_file(&dir, "bad_provider.toml", toml_content);
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
        let toml_content = r#"name = "my-agent""#;
        let path = write_file(&dir, "p.toml", toml_content);
        let snap = load_persona(&path).unwrap();
        assert_eq!(snap.agent_id.as_str(), "my-agent");
        assert_eq!(snap.name.as_str(), "my-agent");
    }

    #[test]
    fn explicit_agent_id_is_used() {
        let dir = TempDir::new().unwrap();
        let toml_content = r#"
name     = "Display Name"
agent_id = "stable-id"
"#;
        let path = write_file(&dir, "p.toml", toml_content);
        let snap = load_persona(&path).unwrap();
        assert_eq!(snap.agent_id.as_str(), "stable-id");
        assert_eq!(snap.name.as_str(), "Display Name");
    }

    // -- Reasoning effort parsing --

    #[test]
    fn valid_reasoning_effort_is_accepted() {
        let dir = TempDir::new().unwrap();
        let toml_content = r#"
name = "reasoning-test"

[model]
reasoning_effort = "medium"
"#;
        let path = write_file(&dir, "p.toml", toml_content);
        let snap = load_persona(&path).unwrap();
        assert!(
            snap.model.chat_options.reasoning_effort.is_some(),
            "reasoning_effort should be set"
        );
    }

    #[test]
    fn invalid_reasoning_effort_produces_error() {
        let dir = TempDir::new().unwrap();
        let toml_content = r#"
name = "bad-reasoning"

[model]
reasoning_effort = "turbo"
"#;
        let path = write_file(&dir, "p.toml", toml_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("turbo") || msg.contains("reasoning_effort"),
            "error should mention the bad value, got: {msg}"
        );
    }

    /// `mid_batch = "filter_self_edits"` in `[context]` must propagate through
    /// to `PersonaSnapshot.context.snapshot_policy.mid_batch`.
    ///
    /// Regression test for fix #11 (code-review finding: snapshot_policy
    /// .mid_batch not exposed in persona TOML).
    #[test]
    fn mid_batch_filter_self_edits_is_loaded() {
        use pattern_core::types::message::MidBatchDeltaBehavior;

        let dir = TempDir::new().unwrap();
        let toml_content = r#"
name = "mid-batch-test"

[context]
mid_batch = "filter_self_edits"
"#;
        let path = write_file(&dir, "p.toml", toml_content);
        let snap = load_persona(&path).unwrap();
        assert_eq!(
            snap.context.snapshot_policy.mid_batch,
            MidBatchDeltaBehavior::FilterSelfEdits,
            "mid_batch should be FilterSelfEdits"
        );
    }

    /// `mid_batch = "include_self_edits"` (explicit default) round-trips
    /// correctly.
    #[test]
    fn mid_batch_include_self_edits_is_loaded() {
        use pattern_core::types::message::MidBatchDeltaBehavior;

        let dir = TempDir::new().unwrap();
        let toml_content = r#"
name = "mid-batch-include-test"

[context]
mid_batch = "include_self_edits"
"#;
        let path = write_file(&dir, "p.toml", toml_content);
        let snap = load_persona(&path).unwrap();
        assert_eq!(
            snap.context.snapshot_policy.mid_batch,
            MidBatchDeltaBehavior::IncludeSelfEdits,
            "mid_batch should be IncludeSelfEdits"
        );
    }

    /// Omitting `mid_batch` from `[context]` defaults to `IncludeSelfEdits`.
    #[test]
    fn mid_batch_absent_defaults_to_include_self_edits() {
        use pattern_core::types::message::MidBatchDeltaBehavior;

        let dir = TempDir::new().unwrap();
        let toml_content = r#"
name = "mid-batch-default-test"
"#;
        let path = write_file(&dir, "p.toml", toml_content);
        let snap = load_persona(&path).unwrap();
        assert_eq!(
            snap.context.snapshot_policy.mid_batch,
            MidBatchDeltaBehavior::IncludeSelfEdits,
            "absent mid_batch should default to IncludeSelfEdits"
        );
    }

    /// An unrecognised `mid_batch` string must produce a clear error.
    #[test]
    fn invalid_mid_batch_produces_error() {
        let dir = TempDir::new().unwrap();
        let toml_content = r#"
name = "bad-mid-batch"

[context]
mid_batch = "aggressive"
"#;
        let path = write_file(&dir, "p.toml", toml_content);
        let err = load_persona(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("aggressive") || msg.contains("mid_batch"),
            "error should mention the bad value, got: {msg}"
        );
    }
}
