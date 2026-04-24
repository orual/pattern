//! Bundle the full 15-handler SDK into a single `DispatchEffect`.
//!
//! Handler position in the HList is the JIT effect tag: agent programs must
//! declare `Eff '[...]` rows whose head prefix aligns with this order. The
//! canonical order is: `Memory, Search, Recall, Tasks` (storage-adjacent),
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
    DiagnosticsHandler, DisplayHandler, FileHandler, LogHandler, McpHandler, MemoryHandler,
    MessageHandler, RecallHandler, RpcHandler, SearchHandler, ShellHandler, SourcesHandler,
    SpawnHandler, TasksHandler, TimeHandler,
};

/// The full 15-handler SDK bundle, typed as a `frunk::HList`.
///
/// Order: `Memory, Search, Recall, Tasks, Message, Display, Time, Log,
/// Shell, File, Sources, Mcp, Rpc, Spawn, Diagnostics`. Search, Recall,
/// and Tasks are placed immediately after Memory (storage-adjacent) so
/// cross-agent search, archival, and task-graph operations cluster
/// together. Diagnostics is last (rarely used; session-level
/// introspection only).
pub type SdkBundle = frunk::HList![
    MemoryHandler,
    SearchHandler,
    RecallHandler,
    TasksHandler,
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
];

/// Collect [`crate::sdk::describe::EffectDecl`] from every handler in
/// the canonical bundle order. Used by the preamble assembler to
/// generate the Haskell boilerplate.
pub fn canonical_effect_decls() -> Vec<crate::sdk::describe::EffectDecl> {
    SdkBundle::collect_decls()
}

/// The canonical effect-row type names in bundle order. Useful for
/// assertions and documentation.
pub const CANONICAL_EFFECT_ROW: &[&str] = &[
    "Memory",
    "Search",
    "Recall",
    "Tasks",
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
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_decls_has_15_entries() {
        let decls = canonical_effect_decls();
        assert_eq!(
            decls.len(),
            15,
            "expected 15 handler decls, got {}",
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
}
