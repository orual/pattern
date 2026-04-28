//! Bundle the full 17-handler SDK into a single `DispatchEffect`.
//!
//! Handler position in the HList is the JIT effect tag: agent programs must
//! declare `Eff '[...]` rows whose head prefix aligns with this order. The
//! canonical order is: `Memory, Search, Recall, Tasks, Skills` (storage-adjacent),
//! then `Message, Display, Time, Log` (Prelude-5 minus Memory), then rarer
//! effects (`Shell, File, Mcp, Spawn, Diagnostics`), then coordination
//! (`Wake, Fronting`), then external services (`Port`).
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
//! unqualified imports across all modules without ambiguity in current
//! Pattern. Storage-adjacent grouping is kept for clarity.
//!
//! v3-sandbox-io Phase 4 retired `Sources` and `Rpc` in favor of the
//! unified `Port` effect. v3-multi-agent Phase 4 added `Wake`; Phase 5
//! added `Fronting`. The merged canonical row reflects both lines:
//! Sources/Rpc are GONE, Wake + Fronting + Port are PRESENT.
//!
//! Individual handler structs remain available for ad-hoc bundles (see
//! `crate::sdk::handlers`).

use crate::sdk::describe::CollectEffectDecls;
use crate::sdk::handlers::{
    ConstellationHandler, DiagnosticsHandler, DisplayHandler, FileHandler, FrontingHandler,
    LogHandler, McpHandler, MemoryHandler, MessageHandler, PortHandler, RecallHandler,
    SearchHandler, ShellHandler, SkillsHandler, SpawnHandler, TasksHandler, TimeHandler,
    WakeHandler,
};

