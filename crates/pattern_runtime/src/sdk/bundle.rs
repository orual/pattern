//! Bundle the full 16-handler SDK into a single `DispatchEffect`.
//!
//! Handler position in the HList is the JIT effect tag: agent programs must
//! declare `Eff '[...]` rows whose head prefix aligns with this order. The
//! canonical order is: `Memory, Search, Recall, Tasks, Skills` (storage-adjacent),
//! then `Message, Display, Time, Log` (Prelude-5 minus Memory), then rarer
//! effects (`Shell, File, Sources, Mcp, Rpc, Spawn, Diagnostics`).
//!
//! **Why Prelude-5-first (historical note):** originally this ordering was
//! required to avoid DataCon name collisions: tidepool-bridge looked up
//! constructors by unqualified name, which failed when distinct modules
//! declared same-named constructors. The fork at `github:orual/tidepool`
//! first added `get_by_name_arity` (arity disambiguation) and later
//! module-qualified lookup via `#[core(module = "Pattern.<Module>",
//! name = "...")]`. Memory uses `Get`/`Put` (KV semantics) rather than
//! `Read`/`Write`, so the remaining residual collisions (e.g. both
//! `Memory.Get` and no `File.Get`) are handled entirely at the
//! derive-layer disambiguation stage — agent programs can mix
//! unqualified imports across all thirteen modules without ambiguity in
//! current Pattern. Storage-adjacent grouping is kept for clarity.
//!
//! Individual handler structs remain available for ad-hoc bundles (see
//! `crate::sdk::handlers`).

use crate::sdk::describe::CollectEffectDecls;
use crate::sdk::handlers::{
    DiagnosticsHandler, DisplayHandler, FileHandler, FrontingHandler, LogHandler, McpHandler,
    MemoryHandler, MessageHandler, RecallHandler, RpcHandler, SearchHandler, ShellHandler,
    SkillsHandler, SourcesHandler, SpawnHandler, TasksHandler, TimeHandler, WakeHandler,
};

/// The full 18-handler SDK bundle, typed as a `frunk::HList`.
///
/// Order: `Memory, Search, Recall, Tasks, Skills, Message, Display, Time, Log,
/// Shell, File, Sources, Mcp, Rpc, Spawn, Diagnostics, Wake, Fronting`.
/// Search, Recall, Tasks, and Skills are placed immediately after Memory
/// (storage-adjacent) so cross-agent search, archival, task-graph, and skill
/// operations cluster together. Diagnostics is session-level introspection;
/// Wake follows it as the second-to-last entry; Fronting is appended last
/// as the newest effect — agent programs encode effect positions in their
/// `Eff '[...]` row shapes, so adding to the end (rather than mid-list) keeps
/// previously-compiled programs valid.
pub type SdkBundle = frunk::HList![
    MemoryHandler,
    SearchHandler,
    RecallHandler,
    TasksHandler,
    SkillsHandler,
    MessageHandler,
    DisplayHandler,
    TimeHandler,
    LogHandler,
    ShellHandler,
    FileHandler,
    SourcesHandler,
    McpHandler,
    RpcHandler,
    SpawnHandler,
    DiagnosticsHandler,
    WakeHandler,
    FrontingHandler,
];

/// Collect [`crate::sdk::describe::EffectDecl`] from every handler in
/// the canonical bundle order. Used by the preamble assembler to
/// generate the Haskell boilerplate.
pub fn canonical_effect_decls() -> Vec<crate::sdk::describe::EffectDecl> {
    SdkBundle::collect_decls()
}

/// Filter [`canonical_effect_decls`] down to the effects an agent's
/// capability set permits.
///
/// Decls whose `type_name` doesn't resolve to a known
/// [`pattern_core::EffectCategory`] are excluded — this protects against
/// drift where a new handler is added to `CANONICAL_EFFECT_ROW` before
/// `EffectCategory` has a matching variant (the
/// `canonical_row_matches_effect_category_implemented_set` test catches
/// this in CI; this filter fails closed at runtime).
pub fn filtered_effect_decls(
    caps: &pattern_core::CapabilitySet,
) -> Vec<crate::sdk::describe::EffectDecl> {
    canonical_effect_decls()
        .into_iter()
        .filter(|decl| {
            pattern_core::EffectCategory::from_type_name(decl.type_name)
                .map(|cat| caps.contains(cat))
                .unwrap_or(false)
        })
        .collect()
}

