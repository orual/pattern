//! Shared helper functions for CLI commands
//!
//! This module consolidates common patterns used across CLI commands to reduce
//! code duplication and ensure consistent behavior.
//!
//! ## Common Helpers
//!
//! - `get_db()` - Opens database connection from config
//! - `load_config()` - Loads config from standard locations
//! - `get_agent_by_name()` - Finds and returns an agent by name
//! - `require_agent_by_name()` - Like get_agent_by_name but returns error if not found
//! - `get_group_by_name()` - Finds and returns a group by name
//! - `require_group_by_name()` - Like get_group_by_name but returns error if not found
//! - `create_runtime_context()` - Creates a RuntimeContext with model provider

use miette::Result;
use pattern_core::config::PatternConfig;
use pattern_core::db::ConstellationDatabases;
use pattern_core::embeddings::cloud::OpenAIEmbedder;
use pattern_core::model::GenAiClient;
use pattern_core::runtime::RuntimeContext;
use pattern_db::ConstellationDb;
use pattern_db::models::{Agent, AgentGroup as DbAgentGroup};
use std::sync::Arc;

// =============================================================================
// Re-exports
// =============================================================================

// Re-export types used by modules that import helpers
pub type DbAgent = Agent;
pub type AgentGroup = DbAgentGroup;

// =============================================================================
// Database Connection Helpers
// =============================================================================

/// Open combined database connections from the config
///
/// This is the canonical way to open database connections in CLI commands.
/// Opens both constellation.db and auth.db from the data directory.
/// All commands that need database access should use this helper.
pub async fn get_dbs(config: &PatternConfig) -> Result<ConstellationDatabases> {
    // The config database.path points to constellation.db, we need the parent directory
    let data_dir = config
        .database
        .path
        .parent()
        .ok_or_else(|| miette::miette!("Invalid database path: no parent directory"))?;

    ConstellationDatabases::open(data_dir)
        .await
        .map_err(|e| miette::miette!("Failed to open databases: {}", e))
}

/// Open just the constellation database connection from the config
///
/// Use this when you only need constellation data (agents, messages, memory)
/// and don't need auth tokens. For full functionality, prefer `get_dbs()`.
pub async fn get_db(config: &PatternConfig) -> Result<ConstellationDb> {
    ConstellationDb::open(&config.database.path)
        .await
        .map_err(|e| miette::miette!("Failed to open database: {}", e))
}

/// Load configuration from standard locations
///
/// Used by commands that don't receive config as a parameter.
/// Searches in standard locations (./pattern.toml, ~/.config/pattern/, etc.)
pub async fn load_config() -> Result<PatternConfig> {
    pattern_core::config::load_config_from_standard_locations()
        .await
        .map_err(|e| miette::miette!("Failed to load config: {}", e))
}

// =============================================================================
// Agent Lookup Helpers
// =============================================================================

/// Find an agent by name, returning None if not found
///
/// Use this when the agent might not exist and that's okay.
/// For cases where the agent must exist, use `require_agent_by_name()`.
pub async fn get_agent_by_name(db: &ConstellationDb, name: &str) -> Result<Option<DbAgent>> {
    pattern_db::queries::get_agent_by_name(db.pool(), name)
        .await
        .map_err(|e| miette::miette!("Failed to query agent '{}': {}", name, e))
}

/// Find an agent by name, returning an error if not found
///
/// Use this when the agent must exist for the operation to proceed.
/// Returns a user-friendly error message if the agent is not found.
pub async fn require_agent_by_name(db: &ConstellationDb, name: &str) -> Result<DbAgent> {
    get_agent_by_name(db, name)
        .await?
        .ok_or_else(|| miette::miette!("Agent '{}' not found", name))
}

/// Find an agent by name using config for database path
///
/// Convenience wrapper that opens the database and looks up the agent.
/// Use this in commands that only need to find an agent.
#[allow(dead_code)]
pub async fn find_agent_by_name(config: &PatternConfig, name: &str) -> Result<Option<DbAgent>> {
    let db = get_db(config).await?;
    get_agent_by_name(&db, name).await
}

/// Find and require an agent by name using config for database path
///
/// Convenience wrapper that opens the database and requires the agent to exist.
#[allow(dead_code)]
pub async fn require_agent(config: &PatternConfig, name: &str) -> Result<DbAgent> {
    let db = get_db(config).await?;
    require_agent_by_name(&db, name).await
}

// =============================================================================
// Group Lookup Helpers
// =============================================================================

/// Find a group by name, returning None if not found
///
/// Use this when the group might not exist and that's okay.
/// For cases where the group must exist, use `require_group_by_name()`.
pub async fn get_group_by_name(db: &ConstellationDb, name: &str) -> Result<Option<AgentGroup>> {
    pattern_db::queries::get_group_by_name(db.pool(), name)
        .await
        .map_err(|e| miette::miette!("Failed to query group '{}': {}", name, e))
}

/// Find a group by name, returning an error if not found
///
/// Use this when the group must exist for the operation to proceed.
/// Returns a user-friendly error message if the group is not found.
pub async fn require_group_by_name(db: &ConstellationDb, name: &str) -> Result<AgentGroup> {
    get_group_by_name(db, name)
        .await?
        .ok_or_else(|| miette::miette!("Group '{}' not found", name))
}

/// Find a group by name using config for database path
///
/// Convenience wrapper that opens the database and looks up the group.
#[allow(dead_code)]
pub async fn find_group_by_name(config: &PatternConfig, name: &str) -> Result<Option<AgentGroup>> {
    let db = get_db(config).await?;
    get_group_by_name(&db, name).await
}

/// Find and require a group by name using config for database path
///
/// Convenience wrapper that opens the database and requires the group to exist.
#[allow(dead_code)]
pub async fn require_group(config: &PatternConfig, name: &str) -> Result<AgentGroup> {
    let db = get_db(config).await?;
    require_group_by_name(&db, name).await
}

// =============================================================================
// RuntimeContext Helpers
// =============================================================================

/// Create a RuntimeContext for agent operations
///
/// This sets up the full runtime context needed for loading and creating agents.
/// The context includes the database connections and model provider.
#[allow(dead_code)]
pub async fn create_runtime_context(config: &PatternConfig) -> Result<RuntimeContext> {
    let dbs = get_dbs(config).await?;

    RuntimeContext::builder()
        .dbs(Arc::new(dbs))
        .build()
        .await
        .map_err(|e| miette::miette!("Failed to create runtime context: {}", e))
}

/// Create a RuntimeContext with pre-opened databases
///
/// Use this when you already have database connections open.
pub async fn create_runtime_context_with_dbs(
    dbs: ConstellationDatabases,
) -> Result<RuntimeContext> {
    // let api_key = std::env::var("OPENAI_API_KEY")
    //     .map_err(|e| miette::miette!("Failed to get OPENAI_API_KEY: {}", e))?;
    // let dimensions = 1536;
    // let embedding_provider = Arc::new(OpenAIEmbedder::new(
    //     "text-embedding-3-small".into(),
    //     api_key,
    //     Some(dimensions),
    // ));

    RuntimeContext::builder()
        .dbs(Arc::new(dbs))
        //.embedding_provider(embedding_provider)
        .build()
        .await
        .map_err(|e| miette::miette!("Failed to create runtime context: {}", e))
}

// =============================================================================
// ID Generation
// =============================================================================

/// Generate a unique ID with the given prefix
///
/// Creates a timestamped hex ID like "agent_1a2b3c4d5e" or "grp_1a2b3c4d5e".
pub fn generate_id(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("{}_{:x}", prefix, now)
}
