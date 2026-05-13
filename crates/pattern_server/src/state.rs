//! Daemon state file management — moved to `pattern_core::daemon_state`.
//!
//! Re-export shim so existing `pattern_server::state::DaemonState` imports keep working.
//! New code should use `pattern_core::daemon_state::DaemonState` directly.

pub use pattern_core::daemon_state::*;
