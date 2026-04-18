//! Search scope types for cross-agent and constellation-wide search.
//!
//! [`SearchScope`] determines the set of agents whose data a search
//! operation considers. The permission resolver in
//! `pattern_runtime::sdk::handlers::scope` validates that the caller
//! actually has permission to access each requested agent's data.

use crate::types::ids::AgentId;

/// Scope for search operations — determines what data is searched.
///
/// Ported from v2's `SearchScope` (`tool_context.rs`). The runtime's
/// scope resolver maps each variant to a concrete `Vec<AgentId>` after
/// permission checks.
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
}
