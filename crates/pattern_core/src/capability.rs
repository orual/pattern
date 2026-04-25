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

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

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
    Sources,
    Mcp,
    Rpc,
    Spawn,
    Diagnostics,
    /// Forward-reserved for the Phase 4 wake-condition effect.
    Wake,
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
        Self::Sources,
        Self::Mcp,
        Self::Rpc,
        Self::Spawn,
        Self::Diagnostics,
        Self::Wake,
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
            Self::Sources => "Sources",
            Self::Mcp => "Mcp",
            Self::Rpc => "Rpc",
            Self::Spawn => "Spawn",
            Self::Diagnostics => "Diagnostics",
            Self::Wake => "Wake",
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

/// The set of effect categories and capability flags an agent may use.
///
/// `categories` controls which `EffectDecl`s land in the agent's
/// generated Haskell prelude (compile-time visibility). `flags` gate
/// orthogonal behaviours that don't map to a single effect.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    pub categories: BTreeSet<EffectCategory>,
    pub flags: BTreeSet<CapabilityFlag>,
}

impl CapabilitySet {
    /// An empty capability set: no effects, no flags. The prelude still
    /// emits base types, so pure-computation agent programs compile and
    /// run; any effect call fails at Tidepool compile.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Full power: every effect category + every flag. Used for
    /// back-compat with sessions that predate capability scoping.
    pub fn all() -> Self {
        Self {
            categories: EffectCategory::ALL.iter().copied().collect(),
            flags: CapabilityFlag::ALL.iter().copied().collect(),
        }
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

    /// Non-strict subset: every category and flag in `self` is present
    /// in `other`.
    pub fn is_subset_of(&self, other: &Self) -> bool {
        self.categories.is_subset(&other.categories) && self.flags.is_subset(&other.flags)
    }

    /// Verify `self` is a subset of `parent`; otherwise surface every
    /// category and flag that would represent escalation.
    ///
    /// Used by spawn paths (ephemeral / fork) to enforce that children
    /// cannot acquire capabilities the parent lacks.
    pub fn restrict_to(self, parent: &Self) -> Result<Self, CapabilityError> {
        let added_categories: Vec<_> = self
            .categories
            .difference(&parent.categories)
            .copied()
            .collect();
        let added_flags: Vec<_> = self.flags.difference(&parent.flags).copied().collect();
        if !added_categories.is_empty() || !added_flags.is_empty() {
            return Err(CapabilityError::Escalation {
                added_categories,
                added_flags,
                parent_categories: parent.categories.iter().copied().collect(),
                parent_flags: parent.flags.iter().copied().collect(),
            });
        }
        Ok(self)
    }
}

impl FromIterator<EffectCategory> for CapabilitySet {
    /// Build a set from effect categories; flags default empty.
    /// Chain `with_flags` afterward to add capability flags.
    fn from_iter<I: IntoIterator<Item = EffectCategory>>(iter: I) -> Self {
        Self {
            categories: iter.into_iter().collect(),
            flags: BTreeSet::new(),
        }
    }
}

/// Errors raised by the capability layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CapabilityError {
    #[error(
        "capability escalation: cannot add categories {added_categories:?} or flags \
         {added_flags:?} to a set restricted to categories {parent_categories:?} flags \
         {parent_flags:?}"
    )]
    Escalation {
        added_categories: Vec<EffectCategory>,
        added_flags: Vec<CapabilityFlag>,
        parent_categories: Vec<EffectCategory>,
        parent_flags: Vec<CapabilityFlag>,
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
            EffectCategory::Sources,
            EffectCategory::Mcp,
            EffectCategory::Rpc,
            EffectCategory::Spawn,
            EffectCategory::Diagnostics,
            EffectCategory::Wake,
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
                | EffectCategory::Sources
                | EffectCategory::Mcp
                | EffectCategory::Rpc
                | EffectCategory::Spawn
                | EffectCategory::Diagnostics
                | EffectCategory::Wake => out.push(cat),
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
}
