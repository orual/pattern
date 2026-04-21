//! Typed representation of a `.pattern.kdl` mount configuration file.
//!
//! Parses the KDL document that lives at `<mount>/.pattern.kdl` into a
//! `MountConfig` using the `knus` derive macros. The top-level call is
//! [`load_mount_config`].
//!
//! # Example `.pattern.kdl`
//!
//! KDL identifiers use kebab-case, matching what the knus derive macros expect
//! when mapping Rust snake_case field names.
//!
//! ```text
//! mount mode="A" memory-db="memory.db"
//!
//! personas {
//!     default "@pattern-default"
//! }
//!
//! isolate-from-persona policy="none"
//!
//! jj enabled=false
//!
//! project name="my-project" created-at="2026-04-19T12:00:00Z"
//! ```

use std::path::Path;

use knus::Decode;
use serde::Serialize;

use super::ConfigError;

// ---------------------------------------------------------------------------
// Top-level document
// ---------------------------------------------------------------------------

/// Parsed representation of a `.pattern.kdl` mount config file.
///
/// Fields map one-to-one to top-level KDL nodes; see the module-level
/// example for a representative document.
///
/// KDL uses kebab-case for node names and property keys; the knus derive
/// converts snake_case field names to kebab-case automatically. For example,
/// the `isolate_from_persona` field maps to the `isolate-from-persona` KDL node.
#[derive(Debug, Clone, Decode, Serialize)]
pub struct MountConfig {
    /// `mount` node — storage mode and DB filename.
    ///
    /// KDL: `mount mode="A" memory-db="memory.db"`
    #[knus(child)]
    pub mount: MountSection,

    /// `personas` block — optional; defaults to an empty list.
    ///
    /// KDL: `personas { default "@pattern-default" }`
    #[knus(child, default)]
    pub personas: PersonasSection,

    /// `isolate-from-persona` node — optional; defaults to policy `"none"`.
    ///
    /// KDL: `isolate-from-persona policy="none"`
    #[knus(child, default)]
    pub isolate_from_persona: IsolateSection,

    /// `jj` integration settings — optional; defaults to `enabled=false`.
    ///
    /// KDL: `jj enabled=false`
    #[knus(child, default)]
    pub jj: JjSection,

    /// `project` node — name and creation timestamp.
    ///
    /// KDL: `project name="my-project" created-at="2026-04-19T12:00:00Z"`
    #[knus(child)]
    pub project: ProjectSection,
}

// ---------------------------------------------------------------------------
// Section structs
// ---------------------------------------------------------------------------

/// The `mount` node: controls storage mode and memory DB filename.
///
/// KDL: `mount mode="A" memory-db="memory.db"`
#[derive(Debug, Clone, Decode, Serialize)]
pub struct MountSection {
    /// Storage mode: `"A"` (in-repo), `"B"` (pattern-jj), or `"C"` (sidecar).
    ///
    /// KDL property: `mode`
    #[knus(property)]
    pub mode: ModeKind,

    /// Relative path to `memory.db` from the mount root.
    ///
    /// KDL property: `memory-db` (the knus derive converts `memory_db` →
    /// `memory-db`).
    #[knus(property)]
    pub memory_db: String,
}

/// Storage mode identifier parsed from the `mode` property of the `mount` node.
///
/// The KDL value must be the uppercase letter `"A"`, `"B"`, or `"C"`.
/// `DecodeScalar` is implemented manually rather than derived so that the
/// canonical form stays uppercase (the `DecodeScalar` derive would lower-case
/// the variants via kebab-case conversion).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ModeKind {
    /// Mode A: in-repo storage; host VCS owns history.
    A,
    /// Mode B: separate Pattern-owned jj repository.
    B,
    /// Mode C: sidecar jj alongside host git (experimental).
    C,
}

impl<S: knus::traits::ErrorSpan> knus::DecodeScalar<S> for ModeKind {
    fn type_check(
        type_name: &Option<knus::span::Spanned<knus::ast::TypeName, S>>,
        ctx: &mut knus::decode::Context<S>,
    ) {
        // ModeKind accepts no KDL type annotations — reject any that are given.
        if let Some(typ) = type_name {
            ctx.emit_error(knus::errors::DecodeError::TypeName {
                span: typ.span().clone(),
                found: Some((**typ).clone()),
                expected: knus::errors::ExpectedType::no_type(),
                rust_type: "ModeKind",
            });
        }
    }

    fn raw_decode(
        val: &knus::span::Spanned<knus::ast::Literal, S>,
        ctx: &mut knus::decode::Context<S>,
    ) -> Result<ModeKind, knus::errors::DecodeError<S>> {
        match &**val {
            knus::ast::Literal::String(s) => match s.as_ref() {
                "A" => Ok(ModeKind::A),
                "B" => Ok(ModeKind::B),
                "C" => Ok(ModeKind::C),
                _ => {
                    // Emit the scalar-kind error to get a good diagnostic, then
                    // return a fallback. knus requires raw_decode to return a
                    // valid value even on error because knus collects errors
                    // separately and surfaces them all at the end rather than
                    // short-circuiting. We fall back to Mode B (not A) because
                    // Mode B keeps all data inside ~/.pattern/ and never
                    // pollutes a project directory.
                    ctx.emit_error(knus::errors::DecodeError::scalar_kind(
                        knus::decode::Kind::String,
                        val,
                    ));
                    Ok(ModeKind::B)
                }
            },
            _ => {
                ctx.emit_error(knus::errors::DecodeError::scalar_kind(
                    knus::decode::Kind::String,
                    val,
                ));
                // Same fallback rationale as above: Mode B is safer than A on
                // error because it stays within ~/.pattern/ and cannot
                // accidentally pollute a project directory.
                Ok(ModeKind::B)
            }
        }
    }
}

