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
use knus::ast::{Literal, SpannedNode};
use knus::decode::Context;
use knus::errors::DecodeError;
use knus::traits::{DecodeChildren, ErrorSpan};
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

    /// `backup` node — snapshot scheduling and retention policy.
    ///
    /// Optional; when absent, no automatic snapshots are taken.
    ///
    /// KDL (optional):
    /// ```text
    /// backup snapshot-interval="1h" {
    ///     keep-recent 24
    ///     hourly-days 1
    ///     daily-months 1
    ///     monthly-forever true
    /// }
    /// ```
    #[knus(child)]
    pub backup: Option<BackupSection>,

    /// `file-policy` block — ordered allow/deny rules for agent file access.
    ///
    /// Optional; when absent (or empty), all file access is denied by default.
    /// Rules are evaluated in declaration order with last-match-wins semantics.
    ///
    /// KDL (optional):
    /// ```text
    /// file-policy {
    ///     allow "/project/**"
    ///     deny  "/project/.env"
    /// }
    /// ```
    #[knus(child, default)]
    pub file_policy: FilePolicySection,
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
/// The canonical KDL values are `"in-repo"`, `"standalone"`, and `"sidecar"`.
/// The legacy uppercase letters `"A"`, `"B"`, and `"C"` are still accepted for
/// backward compatibility with older `.pattern.kdl` files — they map onto the
/// new names without warning. `DecodeScalar` is implemented manually rather
/// than derived so the canonical form stays stable against kebab-case
/// conversion and we can recognise the legacy aliases explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ModeKind {
    /// In-repo storage; host VCS owns history. (Legacy alias: `"A"`.)
    InRepo,
    /// Separate Pattern-owned jj repository. (Legacy alias: `"B"`.)
    Standalone,
    /// Sidecar jj alongside host git. (Legacy alias: `"C"`.)
    Sidecar,
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
                // Canonical names.
                "in-repo" => Ok(ModeKind::InRepo),
                "standalone" => Ok(ModeKind::Standalone),
                "sidecar" => Ok(ModeKind::Sidecar),
                // Legacy single-letter aliases from pre-rename `.pattern.kdl`
                // files. Kept indefinitely — cheap to support, protects users
                // from a lossy upgrade.
                "A" => Ok(ModeKind::InRepo),
                "B" => Ok(ModeKind::Standalone),
                "C" => Ok(ModeKind::Sidecar),
                _ => {
                    // Emit the scalar-kind error to get a good diagnostic, then
                    // return a fallback. knus requires raw_decode to return a
                    // valid value even on error because knus collects errors
                    // separately and surfaces them all at the end rather than
                    // short-circuiting. We fall back to Standalone (not InRepo)
                    // because Standalone keeps all data inside ~/.pattern/ and
                    // never pollutes a project directory.
                    ctx.emit_error(knus::errors::DecodeError::scalar_kind(
                        knus::decode::Kind::String,
                        val,
                    ));
                    Ok(ModeKind::Standalone)
                }
            },
            _ => {
                ctx.emit_error(knus::errors::DecodeError::scalar_kind(
                    knus::decode::Kind::String,
                    val,
                ));
                // Same fallback rationale as above: Standalone is safer than
                // InRepo on error because it stays within ~/.pattern/ and
                // cannot accidentally pollute a project directory.
                Ok(ModeKind::Standalone)
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

impl IsolateSection {
    /// Convert the validated policy string into a typed [`IsolatePolicy`].
    ///
    /// The policy string is already validated at parse time (see
    /// [`validate_config`]), so this method only needs to handle the known
    /// values. Unknown values produce a [`ConfigError::Validation`] with a
    /// helpful message.
    pub fn resolve(&self) -> Result<pattern_core::types::memory_types::IsolatePolicy, ConfigError> {
        use pattern_core::types::memory_types::IsolatePolicy;
        match self.policy.as_str() {
            "none" => Ok(IsolatePolicy::None),
            "core-only" => Ok(IsolatePolicy::CoreOnly),
            "full" => Ok(IsolatePolicy::Full),
            other => Err(ConfigError::Validation {
                path: std::path::PathBuf::from(".pattern.kdl"),
                reason: format!(
                    "invalid isolate_from_persona.policy: {other:?}; \
                     expected none | core-only | full"
                ),
            }),
        }
    }
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
    /// Human-readable project name used for path construction in Standalone mode.
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

/// The `backup` node: snapshot scheduling and retention policy configuration.
///
/// Optional — when absent, no automatic snapshots are taken (manual-only via
/// `pattern backup create`).
///
/// KDL example:
/// ```text
/// backup snapshot-interval="1h" {
///     keep-recent 24
///     hourly-days 1
///     daily-months 1
///     monthly-forever true
/// }
/// ```
#[derive(Debug, Clone, Decode, Serialize)]
pub struct BackupSection {
    /// How often the scheduler wakes up and checks for new messages to snapshot.
    ///
    /// Accepts duration strings: `"1h"`, `"30m"`, `"3600s"`.
    ///
    /// KDL property: `snapshot-interval`
    #[knus(property, default = "1h".to_string())]
    pub snapshot_interval: String,

    /// Keep this many recent snapshots unconditionally.
    ///
    /// KDL child: `keep-recent 24`
    #[knus(child, unwrap(argument), default = 24usize)]
    pub keep_recent: usize,

    /// Keep one snapshot per hour for this many days back.
    ///
    /// KDL child: `hourly-days 1`
    #[knus(child, unwrap(argument), default = 1u32)]
    pub hourly_days: u32,

    /// Keep one snapshot per day for this many months back (1 month ≈ 30 days).
    ///
    /// KDL child: `daily-months 1`
    #[knus(child, unwrap(argument), default = 1u32)]
    pub daily_months: u32,

    /// Keep one snapshot per calendar month indefinitely.
    ///
    /// KDL child: `monthly-forever #true`
    #[knus(child, unwrap(argument), default = true)]
    pub monthly_forever: bool,
}

impl Default for BackupSection {
    fn default() -> Self {
        Self {
            snapshot_interval: "1h".to_string(),
            keep_recent: 24,
            hourly_days: 1,
            daily_months: 1,
            monthly_forever: true,
        }
    }
}

impl BackupSection {
    /// Parse the `snapshot_interval` string into a [`std::time::Duration`].
    ///
    /// Accepted formats: `"Xh"` (hours), `"Xm"` (minutes), `"Xs"` (seconds).
    /// Returns an error string if the format is not recognised.
    ///
    /// # Examples
    ///
    /// ```
    /// # use pattern_memory::config::BackupSection;
    /// let s = BackupSection::default();
    /// assert_eq!(s.parse_interval().unwrap().as_secs(), 3600);
    /// ```
    pub fn parse_interval(&self) -> Result<std::time::Duration, String> {
        parse_duration_str(&self.snapshot_interval)
    }
}

// ---------------------------------------------------------------------------
// Duration string parsing
// ---------------------------------------------------------------------------

/// Parse a simple duration string into a [`std::time::Duration`].
///
/// Accepted formats: `"Xh"` (hours), `"Xm"` (minutes), `"Xs"` (seconds)
/// where `X` is a positive integer. Whitespace is not accepted.
///
/// This is intentionally minimal — it handles the values users will
/// realistically enter in `.pattern.kdl`. For complex duration formats, callers
/// should wrap `parse_duration_str` in a higher-level validator.
///
/// # Errors
///
/// Returns a human-readable error string if the format is not recognised.
pub fn parse_duration_str(s: &str) -> Result<std::time::Duration, String> {
    if s.is_empty() {
        return Err("duration string must not be empty".to_string());
    }

    let (digits, unit) = if let Some(rest) = s.strip_suffix('h') {
        (rest, 'h')
    } else if let Some(rest) = s.strip_suffix('m') {
        (rest, 'm')
    } else if let Some(rest) = s.strip_suffix('s') {
        (rest, 's')
    } else {
        return Err(format!(
            "unrecognised duration format {s:?}; expected a positive integer followed by \
             'h' (hours), 'm' (minutes), or 's' (seconds), e.g. \"1h\", \"30m\", \"3600s\""
        ));
    };

    let n: u64 = digits.parse().map_err(|_| {
        format!("invalid duration {s:?}: {digits:?} is not a valid positive integer")
    })?;

    if n == 0 {
        return Err(format!(
            "invalid duration {s:?}: value must be greater than zero"
        ));
    }

    let secs = match unit {
        'h' => n * 3600,
        'm' => n * 60,
        's' => n,
        _ => unreachable!(),
    };

    Ok(std::time::Duration::from_secs(secs))
}

// ---------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// file-policy block
// ---------------------------------------------------------------------------

/// Rule direction for a single `file-policy` entry.
///
/// Used in [`FilePolicySection`] to carry allow/deny semantics through the
/// KDL decode layer without introducing a dependency on `pattern_runtime`.
/// `pattern_runtime::file_manager::policy::RuleMode` converts `From<FilePolicyMode>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum FilePolicyMode {
    /// The matched path is allowed.
    Allow,
    /// The matched path is denied.
    Deny,
}

/// Parsed `file-policy { allow "..."; deny "..." }` block.
///
/// Holds rules in **declaration order** — order is semantically significant
/// because evaluation is last-match-wins (see `FilePolicy::check_access`).
///
/// `knus`'s standard `#[knus(children(name = "...")]` attribute would split
/// `allow` and `deny` nodes into separate buckets, destroying their interleaved
/// order. This type therefore implements [`knus::traits::DecodeChildren`]
/// by hand, iterating over child nodes exactly once in document order.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FilePolicySection {
    /// Ordered list of `(mode, glob_pattern)` rules.
    pub rules: Vec<(FilePolicyMode, String)>,
}

