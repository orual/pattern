//! Per-persona session UUID façade.
//!
//! Pattern internally has no discrete sessions — one persona runs
//! continuously. Providers (Anthropic especially) expect something
//! session-shaped in request headers, so this module mints a UUID per
//! persona and rotates it on explicit caller signal. From the provider's
//! POV each rotation looks like a new session; internally pattern
//! continues uninterrupted.
//!
//! Rotation triggers are the caller's responsibility:
//! - `compaction.cycle.end` (default, provider sees new session at
//!   compaction boundaries — keeps the rolling-context story tidy)
//! - `persona.detach` (definitive end)
//! - plugin- or user-configurable
//!
//! # AC coverage
//!
//! AC5.3 — session UUID rotates when the caller signals a rotation boundary.
//! The rotator is per-persona; the gateway owns one instance per session.

use parking_lot::Mutex;
use uuid::Uuid;

/// Opaque wrapper around a session UUID. Exposed via `Display`; no direct
/// field access (callers should treat it as an opaque identifier).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PatternSessionUuid(Uuid);

impl PatternSessionUuid {
    /// Access the inner UUID. Rarely needed outside logging + header
    /// construction; prefer the `Display` impl.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl std::fmt::Display for PatternSessionUuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Mutable holder for a [`PatternSessionUuid`]. Reads and rotations are
/// serialized under an internal mutex so concurrent tasks see a
/// consistent value.
pub struct SessionUuidRotator {
    current: Mutex<Uuid>,
}

impl Default for SessionUuidRotator {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionUuidRotator {
    /// Construct a new rotator with a fresh random UUID.
    pub fn new() -> Self {
        Self {
            current: Mutex::new(Uuid::new_v4()),
        }
    }

    /// Construct a rotator with a caller-supplied initial UUID. Primarily
    /// for tests (deterministic) or for restoring a session across
    /// pattern-side restarts.
    pub fn with_initial(uuid: Uuid) -> Self {
        Self {
            current: Mutex::new(uuid),
        }
    }

    /// Read the current session UUID without rotating.
    pub fn current(&self) -> PatternSessionUuid {
        PatternSessionUuid(*self.current.lock())
    }

    /// Generate a fresh UUID, store it, and return the new value.
    pub fn rotate(&self) -> PatternSessionUuid {
        let new = Uuid::new_v4();
        *self.current.lock() = new;
        PatternSessionUuid(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_is_stable_across_reads() {
        let rotator = SessionUuidRotator::new();
        let a = rotator.current();
        let b = rotator.current();
        assert_eq!(a, b, "reads must not rotate");
    }

    #[test]
    fn rotate_produces_new_uuid() {
        let rotator = SessionUuidRotator::new();
        let before = rotator.current();
        let rotated = rotator.rotate();
        let after = rotator.current();

        assert_ne!(before, rotated, "rotate must produce a new UUID");
        assert_eq!(rotated, after, "post-rotation reads must see the new UUID");
    }

    #[test]
    fn with_initial_seeds_deterministically() {
        let fixed = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let rotator = SessionUuidRotator::with_initial(fixed);
        assert_eq!(rotator.current().as_uuid(), fixed);
    }

    #[test]
    fn display_renders_as_uuid_string() {
        let fixed = Uuid::parse_str("11112222-3333-4444-5555-666677778888").unwrap();
        let rotator = SessionUuidRotator::with_initial(fixed);
        let s = rotator.current().to_string();
        assert_eq!(s, "11112222-3333-4444-5555-666677778888");
    }
}
