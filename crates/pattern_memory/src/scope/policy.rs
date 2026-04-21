//! Scope binding configuration for [`super::MemoryScope`].

use pattern_core::types::memory_types::IsolatePolicy;

/// Binding that describes the persona/project relationship for scope routing.
///
/// The `persona_id` is the agent ID of the persona. The optional `project_id`
/// is the agent ID namespace for the project. The `policy` determines how
/// reads and writes are routed between the two scopes.
///
/// When `project_id` is `None`, the scope layer is effectively passthrough
/// regardless of policy (there is no project scope to route to/from).
#[derive(Debug, Clone)]
pub struct ScopeBinding {
    /// Agent ID of the persona whose blocks may be restricted.
    pub persona_id: String,
    /// Agent ID namespace for the project scope. When `None`, the scope
    /// layer passes through to the underlying store unchanged.
    pub project_id: Option<String>,
    /// Isolation policy governing read/write routing.
    pub policy: IsolatePolicy,
}

impl ScopeBinding {
    /// Create a passthrough binding (no project, policy None).
    pub fn passthrough(persona_id: impl Into<String>) -> Self {
        Self {
            persona_id: persona_id.into(),
            project_id: None,
            policy: IsolatePolicy::None,
        }
    }

    /// Create a binding with a project scope.
    pub fn with_project(
        persona_id: impl Into<String>,
        project_id: impl Into<String>,
        policy: IsolatePolicy,
    ) -> Self {
        Self {
            persona_id: persona_id.into(),
            project_id: Some(project_id.into()),
            policy,
        }
    }

    /// Returns `true` when the scope layer should be passthrough (no
    /// project scope or policy is None without a project).
    pub fn is_passthrough(&self) -> bool {
        self.project_id.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_binding_has_no_project() {
        let b = ScopeBinding::passthrough("persona-1");
        assert!(b.is_passthrough());
        assert_eq!(b.persona_id, "persona-1");
        assert_eq!(b.policy, IsolatePolicy::None);
    }

    #[test]
    fn with_project_binding_is_not_passthrough() {
        let b = ScopeBinding::with_project("persona-1", "project-1", IsolatePolicy::CoreOnly);
        assert!(!b.is_passthrough());
        assert_eq!(b.project_id.as_deref(), Some("project-1"));
        assert_eq!(b.policy, IsolatePolicy::CoreOnly);
    }
}
