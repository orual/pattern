//! Fork spawn scaffold — Phase 2 Task 8.
//!
//! A fork is a memory-isolated copy of the parent session with its own
//! compute environment. Phase 2 delivers the lightweight scaffold only:
//!
//! - `ForkIsolation::Lightweight`: returns a typed `ForkHandle` with generated
//!   ids but does NOT execute the fork's program (that lands in Phase 3 when
//!   `LoroDoc::fork()` + program execution are wired).
//! - `ForkIsolation::Persistent`: always returns an error directing the caller
//!   to Phase 3.
//!
//! The capability gate (`compute_child_caps`) is exercised for both paths so
//! the wire-grammar verification works end-to-end in Phase 2.
//!
//! # Phase 3 shape (informational)
//!
//! When Phase 3 lands, `ForkHandle` will gain resolution helpers:
//! ```ignore
//! impl ForkHandle {
//!     pub async fn await_result(&self) -> Result<SpawnResult, SpawnError>;
//!     pub async fn merge_back(&self) -> Result<MergeReport, SpawnError>;
//!     pub async fn discard(&self) -> Result<(), SpawnError>;
//!     pub async fn promote(&self, cfg: PersonaConfig) -> Result<PersonaId, SpawnError>;
//! }
//! ```
//! Phase 3 also wires the actual `LoroDoc::fork()` call and persistent (jj
//! workspace) isolation for `ForkIsolation::Persistent`.

use smol_str::SmolStr;
use tidepool_bridge_derive::ToCore;

use crate::spawn::SpawnError;

// ── ForkHandle ────────────────────────────────────────────────────────────────

/// Handle referencing an in-progress fork.
///
/// In Phase 2 this is a scaffold: the ids are generated but no actual fork
/// computation is running. Phase 3 adds resolution helpers and the
/// `LoroDoc::fork()` compute path.
///
/// The `fork_id` identifies the fork operation; `child_id` identifies the
/// child session that will execute the fork's program. Both are stable
/// references for log correlation and Phase 3 result lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkHandle {
    /// Stable identifier for this fork operation.
    pub fork_id: SmolStr,
    /// Identifier for the child session executing the fork's program.
    pub child_id: SmolStr,
}

// ── WireForkHandle ────────────────────────────────────────────────────────────

/// Wire mirror of `ForkHandle` for the Haskell return direction.
///
/// Maps to the Haskell type:
/// ```haskell
/// data ForkHandle = ForkHandle
///   { forkHandleId      :: SpawnId
///   , forkHandleChildId :: SpawnId
///   }
/// ```
///
/// The `ToCore` encoding is positional — `fork_id` encodes at position 0,
/// `child_id` at position 1 — so the Haskell record selectors (`forkHandleId`,
/// `forkHandleChildId`) are documentation-only from the wire perspective.
#[derive(Debug, ToCore)]
#[core(module = "Pattern.Spawn", name = "ForkHandle")]
pub struct WireForkHandle {
    /// Stable fork operation identifier.
    pub fork_id: String,
    /// Child session identifier.
    pub child_id: String,
}

impl From<ForkHandle> for WireForkHandle {
    fn from(h: ForkHandle) -> Self {
        Self {
            fork_id: h.fork_id.to_string(),
            child_id: h.child_id.to_string(),
        }
    }
}

// ── check_promote_capability ──────────────────────────────────────────────────

/// Gate the `promote()` resolution helper (Phase 3) on the parent's capability
/// set.
///
/// Returns `Ok(())` when the parent holds
/// [`pattern_core::CapabilityFlag::SpawnNewIdentities`]. Returns
/// [`SpawnError::CapabilityEscalation`] otherwise — `promote()` creates a
/// new identity from a fork, which is the same capability class as
/// `SiblingPersona::New`.
pub fn check_promote_capability(
    parent_caps: &pattern_core::CapabilitySet,
) -> Result<(), SpawnError> {
    if parent_caps.has_flag(pattern_core::CapabilityFlag::SpawnNewIdentities) {
        Ok(())
    } else {
        Err(SpawnError::CapabilityEscalation {
            reason: "promote() requires CapabilityFlag::SpawnNewIdentities; \
                     parent does not hold this flag"
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use pattern_core::{CapabilityFlag, CapabilitySet, EffectCategory};

    use super::*;

    // ── ForkHandle round-trip ───────────────────────────────────────────────

    /// A `ForkHandle` converts into a `WireForkHandle` with the same ids.
    #[test]
    fn fork_handle_into_wire_preserves_ids() {
        let h = ForkHandle {
            fork_id: SmolStr::from("fork-abc"),
            child_id: SmolStr::from("child-xyz"),
        };
        let w: WireForkHandle = h.clone().into();
        assert_eq!(w.fork_id, "fork-abc");
        assert_eq!(w.child_id, "child-xyz");
    }

    // ── check_promote_capability ─────────────────────────────────────────────

    /// Parent with `SpawnNewIdentities` flag: `check_promote_capability` → Ok.
    #[test]
    fn promote_capability_ok_when_flag_present() {
        let caps = CapabilitySet::all().with_flags([CapabilityFlag::SpawnNewIdentities]);
        assert!(
            check_promote_capability(&caps).is_ok(),
            "should be Ok when flag is held"
        );
    }

    /// Parent WITHOUT the flag: `check_promote_capability` → CapabilityEscalation.
    #[test]
    fn promote_capability_err_when_flag_absent() {
        let caps: CapabilitySet = [EffectCategory::Memory].into_iter().collect();
        let err = check_promote_capability(&caps).expect_err("should fail without the flag");
        match err {
            SpawnError::CapabilityEscalation { reason } => {
                assert!(
                    reason.contains("SpawnNewIdentities"),
                    "error should name the missing flag; got: {reason}"
                );
            }
            other => panic!("expected CapabilityEscalation, got {other:?}"),
        }
    }
}
