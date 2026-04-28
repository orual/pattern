//! Capability types for v3 multi-agent permission control.
//!
//! `CapabilitySet` describes what an agent (or constellation) is allowed
//! to do at compile time (effect-row visibility) and at runtime (per-effect
//! policy gates). Pure data types — no execution machinery — so this
//! module respects `pattern_core`'s trait/data-only spirit.
//!
//! Concrete enforcement lives in `pattern_runtime`:
//! - Compile-time visibility: `pattern_runtime::sdk::bundle::filtered_effect_decls`
//!   strips `EffectDecl`s whose category is absent from the active set
//!   before the prelude is concatenated.
//! - Runtime gating: `pattern_runtime::policy::PolicySet` evaluates
//!   `PolicyRule`s before each handler dispatch, escalating to the
//!   `PermissionBroker` when human approval is required.

pub mod policy;

pub use policy::{PolicyAction, PolicyContext, PolicyMatcher, PolicyRule, PolicySet, Precedence};

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// Semantic classification of an effect constructor's role from the agent's POV.
///
/// This axis is parallel to [`EffectCategory`] (which is per-module).
/// Together they form a two-axis gate: the prelude filter intersects the
/// agent's `categories` with the canonical effect row, and the agent's
/// `allowed_classes` with each surviving constructor's class.
///
/// See `pattern_runtime::sdk::effect_classes::ALL_CLASSES` for the canonical
/// table mapping every SDK constructor to its class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EffectClass {
    /// Pure observation — agent reads state. Includes one-way pipelines
    /// (CRDT sync, watchers) and emits to operator-controlled communication
    /// sinks (Display, Log, Message-via-router).
    Observe,
    /// Agent mutates its own session-local state (memory blocks, task graph,
    /// session timing, file-manager handle state, archival inserts).
    MutateInternal,
    /// Agent writes to filesystem within mount, visible to other tools/agents.
    MutateExternal,
    /// Agent affects other agents or constellation-level state (messaging,
    /// spawn, fronting, registry mutations, wake registrations).
    Coordinate,
    /// Side effects that leave the runtime sandbox (shell, MCP, network ports,
    /// LLM provider calls).
    Escape,
}

impl EffectClass {
    /// Every variant of `EffectClass`, in canonical order.
    pub const ALL: &'static [Self] = &[
        Self::Observe,
        Self::MutateInternal,
        Self::MutateExternal,
        Self::Coordinate,
        Self::Escape,
    ];
}

/// Whether the EffectClass axis acts as a runtime gate at handler dispatch.
///
/// `Enforce` — handler MUST verify the constructor's class is in the agent's
/// `allowed_classes` before dispatch.
///
/// `Skip` — handler delegates to the existing fine-grained system (router,
/// broker, registry, capability flags) which is authoritative. The class is
/// recorded for compile-time prelude visibility only; runtime enforcement
/// stays with the existing system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuntimeClassCheck {
    Enforce,
    Skip,
}

/// A category of agent-callable effect.
///
/// Variants align with `pattern_runtime::sdk::bundle::CANONICAL_EFFECT_ROW`;
/// `pattern_runtime` carries a cross-check test. `Wake` is forward-reserved
/// for the Phase 4 wake-condition effect (not yet wired into the canonical
/// row) so later phases can flip it on without schema churn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EffectCategory {
    Memory,
    Search,
    Recall,
    Tasks,
    Skills,
    Message,
    Display,
    Time,
    Log,
    Shell,
    File,
    /// Unified external-service port. Gate for per-port allowlisting via
    /// `CapabilitySet::has_port`.
    Port,
    Mcp,
    Spawn,
    Diagnostics,
    /// The wake-condition effect (`Pattern.Wake`) — registered/unregistered
    /// conditions deliver activations to the agent's mailbox. v3-multi-agent
    /// Phase 4.
    Wake,
    /// The fronting effect (`Pattern.Fronting`) — read and mutate the
    /// constellation's active fronting set and routing rules. v3-multi-agent
    /// Phase 5.
    Fronting,
    /// The constellation registry effect (`Pattern.Constellation`) — read
    /// persona records, find by relationship/project, list groups.
    /// v3-multi-agent Phase 6.
    Constellation,
}

