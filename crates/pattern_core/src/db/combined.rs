//! Combined database wrapper for constellation operations.
//!
//! Provides unified access to both constellation.db (agent state, messages, memory)
//! and auth.db (credentials, tokens) for constellation operations.

use std::path::Path;

use pattern_auth::AuthDb;
use pattern_db::ConstellationDb;

use crate::error::Result;

/// Combined database wrapper providing access to both constellation and auth databases.
///
/// This wrapper simplifies constellation operations by managing both databases together:
/// - `constellation.db` - Agent state, messages, memory blocks (via pattern_db)
/// - `auth.db` - Credentials, OAuth tokens (via pattern_auth)
///
/// # Example
///
/// ```rust,ignore
/// use pattern_core::db::ConstellationDatabases;
///
/// // Open both databases from a directory
/// let dbs = ConstellationDatabases::open("/path/to/constellation").await?;
///
/// // Access individual databases
/// let agents = pattern_db::queries::agent::list_agents(dbs.constellation.pool()).await?;
/// ```
#[derive(Debug, Clone)]
pub struct ConstellationDatabases {
    /// The main constellation database (agent state, messages, memory).
    pub constellation: ConstellationDb,
    /// The authentication database (credentials, tokens).
    pub auth: AuthDb,
}

impl ConstellationDatabases {
    /// Open both databases from a constellation directory.
    ///
    /// This expects the directory to contain (or will create):
    /// - `constellation.db` - Main constellation data
    /// - `auth.db` - Authentication credentials
    ///
    /// # Arguments
    ///
    /// * `constellation_dir` - Path to the constellation directory
    ///
    /// # Errors
    ///
    /// Returns an error if either database fails to open or migrate.
    pub async fn open(constellation_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = constellation_dir.as_ref();

        let constellation_path = dir.join("constellation.db");
        let auth_path = dir.join("auth.db");

        // Note: Individual database open() calls already log their paths
        Self::open_paths(&constellation_path, &auth_path).await
    }

    /// Open both databases with explicit paths.
    ///
    /// Use this when the databases are not in the standard locations.
    ///
    /// # Arguments
    ///
    /// * `constellation_path` - Path to constellation.db
    /// * `auth_path` - Path to auth.db
    ///
    /// # Errors
    ///
    /// Returns an error if either database fails to open or migrate.
    pub async fn open_paths(
        constellation_path: impl AsRef<Path>,
        auth_path: impl AsRef<Path>,
    ) -> Result<Self> {
        let constellation = ConstellationDb::open(constellation_path).await?;
        let auth = AuthDb::open(auth_path).await?;

        Ok(Self {
            constellation,
            auth,
        })
    }

    /// Open both databases in memory for testing.
    ///
    /// Creates ephemeral in-memory databases that are destroyed when dropped.
    /// Useful for unit tests that need database access without file system side effects.
    ///
    /// # Errors
    ///
    /// Returns an error if either database fails to initialize.
    pub async fn open_in_memory() -> Result<Self> {
        let constellation = ConstellationDb::open_in_memory().await?;
        let auth = AuthDb::open_in_memory().await?;

        Ok(Self {
            constellation,
            auth,
        })
    }

    /// Close both database connections.
    ///
    /// This gracefully shuts down both connection pools. After calling this,
    /// the databases should not be used.
    pub async fn close(&self) {
        self.constellation.close().await;
        self.auth.close().await;
    }

    /// Check health of both databases.
    ///
    /// Performs a simple query on each database to verify connectivity.
    ///
    /// # Errors
    ///
    /// Returns an error if either database health check fails.
    pub async fn health_check(&self) -> Result<()> {
        self.constellation.health_check().await?;
        self.auth.health_check().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_in_memory() {
        let dbs = ConstellationDatabases::open_in_memory()
            .await
            .expect("Failed to open in-memory databases");

        // Verify both databases are accessible
        assert!(dbs.constellation.pool().size() > 0);
        assert!(dbs.auth.pool().size() > 0);
    }

    #[tokio::test]
    async fn test_health_check() {
        let dbs = ConstellationDatabases::open_in_memory()
            .await
            .expect("Failed to open in-memory databases");

        dbs.health_check()
            .await
            .expect("Health check should pass for fresh databases");
    }

    #[tokio::test]
    async fn test_close() {
        let dbs = ConstellationDatabases::open_in_memory()
            .await
            .expect("Failed to open in-memory databases");

        dbs.close().await;

        // After close, pools should be closed
        assert!(dbs.constellation.pool().is_closed());
        assert!(dbs.auth.pool().is_closed());
    }
}
