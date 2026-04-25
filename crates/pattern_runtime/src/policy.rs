//! Runtime-side policy machinery: built-in defaults, locked rules, and
//! the merger that composes Rust defaults + KDL config + runtime
//! overrides into a single [`pattern_core::PolicySet`] at session
//! open.
//!
//! Pure-data rule types live in `pattern_core::capability::policy`;
//! this module owns the conservative baseline (`defaults`) and the
//! Phase 1 Task 12 / Task 14 wiring that layers KDL on top.

pub mod defaults;

pub use defaults::rust_defaults;

/// Error-message prefix used by handlers to flag a policy denial. Tests
/// pattern-match on this prefix to assert the gate fired without
/// scraping the rest of the message.
///
/// We pack the denial signal into [`tidepool_effect::EffectError::Handler`]
/// (via a string prefix) rather than introducing a new variant in the
/// upstream `tidepool_effect` crate — variant additions there require an
/// upstream patch + `flake.lock` bump, out of scope for Phase 1. When
/// enough handlers accumulate this pattern, promote to a dedicated
/// variant.
pub const PERMISSION_DENIED_PREFIX: &str = "PermissionDenied: ";

/// Error-message prefix flagging that the policy gate fired and
/// approval was granted, but the handler's real implementation lands
/// in a later plan. Used by Shell (Task 10) and File (Task 15) stubs.
pub const GATE_APPROVED_PREFIX: &str = "GateApproved: ";