impl EffectCategory {
    /// Every variant of `EffectCategory`, in canonical order.
    ///
    /// The match in [`Self::type_name`] is exhaustive over the same set —
    /// adding a new variant without updating both produces a compile error.
    pub const ALL: &'static [Self] = &[
        Self::Memory,
        Self::Search,
        Self::Recall,
        Self::Tasks,
        Self::Skills,
        Self::Message,
        Self::Display,
        Self::Time,
        Self::Log,
        Self::Shell,
        Self::File,
        Self::Port,
        Self::Mcp,
        Self::Spawn,
        Self::Diagnostics,
        Self::Wake,
        Self::Fronting,
        Self::Constellation,
    ];

    /// Canonical type name string. Matches `EffectDecl::type_name`
    /// emitted by `pattern_runtime`'s SDK handlers.
    pub fn type_name(self) -> &'static str {
        match self {
            Self::Memory => "Memory",
            Self::Search => "Search",
            Self::Recall => "Recall",
            Self::Tasks => "Tasks",
            Self::Skills => "Skills",
            Self::Message => "Message",
            Self::Display => "Display",
            Self::Time => "Time",
            Self::Log => "Log",
            Self::Shell => "Shell",
            Self::File => "File",
            Self::Port => "Port",
            Self::Mcp => "Mcp",
            Self::Spawn => "Spawn",
            Self::Diagnostics => "Diagnostics",
            Self::Wake => "Wake",
            Self::Fronting => "Fronting",
            Self::Constellation => "Constellation",
        }
    }

    /// Resolve a type name (ASCII case-insensitive) to its category.
    /// Returns `None` for names that don't correspond to any known effect.
    pub fn from_type_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|cat| cat.type_name().eq_ignore_ascii_case(name))
    }
}

impl std::str::FromStr for EffectCategory {
    type Err = CapabilityParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_type_name(s).ok_or_else(|| CapabilityParseError::UnknownEffect(s.to_owned()))
    }
}

/// Orthogonal capability flags. Each flag gates a runtime behaviour that
/// is not mappable to a single effect category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CapabilityFlag {
    /// Permits spawning a persona with a fresh identity (Phase 2 / Phase 3).
    SpawnNewIdentities,
    /// Permits registering custom Haskell wake conditions (Phase 4).
    WakeConditionRegistration,
    /// Permits mutating the `FrontingSet` or routing rules (Phase 5).
    FrontingControl,
}

impl CapabilityFlag {
    pub const ALL: &'static [Self] = &[
        Self::SpawnNewIdentities,
        Self::WakeConditionRegistration,
        Self::FrontingControl,
    ];

    /// Kebab-case name used in KDL config and serialized form.
    pub fn name(self) -> &'static str {
        match self {
            Self::SpawnNewIdentities => "spawn-new-identities",
            Self::WakeConditionRegistration => "wake-condition-registration",
            Self::FrontingControl => "fronting-control",
        }
    }

    /// Resolve a kebab-case name (ASCII case-insensitive) to its flag.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|f| f.name().eq_ignore_ascii_case(name))
    }
}

impl std::str::FromStr for CapabilityFlag {
    type Err = CapabilityParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_name(s).ok_or_else(|| CapabilityParseError::UnknownFlag(s.to_owned()))
    }
}

/// The set of effect categories, capability flags, and optional per-category
/// resource allowlists that describe what an agent is permitted to do.
///
/// `categories` controls which `EffectDecl`s land in the agent's generated
/// Haskell prelude (compile-time visibility). `flags` gate orthogonal
/// behaviours that don't map to a single effect category. `resources` provides
/// fine-grained allowlisting within a category when category-level grants are
/// too broad.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    pub categories: BTreeSet<EffectCategory>,
    pub flags: BTreeSet<CapabilityFlag>,
    /// Optional per-category allowlist of resource identifiers. When a
    /// category has an entry here with a non-empty set, only those resource
    /// IDs are permitted within that category. When the category is absent
    /// from the map (or maps to an empty set), the category-level grant via
    /// `categories` is unrestricted within that category.
    ///
    /// Used by the `Port` effect category for per-port granularity:
    /// an agent with `categories.contains(Port)` can use any port unless
    /// `resources[Port]` is non-empty, in which case only the listed port
    /// IDs are accessible. The same shape can carry Shell command allowlists,
    /// File path-prefix allowlists, etc. when those phases need it.
    resources: BTreeMap<EffectCategory, BTreeSet<SmolStr>>,
    /// Effect classes this capability set permits at compile-time and runtime.
    ///
    /// If empty, defaults to ALL classes (preserves backwards-compatible
    /// behaviour for existing capability sets that pre-date this axis).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub allowed_classes: BTreeSet<EffectClass>,
}

impl CapabilitySet {
    /// An empty capability set: no effects, no flags. The prelude still
    /// emits base types, so pure-computation agent programs compile and
    /// run; any effect call fails at Tidepool compile.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Full power: every effect category + every flag. Used for back-compat
    /// with sessions that predate capability scoping. `resources` is left
    /// empty — no per-resource restrictions anywhere.
    pub fn all() -> Self {
        Self {
            categories: EffectCategory::ALL.iter().copied().collect(),
            flags: CapabilityFlag::ALL.iter().copied().collect(),
            resources: BTreeMap::new(),
            allowed_classes: BTreeSet::new(),
        }
    }