/// The full 17-handler SDK bundle, typed as a `frunk::HList`.
///
/// Order: `Memory, Search, Recall, Tasks, Skills, Message, Display, Time, Log,
/// Shell, File, Mcp, Spawn, Diagnostics, Wake, Fronting, Port`.
/// Search, Recall, Tasks, and Skills are placed immediately after Memory
/// (storage-adjacent) so cross-agent search, archival, task-graph, and skill
/// operations cluster together. Diagnostics is session-level introspection;
/// Wake + Fronting follow it as coordination effects (v3-multi-agent
/// Phase 4-5); Port is appended last (unified external-service port,
/// v3-sandbox-io Phase 4) — agent programs encode effect positions in
/// their `Eff '[...]` row shapes, so adding to the end (rather than
/// mid-list) keeps previously-compiled programs valid.
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
    McpHandler,
    SpawnHandler,
    DiagnosticsHandler,
    WakeHandler,
    FrontingHandler,
    PortHandler,
    ConstellationHandler,
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
/// Filtering is two-level:
///
/// 1. **Category-level**: decls whose `type_name` doesn't resolve to a
///    permitted [`pattern_core::EffectCategory`] are dropped entirely.
///    Decls with no matching `EffectCategory` variant are also dropped
///    (guards against drift where a new handler is added before
///    `EffectCategory` has a matching variant; the
///    `canonical_row_matches_effect_category_implemented_set` test catches
///    this in CI; this filter fails closed at runtime).
///
/// 2. **Per-constructor class-level**: when
///    [`pattern_core::CapabilitySet::allowed_classes`] is non-empty, each
///    constructor is checked against the classification table in
///    [`crate::sdk::effect_classes`]. Constructors whose class is not in
///    `allowed_classes` are dropped. If all constructors of a module are
///    dropped the module is removed entirely.
///
///    When `allowed_classes` is empty the constructor list is preserved
///    unchanged (backwards-compatible full access; see
///    [`pattern_core::CapabilitySet::effective_allowed_classes`]).
pub fn filtered_effect_decls(
    caps: &pattern_core::CapabilitySet,
) -> Vec<crate::sdk::describe::EffectDecl> {
    use crate::sdk::describe::EffectDecl;
    use crate::sdk::describe::parse_constructor;
    use crate::sdk::effect_classes::lookup;
    use std::borrow::Cow;

    let allowed_classes = caps.effective_allowed_classes();
    let filter_by_class = !allowed_classes.is_empty();

    canonical_effect_decls()
        .into_iter()
        .filter_map(|decl| {
            // Category-level filter.
            let cat_ok = pattern_core::EffectCategory::from_type_name(decl.type_name)
                .map(|cat| caps.contains(cat))
                .unwrap_or(false);
            if !cat_ok {
                return None;
            }

            // Per-constructor class-level filter.
            // When allowed_classes is empty, skip filtering (full access).
            if !filter_by_class {
                return Some(decl);
            }

            let kept: Vec<&'static str> = decl
                .constructors
                .iter()
                .filter(|sig| {
                    let name = match parse_constructor(sig) {
                        Ok(p) => p.name,
                        Err(e) => {
                            tracing::warn!(
                                module = decl.type_name,
                                sig = sig,
                                error = %e,
                                "filtered_effect_decls: unparseable constructor signature — dropping"
                            );
                            return false;
                        }
                    };
                    match lookup(decl.type_name, &name) {
                        Some(cc) => allowed_classes.contains(&cc.class),
                        None => {
                            // Constructor is not in the class table — this is
                            // drift; treat as dropped so the agent can't invoke
                            // an unclassified constructor.
                            tracing::warn!(
                                module = decl.type_name,
                                constructor = %name,
                                "filtered_effect_decls: constructor missing from class table (drift) — dropping"
                            );
                            false
                        }
                    }
                })
                .copied()
                .collect();

            if kept.is_empty() {
                // All constructors filtered — drop the module entirely.
                None
            } else {
                Some(EffectDecl {
                    constructors: Cow::Owned(kept),
                    ..decl
                })
            }
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
    "Mcp",
    "Spawn",
    "Diagnostics",
    "Wake",
    "Fronting",
    "Port",
    "Constellation",
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
            for ctor in decl.constructors.iter() {
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
    #[test]
    fn canonical_row_matches_effect_category_implemented_set() {
        use pattern_core::EffectCategory;

        const RESERVED_NOT_IN_ROW: &[EffectCategory] = &[];

        for name in CANONICAL_EFFECT_ROW {
            let cat = EffectCategory::from_type_name(name).unwrap_or_else(|| {
                panic!("CANONICAL_EFFECT_ROW entry {name:?} has no matching EffectCategory variant")
            });
            assert!(
                !RESERVED_NOT_IN_ROW.contains(&cat),
                "{cat:?} is listed as reserved but appears in CANONICAL_EFFECT_ROW"
            );
        }

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

    /// Verify `Pattern.Fronting` registers with four constructors at slot 15.
    #[test]
    fn fronting_effect_registers_with_four_methods_at_tag_15() {
        let decls = canonical_effect_decls();
        let (tag, fronting) = decls
            .iter()
            .enumerate()
            .find(|(_, d)| d.type_name == "Fronting")
            .expect("Fronting must appear in canonical decls");
        assert_eq!(tag, 15, "Fronting must be at slot 15 (after Wake)");
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
    /// at tag 4.
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

    /// Verify `Pattern.Port` still registers at slot 16 (Constellation now appended at 17).
    #[test]
    fn port_effect_registers_at_slot_16() {
        let decls = canonical_effect_decls();
        let (tag, _port) = decls
            .iter()
            .enumerate()
            .find(|(_, d)| d.type_name == "Port")
            .expect("Port must appear in canonical decls");
        assert_eq!(tag, 16, "Port must be at slot 16");
    }

    /// Verify `Pattern.Constellation` registers at the last slot (tag 17).
    #[test]
    fn constellation_effect_registers_at_last_tag() {
        let decls = canonical_effect_decls();
        let (tag, c) = decls
            .iter()
            .enumerate()
            .find(|(_, d)| d.type_name == "Constellation")
            .expect("Constellation must appear in canonical decls");
        assert_eq!(tag, 17, "Constellation must be at the last slot (17)");
        assert_eq!(
            c.constructors.len(),
            3,
            "Pattern.Constellation must enumerate 3 constructors (List, Find, Groups)"
        );
    }

    // ── Drift-detection tests ────────────────────────────────────────────────

    /// Every constructor in `canonical_effect_decls()` must have a
    /// classification entry in `ALL_CLASSES`. If this fails, a new
    /// constructor was added to a handler's `effect_decl()` but not to
    /// the classification table — add the entry to keep the runtime
    /// guard complete.
    #[test]
    fn every_canonical_constructor_has_class_entry() {
        use crate::sdk::describe::parse_constructor;
        use crate::sdk::effect_classes::lookup;
        let decls = canonical_effect_decls();
        let mut missing = vec![];
        for decl in &decls {
            for sig in decl.constructors.iter() {
                let name = parse_constructor(sig).expect("constructor must parse").name;
                if lookup(decl.type_name, &name).is_none() {
                    missing.push(format!("{}::{}", decl.type_name, name));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "constructors missing from ALL_CLASSES: {missing:?}"
        );
    }

    /// Every entry in `ALL_CLASSES` must correspond to a constructor in
    /// `canonical_effect_decls()`. If this fails, a handler was removed
    /// or its constructor was renamed without updating the class table —
    /// orphaned entries are a sign of stale configuration.
    #[test]
    fn no_orphan_classifications() {
        use crate::sdk::describe::parse_constructor;
        use crate::sdk::effect_classes::ALL_CLASSES;
        let decls = canonical_effect_decls();
        let mut orphans = vec![];
        for entry in ALL_CLASSES {
            let in_canonical = decls.iter().any(|d| {
                d.type_name == entry.module
                    && d.constructors.iter().any(|sig| {
                        parse_constructor(sig)
                            .map(|p| p.name)
                            .as_deref()
                            .map(|n| n == entry.constructor)
                            .unwrap_or(false)
                    })
            });
            if !in_canonical {
                orphans.push(format!("{}::{}", entry.module, entry.constructor));
            }
        }
        assert!(
            orphans.is_empty(),
            "ALL_CLASSES entries with no canonical constructor: {orphans:?}"
        );
    }

    /// Pin the exact size of the classification table. Update this test
    /// whenever a new constructor is added or removed. The count is 77:
    /// 18 modules × varying constructor counts (Memory=10, Search=3,
    /// Recall=3, Tasks=8, Skills=5, Message=5, Display=3, Time=2, Log=4,
    /// Shell=4, File=8, Mcp=1, Spawn=7, Diagnostics=1, Wake=2, Fronting=4,
    /// Port=4, Constellation=3). The reference enumeration doc said "73"
    /// but was written before Fronting (4) and Constellation (3) were
    /// finalised; the canonical count is 77.
    #[test]
    fn classification_table_has_77_entries() {
        use crate::sdk::effect_classes::ALL_CLASSES;
        assert_eq!(ALL_CLASSES.len(), 77);
    }

    // ── Behavior tests ───────────────────────────────────────────────────────

    /// With `allowed_classes = {Observe}`, Memory.Get (Observe) must survive
    /// but Memory.Put (MutateInternal) must be filtered from the rendered prelude.
    #[test]
    fn observe_only_capset_drops_mutateinternal_constructors() {
        use pattern_core::{CapabilitySet, EffectClass};
        let caps = CapabilitySet::all().with_classes([EffectClass::Observe]);
        let decls = filtered_effect_decls(&caps);
        let memory = decls
            .iter()
            .find(|d| d.type_name == "Memory")
            .expect("Memory module must survive (it has Observe constructors)");
        let mem_ctors: Vec<&&str> = memory.constructors.iter().collect();
        // Get is Observe → should be present.
        assert!(
            mem_ctors.iter().any(|s| s.starts_with("Get ")),
            "Memory.Get must remain: {mem_ctors:?}"
        );
        // Put is MutateInternal → should be filtered.
        assert!(
            !mem_ctors.iter().any(|s| s.starts_with("Put ")),
            "Memory.Put must be filtered: {mem_ctors:?}"
        );
    }

    /// With `allowed_classes = {Observe}`, modules with no Observe constructors
    /// must be dropped entirely (e.g. Shell has only Escape constructors).
    #[test]
    fn observe_only_capset_drops_modules_with_no_observe_constructors() {
        use pattern_core::{CapabilitySet, EffectClass};
        let caps = CapabilitySet::all().with_classes([EffectClass::Observe]);
        let decls = filtered_effect_decls(&caps);
        // Shell has only Escape constructors → must be dropped.
        assert!(
            !decls.iter().any(|d| d.type_name == "Shell"),
            "Shell must be filtered out of Observe-only prelude"
        );
        // Log has Observe constructors → must survive.
        assert!(
            decls.iter().any(|d| d.type_name == "Log"),
            "Log must survive in Observe-only prelude (all Log constructors are Observe)"
        );
    }

    /// When `allowed_classes` is empty (the default for `CapabilitySet::all()`),
    /// all modules and constructors are preserved unchanged.
    #[test]
    fn empty_allowed_classes_is_full_access_for_backwards_compat() {
        use pattern_core::CapabilitySet;
        // `CapabilitySet::all()` has empty allowed_classes by default.
        let caps = CapabilitySet::all();
        let decls = filtered_effect_decls(&caps);
        let canonical = canonical_effect_decls();
        assert_eq!(
            decls.len(),
            canonical.len(),
            "empty allowed_classes must preserve all modules"
        );
        for (d, c) in decls.iter().zip(canonical.iter()) {
            assert_eq!(
                d.constructors.len(),
                c.constructors.len(),
                "empty allowed_classes must preserve all constructors of {}",
                d.type_name
            );
        }
    }

    // ── Runtime guard tests ──────────────────────────────────────────────────

    /// `check_effect_class` must refuse an Enforce constructor whose class is
    /// not in the agent's allowed set.
    #[test]
    fn runtime_guard_refuses_out_of_class_constructor() {
        use crate::sdk::effect_classes::check_effect_class;
        use pattern_core::{CapabilitySet, EffectClass};
        let caps = CapabilitySet::all().with_classes([EffectClass::Observe]);
        // Memory.Put is MutateInternal/Enforce — must be refused.
        let result = check_effect_class(Some(&caps), "Memory", "Put");
        assert!(
            result.is_err(),
            "Memory.Put must be refused with Observe-only caps"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Memory.Put"),
            "error must identify the effect: {err_msg}"
        );
    }

    /// `check_effect_class` must pass for constructors with `RuntimeClassCheck::Skip`
    /// even when their class is not in `allowed_classes`. Skip means the class
    /// axis is not authoritative for that constructor.
    #[test]
    fn runtime_guard_skips_skip_constructors() {
        use crate::sdk::effect_classes::check_effect_class;
        use pattern_core::{CapabilitySet, EffectClass};
        let caps = CapabilitySet::all().with_classes([EffectClass::Observe]);
        // Message.Send is Coordinate/Skip — class check bypassed for Skip.
        let result = check_effect_class(Some(&caps), "Message", "Send");
        assert!(
            result.is_ok(),
            "Skip constructors must not be class-checked"
        );
    }

    /// When no capability set is configured (`None`), `check_effect_class`
    /// returns `Ok(())` for all constructors (backwards-compatible full access).
    #[test]
    fn runtime_guard_passes_when_no_caps() {
        use crate::sdk::effect_classes::check_effect_class;
        // No capabilities: full access (backwards-compat).
        let result = check_effect_class(None, "Memory", "Put");
        assert!(result.is_ok(), "no caps means full access");
    }
}