/// Hand-rolled `DecodeChildren` so `knus::parse::<FilePolicySection>` works
/// for the test path (children provided as a flat document). This is the
/// same impl used when knus processes the `file-policy { … }` node's children
/// via `#[knus(child)]` on `MountConfig.file_policy`.
impl<S: ErrorSpan> DecodeChildren<S> for FilePolicySection {
    fn decode_children(
        nodes: &[SpannedNode<S>],
        ctx: &mut Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        let mut rules = Vec::with_capacity(nodes.len());

        for node in nodes {
            let name = node.node_name.as_ref();
            let mode = match name {
                "allow" => FilePolicyMode::Allow,
                "deny" => FilePolicyMode::Deny,
                _ => {
                    ctx.emit_error(DecodeError::unexpected(
                        &node.node_name,
                        "node",
                        format!("expected `allow` or `deny` in file-policy, found `{name}`"),
                    ));
                    continue;
                }
            };

            // Each rule has exactly one positional argument: the glob pattern.
            let pattern = match node.arguments.first() {
                Some(arg) => match &*arg.literal {
                    Literal::String(s) => s.as_ref().to_owned(),
                    _ => {
                        ctx.emit_error(DecodeError::unexpected(
                            &arg.literal,
                            "literal",
                            "file-policy rule argument must be a string glob pattern",
                        ));
                        continue;
                    }
                },
                None => {
                    ctx.emit_error(DecodeError::unexpected(
                        &node.node_name,
                        "node",
                        format!("`{name}` rule requires a glob pattern argument"),
                    ));
                    continue;
                }
            };

            rules.push((mode, pattern));
        }

        Ok(Self { rules })
    }
}