/// The canonical effect-row type names in bundle order. Useful for
/// assertions and documentation.
pub const CANONICAL_EFFECT_ROW: &[&str] = &[
    "Memory",
    "Search",
    "Recall",
    "Tasks",
    "Skills",
    "Message",
    "Display",
    "Time",
    "Log",
    "Shell",
    "File",
    "Sources",
    "Mcp",
    "Rpc",
    "Spawn",
    "Diagnostics",
    "Wake",
    "Fronting",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_decls_has_18_entries() {
        let decls = canonical_effect_decls();
        assert_eq!(
            decls.len(),
            18,
            "expected 18 handler decls, got {}",
            decls.len()
        );
    }

    #[test]
    fn canonical_decl_order_matches_row() {
        let decls = canonical_effect_decls();
        let names: Vec<&str> = decls.iter().map(|d| d.type_name).collect();
        assert_eq!(names, CANONICAL_EFFECT_ROW);
    }

    #[test]
    fn every_decl_has_at_least_one_constructor() {
        for decl in canonical_effect_decls() {
            assert!(
                !decl.constructors.is_empty(),
                "{} has no constructors",
                decl.type_name
            );
        }
    }

    #[test]
    fn every_constructor_parses() {
        use crate::sdk::describe::parse_constructor;
        for decl in canonical_effect_decls() {
            for ctor in decl.constructors {
                let parsed = parse_constructor(ctor);
                assert!(
                    parsed.is_ok(),
                    "failed to parse constructor {:?} in {}: {}",
                    ctor,
                    decl.type_name,
                    parsed.unwrap_err()
                );
            }
        }
    }

    /// Verify the Pattern.Tasks effect registers all eight expected methods
    /// (Create, Update, Transition, Link, Unlink, List, QueryGraph, AddComment)
    /// and appears in the storage-adjacent position (tag 3, after Recall).
    #[test]
    fn tasks_effect_registers_with_eight_methods_at_tag_3() {
        let decls = canonical_effect_decls();
        let (tag, tasks) = decls
            .iter()
            .enumerate()
            .find(|(_, d)| d.type_name == "Tasks")
            .expect("Tasks must appear in canonical decls");
        assert_eq!(
            tag, 3,
            "Tasks must be at tag 3 (storage-adjacent after Recall)"
        );
        assert_eq!(
            tasks.constructors.len(),
            8,
            "Pattern.Tasks must enumerate all 8 methods"
        );
        let names: std::collections::HashSet<&str> = tasks
            .constructors
            .iter()
            .filter_map(|c| c.split_whitespace().next())
            .collect();
        for expected in [
            "Create",
            "Update",
            "Transition",
            "Link",
            "Unlink",
            "List",
            "QueryGraph",
            "AddComment",
        ] {
            assert!(
                names.contains(expected),
                "missing Pattern.Tasks method {expected:?}, got {names:?}"
            );
        }
    }

    /// Cross-check that every entry in `CANONICAL_EFFECT_ROW` resolves to
    /// an `EffectCategory` variant, and that every non-reserved
    /// `EffectCategory` variant has a matching entry in the row.
    ///
    /// Adding an 18th handler to `CANONICAL_EFFECT_ROW` without a matching
    /// `EffectCategory` variant fails this test. Adding a new
    /// `EffectCategory` variant without listing it in `RESERVED_NOT_IN_ROW`
    /// also fails. After v3-multi-agent Phase 4, every `EffectCategory`
    /// variant is live (no reservations).
    #[test]
    fn canonical_row_matches_effect_category_implemented_set() {
        use pattern_core::EffectCategory;

        const RESERVED_NOT_IN_ROW: &[EffectCategory] = &[];

        // Every name in the row resolves to a category.
        for name in CANONICAL_EFFECT_ROW {
            let cat = EffectCategory::from_type_name(name).unwrap_or_else(|| {
                panic!("CANONICAL_EFFECT_ROW entry {name:?} has no matching EffectCategory variant")
            });
            assert!(
                !RESERVED_NOT_IN_ROW.contains(&cat),
                "{cat:?} is listed as reserved but appears in CANONICAL_EFFECT_ROW"
            );
        }

        // Every non-reserved EffectCategory has a row entry.
        for cat in EffectCategory::ALL.iter().copied() {
            if RESERVED_NOT_IN_ROW.contains(&cat) {
                continue;
            }
            assert!(
                CANONICAL_EFFECT_ROW.contains(&cat.type_name()),
                "EffectCategory::{cat:?} ({:?}) missing from CANONICAL_EFFECT_ROW",
                cat.type_name()
            );
        }
    }

    /// Verify `Pattern.Fronting` registers with four constructors and appears
    /// at slot 17 (0-indexed), the last entry in the canonical row.
    #[test]
    fn fronting_effect_registers_with_four_methods_at_tag_17() {
        let decls = canonical_effect_decls();
        let (tag, fronting) = decls
            .iter()
            .enumerate()
            .find(|(_, d)| d.type_name == "Fronting")
            .expect("Fronting must appear in canonical decls");
        assert_eq!(tag, 17, "Fronting must be at slot 17 (appended after Wake)");
        assert_eq!(
            fronting.constructors.len(),
            4,
            "Pattern.Fronting must enumerate all 4 constructors"
        );
        let names: std::collections::HashSet<&str> = fronting
            .constructors
            .iter()
            .filter_map(|c| c.split_whitespace().next())
            .collect();
        for expected in ["Current", "Set", "Route", "Clear"] {
            assert!(
                names.contains(expected),
                "missing Pattern.Fronting constructor {expected:?}, got {names:?}"
            );
        }
    }

    /// Verify the Pattern.Skills effect registers all five expected methods
    /// and appears immediately after Tasks (tag 4).
    #[test]
    fn skills_effect_registers_with_five_methods_at_tag_4() {
        let decls = canonical_effect_decls();
        let (tag, skills) = decls
            .iter()
            .enumerate()
            .find(|(_, d)| d.type_name == "Skills")
            .expect("Skills must appear in canonical decls");
        assert_eq!(
            tag, 4,
            "Skills must be at tag 4 (storage-adjacent after Tasks)"
        );
        assert_eq!(
            skills.constructors.len(),
            5,
            "Pattern.Skills must enumerate all 5 methods"
        );
        let names: std::collections::HashSet<&str> = skills
            .constructors
            .iter()
            .filter_map(|c| c.split_whitespace().next())
            .collect();
        for expected in ["List", "GetMetadata", "Load", "Search", "GetUsageStats"] {
            assert!(
                names.contains(expected),
                "missing Pattern.Skills method {expected:?}, got {names:?}"
            );
        }
    }
}
