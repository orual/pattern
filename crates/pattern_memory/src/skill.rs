// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Skill provenance → trust tier assignment.
//!
//! Skills arrive from several sources (first-party resource dir, per-mount
//! skills directory, runtime agent drafts, etc.). Their declared
//! `trust_tier` in YAML frontmatter is **not** authoritative — authors
//! cannot self-promote their own skill to `FirstParty` or `ProjectLocal`
//! just by writing those strings. [`assign_trust_tier`] enforces this
//! policy.
//!
//! # Policy
//!
//! Only [`SkillTrustTier::PluginInstalled`] is preserved from the
//! frontmatter. All other declared tiers are overridden by the
//! source-derived tier. Rationale: project-local / first-party assertions
//! shouldn't be forgeable by authors. `PluginInstalled` is preserved
//! only because the plugin system (Plan 4) will validate its provenance
//! through a separate mechanism; until that lands, a skill that declares
//! `plugin-installed` emits a warning metric so the condition is
//! observable in production.
//!
//! # Source resolution
//!
//! [`resolve_source_for_path`] classifies a `.md` file by absolute path
//! against a caller-supplied first-party root and a list of known mount
//! roots. The first-party root is not baked in here: `pattern_runtime`
//! owns the `resources/skills/` directory and exposes it as a
//! `FIRST_PARTY_SKILL_DIR` const, which callers pass in.

use std::path::{Path, PathBuf};

use pattern_core::types::memory_types::SkillTrustTier;

// region: types

/// Where a skill was discovered — determines its source-derived trust tier.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    /// Shipped with pattern_runtime in its `resources/skills/` directory.
    SdkResourceDir,
    /// Found under a mount's `skills/` subdirectory.
    MountSkillsDir {
        /// Absolute path of the mount root (not the `skills/` subdir).
        mount: PathBuf,
    },
    /// Stored as a Skill block in project scope (no backing file).
    ProjectBlock,
    /// Created at runtime by an agent via `MemoryStore::put_block`.
    Runtime,
}

/// Provenance data for a single skill: where it came from plus whatever
/// tier the frontmatter declared (which is mostly advisory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillProvenance {
    /// Actual source of the skill, derived from disk/DB location.
    pub source: SkillSource,
    /// `trust_tier` field from the YAML frontmatter, if any.
    pub declared_tier: Option<SkillTrustTier>,
}

// endregion: types

// region: assign_trust_tier

/// Assign the effective [`SkillTrustTier`] for a loaded skill.
///
/// Policy:
/// - If the frontmatter declared `PluginInstalled`, it is preserved. A
///   warning metric `skill.plugin_installed_tier_without_plugin_system`
///   is incremented and a `tracing::warn!` is emitted, because the
///   plugin system is not yet active.
/// - All other declared tiers are ignored; the tier is derived from
///   [`SkillProvenance::source`].
pub fn assign_trust_tier(prov: &SkillProvenance) -> SkillTrustTier {
    if prov.declared_tier == Some(SkillTrustTier::PluginInstalled) {
        metrics::counter!("skill.plugin_installed_tier_without_plugin_system").increment(1);
        tracing::warn!(
            "skill declares trust_tier=plugin-installed but plugin system is not active yet"
        );
        return SkillTrustTier::PluginInstalled;
    }
    match &prov.source {
        SkillSource::SdkResourceDir => SkillTrustTier::FirstParty,
        SkillSource::MountSkillsDir { .. } | SkillSource::ProjectBlock => {
            SkillTrustTier::ProjectLocal
        }
        SkillSource::Runtime => SkillTrustTier::AdHoc,
    }
}

// endregion: assign_trust_tier

// region: resolve_source_for_path

/// Classify a skill `.md` file by absolute path.
///
/// Resolution order:
/// 1. If `first_party_dir` is `Some` and `path` is under it →
///    [`SkillSource::SdkResourceDir`].
/// 2. If `path` is under `<mount>/skills/` for any `mount` in
///    `known_mounts` → [`SkillSource::MountSkillsDir`] with that mount.
/// 3. Otherwise → [`SkillSource::Runtime`]. Callers that know the skill
///    originated from a project block (no file at all) should construct
///    [`SkillSource::ProjectBlock`] directly.
pub fn resolve_source_for_path(
    path: &Path,
    first_party_dir: Option<&Path>,
    known_mounts: &[&Path],
) -> SkillSource {
    if let Some(fp) = first_party_dir
        && path.starts_with(fp)
    {
        return SkillSource::SdkResourceDir;
    }
    for mount in known_mounts {
        let skills_dir = mount.join("skills");
        if path.starts_with(&skills_dir) {
            return SkillSource::MountSkillsDir {
                mount: mount.to_path_buf(),
            };
        }
    }
    SkillSource::Runtime
}

// endregion: resolve_source_for_path

#[cfg(test)]
mod tests {
    use super::*;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use std::path::PathBuf;