/// `knus::Decode` wrapping for use as a `#[knus(child)]` field on `MountConfig`.
///
/// When knus processes `file-policy { allow "..."; deny "..." }` as a child
/// node, it calls `Decode::decode_node`. We extract the node's children and
/// delegate to `DecodeChildren::decode_children` so the two decode paths
/// share the same logic.
impl<S: ErrorSpan> knus::traits::Decode<S> for FilePolicySection {
    fn decode_node(node: &SpannedNode<S>, ctx: &mut Context<S>) -> Result<Self, DecodeError<S>> {
        let children: &[SpannedNode<S>] =
            node.children.as_ref().map(|c| c.as_slice()).unwrap_or(&[]);
        FilePolicySection::decode_children(children, ctx)
    }
}

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
/// - Standalone mode requires `jj enabled=true` (Pattern owns VCS history).
/// - Sidecar mode requires `jj enabled=true` (sidecar jj must be active).
/// - `isolate-from-persona policy` must be one of `"none"`, `"core-only"`,
///   or `"full"`.
/// - `backup.snapshot-interval`, when present, must be a recognised duration
///   string (e.g. `"1h"`, `"30m"`, `"3600s"`). Validating at parse time
///   surfaces bad config immediately rather than silently falling back to a
///   1-hour default at attach time.
///
/// Path-level constraints (e.g. InRepo mode requiring a hashable project root)
/// are deferred to attach time, since parse time does not know the project
/// root path.
fn validate_config(config: &MountConfig, path: &Path) -> Result<(), ConfigError> {
    match config.mount.mode {
        ModeKind::Standalone | ModeKind::Sidecar if !config.jj.enabled => {
            return Err(ConfigError::Validation {
                path: path.to_owned(),
                reason: format!(
                    "mode `{}` requires `jj enabled=true` but `jj.enabled` is false",
                    match config.mount.mode {
                        ModeKind::Standalone => "standalone",
                        ModeKind::Sidecar => "sidecar",
                        ModeKind::InRepo => unreachable!(),
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

    // Validate backup.snapshot-interval at config-load time so the error is
    // surfaced immediately with a clear diagnostic rather than silently
    // falling back to the 1h default in attach().
    if let Some(backup) = &config.backup
        && let Err(e) = parse_duration_str(&backup.snapshot_interval)
    {
        return Err(ConfigError::Validation {
            path: path.to_owned(),
            reason: format!("backup.snapshot-interval is invalid: {e}"),
        });
    }

    Ok(())
}
