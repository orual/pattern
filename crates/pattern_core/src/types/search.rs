//! Search scope types for cross-agent and constellation-wide search.
//!
//! [`SearchScope`] determines the set of agents whose data a search
//! operation considers. The permission resolver in
//! `pattern_runtime::sdk::handlers::scope` validates that the caller
//! actually has permission to access each requested agent's data.

use crate::types::{ids::AgentId, memory_types::BlockSchemaKind};

/// Scope for search operations — determines what data is searched.
///
/// Ported from v2's `SearchScope` (`tool_context.rs`). The runtime's
/// scope resolver maps each variant to a concrete `Vec<AgentId>` after
/// permission checks.
///
/// `#[non_exhaustive]` allows adding new scope variants in future phases
/// without a breaking change for external match sites.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SearchScope {
    /// Search only the current agent's data (always allowed).
    #[default]
    CurrentAgent,
    /// Search a specific agent's data (requires permission).
    Agent(AgentId),
    /// Search multiple agents' data (requires permission for each).
    Agents(Vec<AgentId>),
    /// Search all data in the constellation (requires broad permission).
    Constellation,
    /// Restrict results to blocks whose schema matches the given kind.
    ///
    /// Added in Phase 2 (v3-task-skill-blocks) to support schema-scoped
    /// filtering in Phase 5's skill search, avoiding post-filtering over
    /// the full result set.
    Schema(BlockSchemaKind),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_scope_schema_task_list_is_constructible() {
        let scope = SearchScope::Schema(BlockSchemaKind::TaskList);
        assert_eq!(scope, SearchScope::Schema(BlockSchemaKind::TaskList));
    }

    #[test]
    fn search_scope_schema_text_is_constructible() {
        let scope = SearchScope::Schema(BlockSchemaKind::Text);
        assert_eq!(scope, SearchScope::Schema(BlockSchemaKind::Text));
    }

    #[test]
    fn search_scope_default_is_current_agent() {
        assert_eq!(SearchScope::default(), SearchScope::CurrentAgent);
    }

    #[test]
    fn search_scope_schema_task_list_serde_round_trips() {
        // SearchScope itself is not serde, but BlockSchemaKind inside it is.
        // Verify the inner kind round-trips correctly when used in this context.
        let kind = BlockSchemaKind::TaskList;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""task-list""#);
        let recovered: BlockSchemaKind = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered, kind);
    }

    #[test]
    fn search_scope_schema_log_serde_round_trips_inner() {
        // Confirm BlockSchemaKind::Log, used in a Schema variant, round-trips.
        let kind = BlockSchemaKind::Log;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""log""#);
        let recovered: BlockSchemaKind = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered, kind);
    }
}
