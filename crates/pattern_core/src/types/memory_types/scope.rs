//! [`Scope`] — typed ownership boundary for memory blocks.
//!
//! Replaces the prior practice of overloading [`MemoryStore`] methods'
//! `agent_id: &str` parameter as a free-form ownership string. A block's
//! scope is now a sum type:
//!
//! - [`Scope::Local`] — project-scoped block, shared across all agents in
//!   a project mount. Stored under `<mount>/blocks/<type>/<label>.<ext>`.
//! - [`Scope::Global`] — persona-scoped block, follows the persona across
//!   mounts. Stored under
//!   `$XDG_STATE_HOME/pattern/personas/@<persona_id>/blocks/<type>/<label>.<ext>`.
//!
//! The string id inside each variant is the project_id (Local) or the
//! persona_id (Global). Equality treats `Local("x")` and `Global("x")` as
//! distinct — fixing the prior collision bug where a project named
//! `"pattern"` and a persona named `"@pattern"` shared a single keyspace.
//!
//! [`MemoryStore`]: crate::traits::MemoryStore

use core::fmt;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// Ownership boundary for a memory block.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "kebab-case")]
pub enum Scope {
    /// Project-scoped block; shared across all agents in the mount.
    Local(SmolStr),
    /// Persona-scoped block; follows the persona across mounts.
    Global(SmolStr),
}

impl Scope {
    /// Construct a [`Scope::Local`] from any string-like value.
    pub fn local(project_id: impl Into<SmolStr>) -> Self {
        Self::Local(project_id.into())
    }

    /// Construct a [`Scope::Global`] from any string-like value.
    pub fn global(persona_id: impl Into<SmolStr>) -> Self {
        Self::Global(persona_id.into())
    }

    /// The id string carried by this scope (project_id or persona_id).
    pub fn id(&self) -> &str {
        match self {
            Self::Local(id) | Self::Global(id) => id.as_str(),
        }
    }

    /// `true` if this is a project-scoped block.
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }

    /// `true` if this is a persona-scoped block.
    pub fn is_global(&self) -> bool {
        matches!(self, Self::Global(_))
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(id) => write!(f, "local:{id}"),
            Self::Global(id) => write!(f, "global:{id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_and_global_with_same_id_are_distinct() {
        let local = Scope::local("pattern");
        let global = Scope::global("pattern");
        assert_ne!(local, global);
    }

    #[test]
    fn id_returns_inner_string_regardless_of_kind() {
        assert_eq!(Scope::local("pattern").id(), "pattern");
        assert_eq!(Scope::global("flux").id(), "flux");
    }

    #[test]
    fn is_local_and_is_global_are_complementary() {
        let local = Scope::local("p");
        let global = Scope::global("g");
        assert!(local.is_local() && !local.is_global());
        assert!(global.is_global() && !global.is_local());
    }

    #[test]
    fn display_disambiguates_kinds() {
        assert_eq!(Scope::local("pattern").to_string(), "local:pattern");
        assert_eq!(Scope::global("pattern").to_string(), "global:pattern");
    }

    #[test]
    fn serde_round_trip_preserves_kind() {
        let cases = [Scope::local("p1"), Scope::global("a1")];
        for scope in cases {
            let json = serde_json::to_string(&scope).unwrap();
            let parsed: Scope = serde_json::from_str(&json).unwrap();
            assert_eq!(scope, parsed);
        }
    }
}