    /// Returns the effective set of allowed classes. If `allowed_classes`
    /// is empty, returns all classes (backwards-compatible default).
    pub fn effective_allowed_classes(&self) -> BTreeSet<EffectClass> {
        if self.allowed_classes.is_empty() {
            EffectClass::ALL.iter().copied().collect()
        } else {
            self.allowed_classes.clone()
        }
    }

    /// Builder: restrict to specific effect classes.
    #[must_use]
    pub fn with_classes(mut self, classes: impl IntoIterator<Item = EffectClass>) -> Self {
        self.allowed_classes = classes.into_iter().collect();
        self
    }

    /// Builder-style: replace the flag set.
    #[must_use]
    pub fn with_flags<I: IntoIterator<Item = CapabilityFlag>>(mut self, iter: I) -> Self {
        self.flags = iter.into_iter().collect();
        self
    }

    pub fn contains(&self, cat: EffectCategory) -> bool {
        self.categories.contains(&cat)
    }

    pub fn has_flag(&self, flag: CapabilityFlag) -> bool {
        self.flags.contains(&flag)
    }

    pub fn iter_categories(&self) -> impl Iterator<Item = EffectCategory> + '_ {
        self.categories.iter().copied()
    }

    pub fn iter_flags(&self) -> impl Iterator<Item = CapabilityFlag> + '_ {
        self.flags.iter().copied()
    }

    /// Granular access check: returns true iff the category is allowed AND
    /// (the resource allowlist for that category is absent/empty, OR the list
    /// contains `resource_id`). Use this for per-resource gating; use
    /// `contains(category)` for category-level checks where no resource
    /// granularity is meaningful.
    pub fn has_resource(&self, category: EffectCategory, resource_id: &str) -> bool {
        if !self.categories.contains(&category) {
            return false;
        }
        match self.resources.get(&category) {
            // No allowlist entry → unrestricted within the category.
            None => true,
            // Empty set treated as unrestricted (erased by `with_resources`).
            Some(set) if set.is_empty() => true,
            Some(set) => set.contains(resource_id),
        }
    }

    /// Builder-style: set the resource allowlist for a category. Replaces any
    /// existing entry. Passing an empty iterator erases the entry, returning
    /// the category to unrestricted status.
    #[must_use]
    pub fn with_resources<I: IntoIterator<Item = SmolStr>>(
        mut self,
        category: EffectCategory,
        ids: I,
    ) -> Self {
        let set: BTreeSet<SmolStr> = ids.into_iter().collect();
        if set.is_empty() {
            self.resources.remove(&category);
        } else {
            self.resources.insert(category, set);
        }
        self
    }

    /// Iterate the resource allowlist for a category. Yields nothing when the
    /// category is unrestricted (no entry, or empty entry which `with_resources`
    /// erases on insert).
    pub fn iter_resources(&self, category: EffectCategory) -> impl Iterator<Item = &SmolStr> {
        self.resources
            .get(&category)
            .into_iter()
            .flat_map(|s| s.iter())
    }

    /// Convenience: returns true iff `File` is in the category set.
    pub fn has_file(&self) -> bool {
        self.categories.contains(&EffectCategory::File)
    }

    /// Convenience: returns true iff `Shell` is in the category set.
    pub fn has_shell(&self) -> bool {
        self.categories.contains(&EffectCategory::Shell)
    }

    /// Per-port granular check. Returns true iff the `Port` effect category
    /// is present and (if the resource allowlist is non-empty) `port_id` is
    /// in the allowlist.
    pub fn has_port(&self, port_id: &str) -> bool {
        self.has_resource(EffectCategory::Port, port_id)
    }

    /// Non-strict subset: every category, flag, and resource allowlist in
    /// `self` is permitted by `other`.
    ///
    /// Resource subset semantics: if `other` has a non-empty allowlist for a
    /// category, `self` must also have a non-empty allowlist that is a subset
    /// of it. A `self` that is unrestricted (no entry) within a category where
    /// `other` is restricted escalates beyond `other` — this returns false.
    pub fn is_subset_of(&self, other: &Self) -> bool {
        if !self.categories.is_subset(&other.categories) {
            return false;
        }
        if !self.flags.is_subset(&other.flags) {
            return false;
        }
        // Check class constraints: if other has a non-empty allowed_classes,
        // self must also have a non-empty subset.
        if !other.allowed_classes.is_empty() {
            if self.allowed_classes.is_empty() {
                // Self is unrestricted while other is restricted — escalation.
                return false;
            }
            if !self.allowed_classes.is_subset(&other.allowed_classes) {
                return false;
            }
        }
        // Check resource constraints for every category self participates in.
        for cat in &self.categories {
            let other_entry = other.resources.get(cat);
            // Other unrestricted (absent or empty entry) → no constraint on self.
            let other_set = match other_entry {
                None => continue,
                Some(s) if s.is_empty() => continue,
                Some(s) => s,
            };
            // Other has a non-empty allowlist — self must have a non-empty subset.
            let self_entry = self.resources.get(cat);
            match self_entry {
                // Self is unrestricted while other is restricted — escalation.
                None => return false,
                Some(s) if s.is_empty() => return false,
                Some(self_set) => {
                    if !self_set.is_subset(other_set) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Verify `self` is a subset of `parent`; otherwise surface every
    /// category, flag, and resource that would represent escalation.
    ///
    /// Used by spawn paths (ephemeral / fork) to enforce that children cannot
    /// acquire capabilities the parent lacks.
    pub fn restrict_to(self, parent: &Self) -> Result<Self, CapabilityError> {
        let added_categories: Vec<_> = self
            .categories
            .difference(&parent.categories)
            .copied()
            .collect();
        let added_flags: Vec<_> = self.flags.difference(&parent.flags).copied().collect();

        // Compute per-category resource escalations.
        let mut added_resources: BTreeMap<EffectCategory, Vec<SmolStr>> = BTreeMap::new();
        let mut parent_resources_snapshot: BTreeMap<EffectCategory, Vec<SmolStr>> = BTreeMap::new();

        for cat in &self.categories {
            // Skip categories already flagged as escalated at the category level;
            // the category escalation is the primary signal in that case.
            if !parent.categories.contains(cat) {
                continue;
            }
            let self_entry = self.resources.get(cat);
            let parent_entry = parent.resources.get(cat);

            // Parent is unrestricted for this category — no resource escalation.
            let parent_set = match parent_entry {
                None => continue,
                Some(s) if s.is_empty() => continue,
                Some(s) => s,
            };

            match self_entry {
                // Self is unrestricted while parent is restricted — escalation.
                // Represent this as an empty Vec (signals "child was unrestricted",
                // distinct from "child had specific extra resources").
                None => {
                    added_resources.entry(*cat).or_default();
                    parent_resources_snapshot
                        .entry(*cat)
                        .or_insert_with(|| parent_set.iter().cloned().collect());
                }
                Some(self_set) if self_set.is_empty() => {
                    // Empty set was erased by `with_resources`, so this branch is
                    // unreachable in practice; guarded for belt-and-suspenders.
                    added_resources.entry(*cat).or_default();
                    parent_resources_snapshot
                        .entry(*cat)
                        .or_insert_with(|| parent_set.iter().cloned().collect());
                }
                Some(self_set) => {
                    // Collect resources in self but not in parent.
                    let extras: Vec<SmolStr> = self_set.difference(parent_set).cloned().collect();
                    if !extras.is_empty() {
                        added_resources.insert(*cat, extras);
                        parent_resources_snapshot
                            .entry(*cat)
                            .or_insert_with(|| parent_set.iter().cloned().collect());
                    }
                }
            }
        }

        // Compute class escalations.
        let added_classes: Vec<EffectClass> = if !parent.allowed_classes.is_empty() {
            if self.allowed_classes.is_empty() {
                // Self is unrestricted while parent is restricted.
                EffectClass::ALL
                    .iter()
                    .copied()
                    .filter(|c| !parent.allowed_classes.contains(c))
                    .collect()
            } else {
                self.allowed_classes
                    .difference(&parent.allowed_classes)
                    .copied()
                    .collect()
            }
        } else {
            vec![]
        };

        if !added_categories.is_empty()
            || !added_flags.is_empty()
            || !added_resources.is_empty()
            || !added_classes.is_empty()
        {
            return Err(CapabilityError::Escalation {
                added_categories,
                added_flags,
                added_classes,
                parent_categories: parent.categories.iter().copied().collect(),
                parent_flags: parent.flags.iter().copied().collect(),
                parent_classes: parent.allowed_classes.iter().copied().collect(),
                added_resources,
                parent_resources: parent_resources_snapshot,
            });
        }
        Ok(self)
    }
}

impl FromIterator<EffectCategory> for CapabilitySet {
    /// Build a set from effect categories; flags and resources default empty.
    /// Chain `with_flags` to add capability flags, or `with_resources` to add
    /// per-category resource allowlists.
    fn from_iter<I: IntoIterator<Item = EffectCategory>>(iter: I) -> Self {
        Self {
            categories: iter.into_iter().collect(),
            flags: BTreeSet::new(),
            resources: BTreeMap::new(),
            allowed_classes: BTreeSet::new(),
        }
    }
}

/// Errors raised by the capability layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CapabilityError {
    #[error(
        "capability escalation: cannot add categories {added_categories:?} or flags \
         {added_flags:?} or classes {added_classes:?} or resources {added_resources:?} \
         to a set restricted to categories {parent_categories:?} flags {parent_flags:?} \
         classes {parent_classes:?} resources {parent_resources:?}"
    )]
    Escalation {
        added_categories: Vec<EffectCategory>,
        added_flags: Vec<CapabilityFlag>,
        /// Effect classes the child claims that escalate beyond the parent's
        /// allowed_classes set.
        added_classes: Vec<EffectClass>,
        parent_categories: Vec<EffectCategory>,
        parent_flags: Vec<CapabilityFlag>,
        /// The parent's allowed_classes at the time of the escalation check.
        parent_classes: Vec<EffectClass>,
        /// Per-category resources the child claims that escalate beyond the parent's
        /// allowlist. An empty `Vec` for a category means the child is unrestricted
        /// while the parent has a non-empty allowlist (which is itself an escalation).
        added_resources: BTreeMap<EffectCategory, Vec<SmolStr>>,
        /// The parent's resource allowlist at the time of the escalation check, for
        /// diagnostic context.
        parent_resources: BTreeMap<EffectCategory, Vec<SmolStr>>,
    },

    #[error("capability denied: effect {category:?} not present in set")]
    Denied { category: EffectCategory },

    #[error("capability flag denied: {flag:?} not present in set")]
    FlagDenied { flag: CapabilityFlag },
}

