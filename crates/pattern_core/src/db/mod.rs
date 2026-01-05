//! V2 Database layer using SQLite via pattern_db
//!
//! This module re-exports pattern_db types for convenient access.
//! Use pattern_db::queries directly for database operations.
//!
//! Complex operations that combine multiple queries or add domain
//! logic should be added here as helpers.

mod combined;

pub use combined::ConstellationDatabases;
pub use pattern_db::models;
pub use pattern_db::queries;
pub use pattern_db::{ConstellationDb, DbError, DbResult};