    fn prov(source: SkillSource, declared: Option<SkillTrustTier>) -> SkillProvenance {
        SkillProvenance {
            source,
            declared_tier: declared,
        }
    }

    // region: source → tier

    #[test]
    fn sdk_resource_dir_is_first_party() {
        // AC7.1
        assert_eq!(
            assign_trust_tier(&prov(SkillSource::SdkResourceDir, None)),
            SkillTrustTier::FirstParty
        );
    }

    #[test]
    fn mount_skills_dir_is_project_local() {
        // AC7.2
        assert_eq!(
            assign_trust_tier(&prov(
                SkillSource::MountSkillsDir {
                    mount: PathBuf::from("/mnt/x")
                },
                None
            )),
            SkillTrustTier::ProjectLocal
        );
    }

    #[test]
    fn project_block_is_project_local() {
        assert_eq!(
            assign_trust_tier(&prov(SkillSource::ProjectBlock, None)),
            SkillTrustTier::ProjectLocal
        );
    }

    #[test]
    fn runtime_is_ad_hoc() {
        // AC7.3
        assert_eq!(
            assign_trust_tier(&prov(SkillSource::Runtime, None)),
            SkillTrustTier::AdHoc
        );
    }

    // endregion: source → tier

    // region: declared-tier policy

    #[test]
    fn declared_ad_hoc_with_sdk_source_still_first_party() {
        // Source wins for all non-PluginInstalled declarations: authors
        // cannot self-demote a first-party skill either, nor self-promote.
        assert_eq!(
            assign_trust_tier(&prov(
                SkillSource::SdkResourceDir,
                Some(SkillTrustTier::AdHoc)
            )),
            SkillTrustTier::FirstParty
        );
    }

    #[test]
    fn declared_project_local_with_runtime_source_is_ad_hoc() {
        // Can't forge ProjectLocal from a Runtime source.
        assert_eq!(
            assign_trust_tier(&prov(
                SkillSource::Runtime,
                Some(SkillTrustTier::ProjectLocal)
            )),
            SkillTrustTier::AdHoc
        );
    }

    // endregion: declared-tier policy

    // region: plugin-installed preservation + metric

    #[test]
    fn declared_plugin_installed_preserved_and_emits_metric() {
        // AC7.4: PluginInstalled declaration is preserved regardless of
        // source AND increments the observability counter.
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();

        let tier = metrics::with_local_recorder(&recorder, || {
            assign_trust_tier(&prov(
                SkillSource::MountSkillsDir {
                    mount: PathBuf::from("/mnt/x"),
                },
                Some(SkillTrustTier::PluginInstalled),
            ))
        });

        assert_eq!(tier, SkillTrustTier::PluginInstalled);

        let snapshot = snapshotter.snapshot().into_vec();
        let entry = snapshot
            .iter()
            .find(|(ck, _, _, _)| {
                ck.key().name() == "skill.plugin_installed_tier_without_plugin_system"
            })
            .unwrap_or_else(|| {
                panic!("expected plugin-installed warning counter; got {snapshot:?}")
            });
        let (_, _, _, value) = entry;
        assert_eq!(*value, DebugValue::Counter(1));
    }

    #[test]
    fn plugin_installed_preserved_even_for_sdk_source() {
        // The PluginInstalled exception applies regardless of source.
        let recorder = DebuggingRecorder::new();
        let tier = metrics::with_local_recorder(&recorder, || {
            assign_trust_tier(&prov(
                SkillSource::SdkResourceDir,
                Some(SkillTrustTier::PluginInstalled),
            ))
        });
        assert_eq!(tier, SkillTrustTier::PluginInstalled);
    }

    // endregion: plugin-installed preservation + metric

    // region: resolve_source_for_path

    #[test]
    fn resolve_first_party_match() {
        let fp = PathBuf::from("/opt/runtime/resources/skills");
        let path = fp.join("example.md");
        let src = resolve_source_for_path(&path, Some(&fp), &[]);
        assert_eq!(src, SkillSource::SdkResourceDir);
    }

    #[test]
    fn resolve_mount_match() {
        let mount = PathBuf::from("/mnt/a");
        let path = mount.join("skills").join("foo.md");
        let src = resolve_source_for_path(&path, None, &[mount.as_path()]);
        assert_eq!(src, SkillSource::MountSkillsDir { mount });
    }

    #[test]
    fn resolve_first_party_beats_mount_when_both_match() {
        // First-party root takes precedence even if mount also contains it.
        let fp = PathBuf::from("/a/fp/skills");
        let mount = PathBuf::from("/a");
        let path = fp.join("s.md");
        let src = resolve_source_for_path(&path, Some(&fp), &[mount.as_path()]);
        assert_eq!(src, SkillSource::SdkResourceDir);
    }

    #[test]
    fn resolve_unknown_falls_back_to_runtime() {
        let path = PathBuf::from("/tmp/wat.md");
        let src = resolve_source_for_path(&path, None, &[]);
        assert_eq!(src, SkillSource::Runtime);
    }

    // endregion: resolve_source_for_path
}