/// Parse errors for `EffectCategory` / `CapabilityFlag` `FromStr`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CapabilityParseError {
    #[error("unknown effect category: {0:?}")]
    UnknownEffect(String),
    #[error("unknown capability flag: {0:?}")]
    UnknownFlag(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Exhaustive match over `EffectCategory`. Adding a new variant
    /// without updating this list fails to compile.
    fn enumerate_effect_categories() -> Vec<EffectCategory> {
        let mut out = Vec::new();
        for cat in [
            EffectCategory::Memory,
            EffectCategory::Search,
            EffectCategory::Recall,
            EffectCategory::Tasks,
            EffectCategory::Skills,
            EffectCategory::Message,
            EffectCategory::Display,
            EffectCategory::Time,
            EffectCategory::Log,
            EffectCategory::Shell,
            EffectCategory::File,
            EffectCategory::Port,
            EffectCategory::Mcp,
            EffectCategory::Spawn,
            EffectCategory::Diagnostics,
            EffectCategory::Wake,
            EffectCategory::Fronting,
            EffectCategory::Constellation,
        ] {
            // Force exhaustive coverage at compile time. If a new variant
            // is added, the match below stops compiling until it's listed.
            match cat {
                EffectCategory::Memory
                | EffectCategory::Search
                | EffectCategory::Recall
                | EffectCategory::Tasks
                | EffectCategory::Skills
                | EffectCategory::Message
                | EffectCategory::Display
                | EffectCategory::Time
                | EffectCategory::Log
                | EffectCategory::Shell
                | EffectCategory::File
                | EffectCategory::Port
                | EffectCategory::Mcp
                | EffectCategory::Spawn
                | EffectCategory::Diagnostics
                | EffectCategory::Wake
                | EffectCategory::Fronting
                | EffectCategory::Constellation => out.push(cat),
            }
        }
        out
    }

    fn enumerate_capability_flags() -> Vec<CapabilityFlag> {
        let mut out = Vec::new();
        for flag in [
            CapabilityFlag::SpawnNewIdentities,
            CapabilityFlag::WakeConditionRegistration,
            CapabilityFlag::FrontingControl,
        ] {
            match flag {
                CapabilityFlag::SpawnNewIdentities
                | CapabilityFlag::WakeConditionRegistration
                | CapabilityFlag::FrontingControl => out.push(flag),
            }
        }
        out
    }

    #[test]
    fn all_contains_every_effect_category_variant() {
        let expected = enumerate_effect_categories();
        let set = CapabilitySet::all();
        for cat in &expected {
            assert!(
                set.contains(*cat),
                "CapabilitySet::all() missing category {cat:?}"
            );
        }
        assert_eq!(
            set.categories.len(),
            expected.len(),
            "all() has unexpected number of categories"
        );
    }

    #[test]
    fn all_contains_every_capability_flag_variant() {
        let expected = enumerate_capability_flags();
        let set = CapabilitySet::all();
        for flag in &expected {
            assert!(
                set.has_flag(*flag),
                "CapabilitySet::all() missing flag {flag:?}"
            );
        }
        assert_eq!(
            set.flags.len(),
            expected.len(),
            "all() has unexpected number of flags"
        );
    }

    #[test]
    fn default_equals_empty() {
        assert_eq!(CapabilitySet::default(), CapabilitySet::empty());
        assert!(CapabilitySet::empty().categories.is_empty());
        assert!(CapabilitySet::empty().flags.is_empty());
    }

    #[test]
    fn has_flag_false_on_default_true_after_insert() {
        let mut set = CapabilitySet::empty();
        assert!(!set.has_flag(CapabilityFlag::SpawnNewIdentities));
        set.flags.insert(CapabilityFlag::SpawnNewIdentities);
        assert!(set.has_flag(CapabilityFlag::SpawnNewIdentities));
    }

    #[test]
    fn restrict_to_ok_when_subset() {
        let parent = CapabilitySet::all();
        let child = CapabilitySet::from_iter([EffectCategory::Memory, EffectCategory::Message])
            .with_flags([CapabilityFlag::SpawnNewIdentities]);
        let restricted = child
            .clone()
            .restrict_to(&parent)
            .expect("subset must succeed");
        assert_eq!(restricted, child);
    }

    #[test]
    fn restrict_to_err_when_adding_categories() {
        let parent = CapabilitySet::from_iter([EffectCategory::Memory]);
        let child = CapabilitySet::from_iter([EffectCategory::Memory, EffectCategory::Shell]);
        let err = child.restrict_to(&parent).unwrap_err();
        match err {
            CapabilityError::Escalation {
                added_categories,
                added_flags,
                ..
            } => {
                assert_eq!(added_categories, vec![EffectCategory::Shell]);
                assert!(added_flags.is_empty());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn restrict_to_err_when_adding_flags() {
        let parent = CapabilitySet::from_iter([EffectCategory::Memory]);
        let child = CapabilitySet::from_iter([EffectCategory::Memory])
            .with_flags([CapabilityFlag::SpawnNewIdentities]);
        let err = child.restrict_to(&parent).unwrap_err();
        match err {
            CapabilityError::Escalation { added_flags, .. } => {
                assert_eq!(added_flags, vec![CapabilityFlag::SpawnNewIdentities]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn restrict_to_err_lists_both_categories_and_flags() {
        let parent = CapabilitySet::empty();
        let child = CapabilitySet::from_iter([EffectCategory::Shell])
            .with_flags([CapabilityFlag::FrontingControl]);
        let err = child.restrict_to(&parent).unwrap_err();
        match err {
            CapabilityError::Escalation {
                added_categories,
                added_flags,
                ..
            } => {
                assert_eq!(added_categories, vec![EffectCategory::Shell]);
                assert_eq!(added_flags, vec![CapabilityFlag::FrontingControl]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn from_type_name_resolves_known_strings() {
        assert_eq!(
            EffectCategory::from_type_name("Memory"),
            Some(EffectCategory::Memory)
        );
        assert_eq!(
            EffectCategory::from_type_name("memory"),
            Some(EffectCategory::Memory)
        );
        assert_eq!(EffectCategory::from_type_name("nonsense"), None);
    }

    #[test]
    fn capability_flag_round_trips_kebab_case() {
        assert_eq!(
            CapabilityFlag::from_name("spawn-new-identities"),
            Some(CapabilityFlag::SpawnNewIdentities)
        );
        assert_eq!(
            CapabilityFlag::from_name("Spawn-New-Identities"),
            Some(CapabilityFlag::SpawnNewIdentities)
        );
        assert_eq!(CapabilityFlag::from_name("nonsense"), None);
    }

    fn arb_effect_category() -> impl Strategy<Value = EffectCategory> {
        prop::sample::select(EffectCategory::ALL.to_vec())
    }

    fn arb_capability_flag() -> impl Strategy<Value = CapabilityFlag> {
        prop::sample::select(CapabilityFlag::ALL.to_vec())
    }

    fn arb_capability_set() -> impl Strategy<Value = CapabilitySet> {
        (
            prop::collection::vec(arb_effect_category(), 0..EffectCategory::ALL.len()),
            prop::collection::vec(arb_capability_flag(), 0..CapabilityFlag::ALL.len()),
        )
            .prop_map(|(cats, flags)| CapabilitySet::from_iter(cats).with_flags(flags))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn capability_set_round_trips_through_serde_json(set in arb_capability_set()) {
            let encoded = serde_json::to_string(&set).expect("serialize");
            let decoded: CapabilitySet =
                serde_json::from_str(&encoded).expect("deserialize");
            prop_assert_eq!(set, decoded);
        }
    }

    // ─── per-resource granularity tests ───────────────────────────────────────

    #[test]
    fn has_resource_true_when_unrestricted_category_grant() {
        // Category present, no resources entry → unrestricted, any id passes.
        let set = CapabilitySet::from_iter([EffectCategory::Port]);
        assert!(set.has_resource(EffectCategory::Port, "any-port-id"));
        assert!(set.has_resource(EffectCategory::Port, ""));
    }

    #[test]
    fn has_resource_true_when_id_in_allowlist() {
        let set = CapabilitySet::from_iter([EffectCategory::Port]).with_resources(
            EffectCategory::Port,
            [SmolStr::from("a"), SmolStr::from("b")],
        );
        assert!(set.has_resource(EffectCategory::Port, "a"));
        assert!(set.has_resource(EffectCategory::Port, "b"));
        assert!(!set.has_resource(EffectCategory::Port, "c"));
    }

    #[test]
    fn has_resource_false_when_category_missing() {
        // No category grant at all → false regardless of resources map.
        let set = CapabilitySet::empty();
        assert!(!set.has_resource(EffectCategory::Port, "any-port-id"));
        // Also false even if resources are populated for a different category.
        let set2 = CapabilitySet::from_iter([EffectCategory::Memory])
            .with_resources(EffectCategory::Memory, [SmolStr::from("x")]);
        // Port is not in categories.
        assert!(!set2.has_resource(EffectCategory::Port, "x"));
    }

    #[test]
    fn has_resource_empty_set_unrestricted() {
        // with_resources(cat, []) should erase the entry → unrestricted.
        let set = CapabilitySet::from_iter([EffectCategory::Port])
            .with_resources(EffectCategory::Port, [SmolStr::from("a")])
            .with_resources(EffectCategory::Port, Vec::<SmolStr>::new());
        // Entry should be gone; any id permitted.
        assert!(set.has_resource(EffectCategory::Port, "a"));
        assert!(set.has_resource(EffectCategory::Port, "z"));
        // Verify via iter_resources that no entries remain.
        assert_eq!(set.iter_resources(EffectCategory::Port).count(), 0);
    }

    #[test]
    fn with_resources_replaces_existing_entry() {
        let set = CapabilitySet::from_iter([EffectCategory::Port])
            .with_resources(EffectCategory::Port, [SmolStr::from("a")])
            .with_resources(EffectCategory::Port, [SmolStr::from("b")]);
        // Only "b" should be present.
        assert!(!set.has_resource(EffectCategory::Port, "a"));
        assert!(set.has_resource(EffectCategory::Port, "b"));
        assert_eq!(set.iter_resources(EffectCategory::Port).count(), 1);
    }

    #[test]
    fn with_resources_empty_erases_entry() {
        let set = CapabilitySet::from_iter([EffectCategory::Port])
            .with_resources(EffectCategory::Port, [SmolStr::from("a")])
            .with_resources(EffectCategory::Port, Vec::<SmolStr>::new());
        assert_eq!(set.iter_resources(EffectCategory::Port).count(), 0);
        // Semantically unrestricted after erasure.
        assert!(set.has_resource(EffectCategory::Port, "anything"));
    }

    #[test]
    fn is_subset_of_resource_escalation_caught() {
        // Parent allows [a, b]; child claims [a, c] → not a subset.
        let parent = CapabilitySet::from_iter([EffectCategory::Port]).with_resources(
            EffectCategory::Port,
            [SmolStr::from("a"), SmolStr::from("b")],
        );
        let child = CapabilitySet::from_iter([EffectCategory::Port]).with_resources(
            EffectCategory::Port,
            [SmolStr::from("a"), SmolStr::from("c")],
        );
        assert!(!child.is_subset_of(&parent));
    }

    #[test]
    fn is_subset_of_child_unrestricted_escalation_caught() {
        // Parent has [a, b]; child is unrestricted (empty resources) → escalates,
        // not a subset.
        let parent = CapabilitySet::from_iter([EffectCategory::Port]).with_resources(
            EffectCategory::Port,
            [SmolStr::from("a"), SmolStr::from("b")],
        );
        let child = CapabilitySet::from_iter([EffectCategory::Port]);
        assert!(!child.is_subset_of(&parent));
    }

    #[test]
    fn is_subset_of_resource_ok_when_truly_subset() {
        // Parent [a, b, c]; child [a, b] → legitimate subset.
        let parent = CapabilitySet::from_iter([EffectCategory::Port]).with_resources(
            EffectCategory::Port,
            [SmolStr::from("a"), SmolStr::from("b"), SmolStr::from("c")],
        );
        let child = CapabilitySet::from_iter([EffectCategory::Port]).with_resources(
            EffectCategory::Port,
            [SmolStr::from("a"), SmolStr::from("b")],
        );
        assert!(child.is_subset_of(&parent));
    }

    #[test]
    fn is_subset_of_parent_unrestricted_child_restricted_ok() {
        // Parent unrestricted (no resources entry); child restricted → child is
        // a subset (narrower than parent).
        let parent = CapabilitySet::from_iter([EffectCategory::Port]);
        let child = CapabilitySet::from_iter([EffectCategory::Port])
            .with_resources(EffectCategory::Port, [SmolStr::from("a")]);
        assert!(child.is_subset_of(&parent));
    }

    #[test]
    fn restrict_to_returns_escalation_with_resource_diff() {
        // Parent [a, b]; child [a, c] → Escalation with added_resources populated.
        let parent = CapabilitySet::from_iter([EffectCategory::Port]).with_resources(
            EffectCategory::Port,
            [SmolStr::from("a"), SmolStr::from("b")],
        );
        let child = CapabilitySet::from_iter([EffectCategory::Port]).with_resources(
            EffectCategory::Port,
            [SmolStr::from("a"), SmolStr::from("c")],
        );
        let err = child.restrict_to(&parent).unwrap_err();
        match err {
            CapabilityError::Escalation {
                added_resources,
                parent_resources,
                added_categories,
                added_flags,
                ..
            } => {
                assert!(added_categories.is_empty());
                assert!(added_flags.is_empty());
                // "c" is the resource child claims but parent doesn't allow.
                assert!(
                    added_resources
                        .get(&EffectCategory::Port)
                        .map(|v| v.contains(&SmolStr::from("c")))
                        .unwrap_or(false),
                    "added_resources should contain 'c' for Port, got: {added_resources:?}"
                );
                // parent_resources should record parent's allowlist.
                assert!(
                    parent_resources
                        .get(&EffectCategory::Port)
                        .map(|v| v.contains(&SmolStr::from("a")) && v.contains(&SmolStr::from("b")))
                        .unwrap_or(false),
                    "parent_resources should contain ['a','b'] for Port, got: {parent_resources:?}"
                );
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn restrict_to_escalation_when_child_unrestricted_parent_restricted() {
        // Parent [a, b]; child unrestricted → escalates.
        let parent = CapabilitySet::from_iter([EffectCategory::Port]).with_resources(
            EffectCategory::Port,
            [SmolStr::from("a"), SmolStr::from("b")],
        );
        let child = CapabilitySet::from_iter([EffectCategory::Port]);
        let err = child.restrict_to(&parent).unwrap_err();
        match err {
            CapabilityError::Escalation {
                added_resources, ..
            } => {
                // added_resources[Port] should be non-empty to indicate escalation.
                assert!(
                    !added_resources.is_empty(),
                    "escalation map should be non-empty, got: {added_resources:?}"
                );
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn has_port_uses_port_category() {
        let set = CapabilitySet::from_iter([EffectCategory::Port]).with_resources(
            EffectCategory::Port,
            [SmolStr::from("github"), SmolStr::from("discord")],
        );
        assert!(set.has_port("github"));
        assert!(set.has_port("discord"));
        assert!(!set.has_port("slack"));
    }

    #[test]
    fn has_file_and_has_shell_convenience_methods() {
        let neither = CapabilitySet::empty();
        assert!(!neither.has_file());
        assert!(!neither.has_shell());

        let both = CapabilitySet::from_iter([EffectCategory::File, EffectCategory::Shell]);
        assert!(both.has_file());
        assert!(both.has_shell());

        let file_only = CapabilitySet::from_iter([EffectCategory::File]);
        assert!(file_only.has_file());
        assert!(!file_only.has_shell());
    }

    #[test]
    fn all_leaves_resources_empty_unrestricted() {
        // CapabilitySet::all() must leave resources empty — unrestricted everywhere.
        let set = CapabilitySet::all();
        for cat in EffectCategory::ALL {
            assert_eq!(
                set.iter_resources(*cat).count(),
                0,
                "all() should have no resource restrictions for {cat:?}"
            );
            // Every id must pass for every category in all().
            assert!(
                set.has_resource(*cat, "any-id"),
                "all() should be unrestricted for {cat:?}"
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn roundtrip_with_resources_has_resource(
            ids in prop::collection::vec("[a-z]{1,8}", 1..6),
            probe in "[a-z]{1,8}",
        ) {
            let smol_ids: Vec<SmolStr> = ids.iter().map(|s| SmolStr::from(s.as_str())).collect();
            let set = CapabilitySet::from_iter([EffectCategory::Port])
                .with_resources(EffectCategory::Port, smol_ids.clone());
            // Every id we inserted must be accessible.
            for id in &ids {
                assert!(set.has_resource(EffectCategory::Port, id.as_str()));
            }
            // Probe passes iff it's in the original set.
            let expected = ids.iter().any(|id| id.as_str() == probe.as_str());
            prop_assert_eq!(set.has_resource(EffectCategory::Port, probe.as_str()), expected);
        }
    }
}
