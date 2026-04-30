//! Effect-class classification table for the canonical SDK effect row.
//!
//! See [`pattern_core::EffectClass`] for the class enum. Every constructor in
//! `bundle::CANONICAL_EFFECT_ROW` MUST have an entry here. Drift detection
//! tests in `bundle.rs` enforce this invariant.

use pattern_core::{EffectClass, RuntimeClassCheck};

/// Per-constructor classification.
#[derive(Debug, Clone, Copy)]
pub struct ConstructorClass {
    pub module: &'static str,
    pub constructor: &'static str,
    pub class: EffectClass,
    pub runtime_check: RuntimeClassCheck,
}

/// Canonical classification table. 73 entries.
pub const ALL_CLASSES: &[ConstructorClass] = &[
    // ── Pattern.Memory (10) ──────────────────────────────────────────────
    ConstructorClass {
        module: "Memory",
        constructor: "Get",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Memory",
        constructor: "Put",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Memory",
        constructor: "Create",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Memory",
        constructor: "Append",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Memory",
        constructor: "Replace",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Memory",
        constructor: "Search",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Memory",
        constructor: "Recall",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Memory",
        constructor: "GetShared",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Memory",
        constructor: "WriteToPersona",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    // ── Pattern.Search (3) ───────────────────────────────────────────────
    ConstructorClass {
        module: "Search",
        constructor: "SearchMessages",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Search",
        constructor: "SearchArchival",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Search",
        constructor: "SearchAll",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Skip,
    },
    // ── Pattern.Recall (3) ───────────────────────────────────────────────
    ConstructorClass {
        module: "Recall",
        constructor: "RecallInsert",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Recall",
        constructor: "RecallSearch",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Recall",
        constructor: "RecallGet",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    // ── Pattern.Tasks (8) ────────────────────────────────────────────────
    ConstructorClass {
        module: "Tasks",
        constructor: "Create",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Tasks",
        constructor: "Update",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Tasks",
        constructor: "Transition",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Tasks",
        constructor: "Link",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Tasks",
        constructor: "Unlink",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Tasks",
        constructor: "List",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Tasks",
        constructor: "QueryGraph",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Tasks",
        constructor: "AddComment",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    // ── Pattern.Skills (5) ───────────────────────────────────────────────
    ConstructorClass {
        module: "Skills",
        constructor: "List",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Skills",
        constructor: "GetMetadata",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Skills",
        constructor: "Load",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Skills",
        constructor: "Search",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Skills",
        constructor: "GetUsageStats",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    // ── Pattern.Message (5) ──────────────────────────────────────────────
    ConstructorClass {
        module: "Message",
        constructor: "Ask",
        class: EffectClass::Escape,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Message",
        constructor: "Send",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Message",
        constructor: "Reply",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Message",
        constructor: "Notify",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Message",
        constructor: "Delegate",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    // ── Pattern.Display (3) ──────────────────────────────────────────────
    ConstructorClass {
        module: "Display",
        constructor: "Chunk",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Display",
        constructor: "Final",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Display",
        constructor: "Note",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    // ── Pattern.Time (2) ─────────────────────────────────────────────────
    ConstructorClass {
        module: "Time",
        constructor: "Now",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Time",
        constructor: "Sleep",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    // ── Pattern.Log (4) ──────────────────────────────────────────────────
    ConstructorClass {
        module: "Log",
        constructor: "Debug",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Log",
        constructor: "Info",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Log",
        constructor: "Warn",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Log",
        constructor: "Error",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    // ── Pattern.Shell (4) ────────────────────────────────────────────────
    ConstructorClass {
        module: "Shell",
        constructor: "Execute",
        class: EffectClass::Escape,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Shell",
        constructor: "Spawn",
        class: EffectClass::Escape,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Shell",
        constructor: "Kill",
        class: EffectClass::Escape,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Shell",
        constructor: "Status",
        class: EffectClass::Escape,
        runtime_check: RuntimeClassCheck::Skip,
    },
    // ── Pattern.File (8) ─────────────────────────────────────────────────
    ConstructorClass {
        module: "File",
        constructor: "Read",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "File",
        constructor: "Write",
        class: EffectClass::MutateExternal,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "File",
        constructor: "ListDir",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "File",
        constructor: "Open",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "File",
        constructor: "Close",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "File",
        constructor: "Watch",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "File",
        constructor: "Reload",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "File",
        constructor: "ForceWrite",
        class: EffectClass::MutateExternal,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "File",
        constructor: "InsertLines",
        class: EffectClass::MutateExternal,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "File",
        constructor: "ReplaceLines",
        class: EffectClass::MutateExternal,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "File",
        constructor: "DeleteLines",
        class: EffectClass::MutateExternal,
        runtime_check: RuntimeClassCheck::Skip,
    },
    // ── Pattern.Mcp (1) ──────────────────────────────────────────────────
    ConstructorClass {
        module: "Mcp",
        constructor: "Use",
        class: EffectClass::Escape,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    // ── Pattern.Spawn (7) ────────────────────────────────────────────────
    ConstructorClass {
        module: "Spawn",
        constructor: "Ephemeral",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Spawn",
        constructor: "AwaitSpawn",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Spawn",
        constructor: "AwaitAll",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Spawn",
        constructor: "Fork",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Spawn",
        constructor: "Sibling",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Spawn",
        constructor: "Stop",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Spawn",
        constructor: "ForkOp",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    // ── Pattern.Diagnostics (1) ──────────────────────────────────────────
    ConstructorClass {
        module: "Diagnostics",
        constructor: "GetDiagnostics",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    // ── Pattern.Wake (2) ─────────────────────────────────────────────────
    ConstructorClass {
        module: "Wake",
        constructor: "Register",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Wake",
        constructor: "Unregister",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    // ── Pattern.Fronting (4) ─────────────────────────────────────────────
    ConstructorClass {
        module: "Fronting",
        constructor: "Current",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Fronting",
        constructor: "Set",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Fronting",
        constructor: "Route",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Fronting",
        constructor: "Clear",
        class: EffectClass::Coordinate,
        runtime_check: RuntimeClassCheck::Skip,
    },
    // ── Pattern.Port (4) ─────────────────────────────────────────────────
    ConstructorClass {
        module: "Port",
        constructor: "List",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Port",
        constructor: "Call",
        class: EffectClass::Escape,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Port",
        constructor: "Subscribe",
        class: EffectClass::Escape,
        runtime_check: RuntimeClassCheck::Skip,
    },
    ConstructorClass {
        module: "Port",
        constructor: "Unsubscribe",
        class: EffectClass::MutateInternal,
        runtime_check: RuntimeClassCheck::Skip,
    },
    // ── Pattern.Constellation (3) ────────────────────────────────────────
    ConstructorClass {
        module: "Constellation",
        constructor: "List",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Constellation",
        constructor: "Find",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
    ConstructorClass {
        module: "Constellation",
        constructor: "Groups",
        class: EffectClass::Observe,
        runtime_check: RuntimeClassCheck::Enforce,
    },
];

/// Look up a constructor's class. Returns `None` if the (module, constructor)
/// pair is not in the table — caller should treat that as drift.
pub fn lookup(module: &str, constructor: &str) -> Option<&'static ConstructorClass> {
    ALL_CLASSES
        .iter()
        .find(|c| c.module == module && c.constructor == constructor)
}

/// Returns the set of [`EffectClass`] values that appear among a module's
/// constructors. Used by the prelude filter to decide whether a module has
/// any surviving constructors for a given `allowed_classes` set.
pub fn classes_for_module(module: &str) -> std::collections::BTreeSet<EffectClass> {
    ALL_CLASSES
        .iter()
        .filter(|c| c.module == module)
        .map(|c| c.class)
        .collect()
}

/// Runtime class-check gate for handler dispatch.
///
/// For constructors with [`RuntimeClassCheck::Enforce`], verifies that the
/// constructor's class is in the agent's `allowed_classes`. Returns `Ok(())`
/// when:
/// - The constructor has `RuntimeClassCheck::Skip` (existing system is authoritative).
/// - No capabilities are configured (backwards-compatible full access).
/// - The class is in the effective allowed set.
///
/// Returns `Err(EffectError::Handler(...))` when the class is denied.
pub fn check_effect_class(
    caps: Option<&pattern_core::CapabilitySet>,
    module: &str,
    constructor: &str,
) -> Result<(), tidepool_effect::EffectError> {
    let entry = match lookup(module, constructor) {
        Some(e) => e,
        None => {
            return Err(tidepool_effect::EffectError::Handler(format!(
                "unknown constructor {module}.{constructor} (effect class table drift)"
            )));
        }
    };
    if entry.runtime_check == RuntimeClassCheck::Skip {
        return Ok(());
    }
    let caps = match caps {
        Some(c) => c,
        None => return Ok(()), // no capability set → full access (backwards-compat).
    };
    let allowed = caps.effective_allowed_classes();
    if allowed.contains(&entry.class) {
        Ok(())
    } else {
        Err(tidepool_effect::EffectError::Handler(format!(
            "{module}.{constructor} requires effect class {:?}; agent has {:?}",
            entry.class, allowed
        )))
    }
}
