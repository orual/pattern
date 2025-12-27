// TODO: CLI Refactoring for pattern_db (SQLite/sqlx) migration
//!
//! Database inspection and query commands
//!
//! This module provides direct database access for debugging and
//! inspection purposes.
//!
//! ## Migration Status
//!
//! ALL FUNCTIONS in this module are STUBBED during the pattern_db migration.
//! These commands were SurrealDB-specific and will need reimplementation
//! for SQLite/sqlx.
//!
//! ## Previous SurrealDB Usage
//!
//! The module previously used:
//! - `pattern_core::db_v1::client::DB` for database access
//! - Raw SurrealQL queries
//! - `surrealdb::Value` for result handling
//! - Custom unwrap_surrealdb_value() for type descriptor handling
//!
//! ## Future Implementation
//!
//! With pattern_db (SQLite), we'll need:
//! - Raw SQL query execution
//! - Result formatting for terminal display
//! - Stats queries for entity counts

use miette::Result;

use pattern_core::config::PatternConfig;

use crate::output::Output;

// =============================================================================
// Database Stats - STUBBED
// =============================================================================

// TODO: Reimplement for pattern_db (SQLite/sqlx)
//
// Previous implementation:
// 1. Ran COUNT queries for agents, messages, memories, tool calls
// 2. Queried most active agents by message count
// 3. Displayed database type and file path
// 4. Showed file size for embedded databases
//
// Needs: pattern_db stats queries

/// Show database statistics
///
/// NOTE: Currently STUBBED. Needs pattern_db stats queries.
pub async fn stats(_config: &PatternConfig, output: &Output) -> Result<()> {
    output.warning("Database stats temporarily disabled during database migration");
    output.info("Reason:", "Needs pattern_db stats queries");
    output.status("Previous functionality:");
    output.list_item("Entity counts (agents, messages, memories, tool calls)");
    output.list_item("Most active agents by message count");
    output.list_item("Database type and file path");
    output.list_item("Database file size");

    // TODO: Implement pattern_db stats
    //
    // Example pattern_db queries:
    // SELECT COUNT(*) FROM agents
    // SELECT COUNT(*) FROM messages
    // SELECT COUNT(*) FROM memory_blocks
    // SELECT name, message_count FROM agents ORDER BY message_count DESC LIMIT 5

    Ok(())
}

// =============================================================================
// Raw Query - STUBBED
// =============================================================================

// TODO: Reimplement for pattern_db (SQLite/sqlx)
//
// Previous implementation:
// 1. Executed raw SurrealQL query via DB.query(sql)
// 2. Processed each statement result
// 3. Converted surrealdb::Value to JSON
// 4. Used unwrap_surrealdb_value() to clean type descriptors
// 5. Pretty-printed results
//
// Needs: pattern_db raw query execution

/// Run a raw SQL query
///
/// NOTE: Currently STUBBED. Needs pattern_db raw query support.
pub async fn query(sql: &str, output: &Output) -> Result<()> {
    output.warning("Raw query temporarily disabled during database migration");
    output.info("Query:", sql);
    output.info("Reason:", "Pattern_db uses SQLite, not SurrealDB");
    output.status("Previous functionality:");
    output.list_item("Execute raw SurrealQL queries");
    output.list_item("Display results in JSON format");
    output.list_item("Multiple statement support");

    output.section("Migration Notes");
    output.status("Pattern_db uses SQLite with sqlx.");
    output.status("Raw queries will use standard SQL syntax.");
    output.status("SurrealQL features (RELATE, graph traversal) not available.");

    Ok(())
}

// =============================================================================
// Helper Functions - Removed
// =============================================================================

// The following helper was SurrealDB-specific and is no longer needed:
//
// unwrap_surrealdb_value() - Recursively unwrapped SurrealDB type descriptors
//   from JSON values (Array, Object, Strand, Number, Thing, etc.)
//
// SQLite/sqlx returns standard types that don't need this unwrapping.