/// The `personas` block: maps slot names to persona handles.
///
/// KDL:
/// ```text
/// personas {
///     default "@pattern-default"
///     focused "@pattern-focus"
/// }
/// ```
#[derive(Debug, Clone, Default, Decode, Serialize)]
pub struct PersonasSection {
    /// Child nodes: each node's name is the slot (e.g. `default`) and its
    /// single argument is the persona handle (e.g. `"@pattern-default"`).
    #[knus(children)]
    pub entries: Vec<PersonaBinding>,
}

/// A single `<slot> "<handle>"` line inside the `personas` block.
#[derive(Debug, Clone, Decode, Serialize)]
pub struct PersonaBinding {
    /// The KDL node name used as the slot identifier (e.g. `"default"`).
    #[knus(node_name)]
    pub slot: String,
    /// The persona handle string (e.g. `"@pattern-default"`).
    #[knus(argument)]
    pub persona: String,
}

/// The `isolate-from-persona` node: persona isolation policy.
///
/// KDL: `isolate-from-persona policy="none"`
#[derive(Debug, Clone, Decode, Serialize)]
pub struct IsolateSection {
    /// Isolation policy: `"none"`, `"core-only"`, or `"full"`.
    ///
    /// Defaults to `"none"` when the entire node is absent.
    #[knus(property, default = "none".to_string())]
    pub policy: String,
}

impl Default for IsolateSection {
    fn default() -> Self {
        Self {
            policy: "none".to_string(),
        }
    }
}

/// The `jj` node: controls whether Pattern invokes `jj` for VCS history.
///
/// KDL: `jj enabled=false max-new-file-size="100MiB"`
#[derive(Debug, Clone, Decode, Serialize)]
pub struct JjSection {
    /// Whether Pattern's jj integration is enabled for this mount.
    ///
    /// Defaults to `false` when the entire node is absent.
    #[knus(property, default = false)]
    pub enabled: bool,

    /// Maximum size for new files tracked by jj.
    ///
    /// KDL property: `max-new-file-size` (the knus derive converts
    /// `max_new_file_size` → `max-new-file-size`).
    ///
    /// Defaults to `"100MiB"` when the entire node is absent.
    #[knus(property, default = "100MiB".to_string())]
    pub max_new_file_size: String,
}

impl Default for JjSection {
    fn default() -> Self {
        Self {
            enabled: false,
            max_new_file_size: "100MiB".to_string(),
        }
    }
}

/// The `project` node: stable project identity metadata.
///
/// KDL: `project name="my-project" created-at="2026-04-19T12:00:00Z"`
#[derive(Debug, Clone, Decode, Serialize)]
pub struct ProjectSection {
    /// Human-readable project name used for path construction in Mode B.
    ///
    /// KDL property: `name`
    #[knus(property)]
    pub name: String,

    /// ISO 8601 timestamp string recording when this mount was initialized.
    ///
    /// KDL property: `created-at` (the knus derive converts `created_at` →
    /// `created-at`).
    #[knus(property)]
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------

/// Load and parse a `.pattern.kdl` config from the given path.
///
/// Returns a [`MountConfig`] on success, or a [`ConfigError`] with
/// line/column span information on parse failure, or an I/O error if the
/// file cannot be read.
///
/// # Errors
///
/// - [`ConfigError::Io`] — the file could not be read.
/// - [`ConfigError::Parse`] — the KDL is malformed or the schema doesn't match.
/// - [`ConfigError::Validation`] — post-parse cross-field constraint violated.
pub fn load_mount_config(path: &Path) -> Result<MountConfig, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.to_owned(),
        source: e,
    })?;
    let config = knus::parse::<MountConfig>(&path.display().to_string(), &text).map_err(|e| {
        ConfigError::Parse {
            path: path.to_owned(),
            source: e,
        }
    })?;
    validate_config(&config, path)?;
    Ok(config)
}

// ---------------------------------------------------------------------------
// Post-parse validation
// ---------------------------------------------------------------------------

/// Enforce cross-field constraints that KDL syntax alone cannot express.
///
/// Rules validated here:
/// - Mode B requires `jj enabled=true` (Pattern owns VCS history).
/// - Mode C requires `jj enabled=true` (sidecar jj must be active).
/// - `isolate-from-persona policy` must be one of `"none"`, `"core-only"`,
///   or `"full"`.
///
/// Path-level constraints (e.g. Mode A requiring a hashable project root)
/// are deferred to attach time, since parse time does not know the project
/// root path.
fn validate_config(config: &MountConfig, path: &Path) -> Result<(), ConfigError> {
    match config.mount.mode {
        ModeKind::B | ModeKind::C if !config.jj.enabled => {
            return Err(ConfigError::Validation {
                path: path.to_owned(),
                reason: format!(
                    "mode {} requires `jj enabled=true` but `jj.enabled` is false",
                    match config.mount.mode {
                        ModeKind::B => "B",
                        ModeKind::C => "C",
                        ModeKind::A => unreachable!(),
                    }
                ),
            });
        }
        _ => {}
    }

    // Validate the isolation policy is a known value. The KDL type is a raw
    // String, so we must validate explicitly rather than relying on the parser.
    let policy = config.isolate_from_persona.policy.as_str();
    if !matches!(policy, "none" | "core-only" | "full") {
        return Err(ConfigError::Validation {
            path: path.to_owned(),
            reason: format!(
                "isolate-from-persona policy must be \"none\", \"core-only\", or \"full\", got \"{policy}\""
            ),
        });
    }

    Ok(())
}
