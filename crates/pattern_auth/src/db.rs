//! Database connection and operations for auth.db.

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use tracing::{debug, info};

use crate::error::AuthResult;

/// Authentication database handle.
///
/// Manages the SQLite connection pool for auth.db, which stores:
/// - ATProto OAuth sessions
/// - ATProto app-password sessions
/// - Discord bot configuration
/// - Model provider OAuth tokens
#[derive(Debug, Clone)]
pub struct AuthDb {
    pool: SqlitePool,
}

impl AuthDb {
    /// Open or create an auth database at the given path.
    ///
    /// This will:
    /// 1. Create the database file if it doesn't exist
    /// 2. Run any pending migrations
    /// 3. Configure SQLite for optimal performance (WAL mode, etc.)
    pub async fn open(path: impl AsRef<Path>) -> AuthResult<Self> {
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent().filter(|p| !p.exists()) {
            std::fs::create_dir_all(parent)?;
        }

        let path_str = path.to_string_lossy();
        info!("Opening auth database: {}", path_str);

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // Recommended SQLite pragmas for performance
            .pragma("cache_size", "-16000") // 16MB cache (smaller than constellation db)
            .pragma("synchronous", "NORMAL") // Safe with WAL
            .pragma("temp_store", "MEMORY")
            .pragma("foreign_keys", "ON");

        let pool = SqlitePoolOptions::new()
            .max_connections(3) // Auth db has less concurrent access
            .connect_with(options)
            .await?;

        debug!("Auth database connection established");

        // Run migrations
        Self::run_migrations(&pool).await?;

        Ok(Self { pool })
    }

    /// Open an in-memory database (for testing).
    pub async fn open_in_memory() -> AuthResult<Self> {
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .journal_mode(SqliteJournalMode::Wal)
            .pragma("foreign_keys", "ON");

        let pool = SqlitePoolOptions::new()
            .max_connections(1) // In-memory must be single connection to share state
            .connect_with(options)
            .await?;

        Self::run_migrations(&pool).await?;

        Ok(Self { pool })
    }

    /// Run database migrations.
    async fn run_migrations(pool: &SqlitePool) -> AuthResult<()> {
        debug!("Running auth database migrations");
        sqlx::migrate!("./migrations").run(pool).await?;
        info!("Auth database migrations complete");
        Ok(())
    }

    /// Get a reference to the connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Close the database connection.
    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Check if the database is healthy.
    pub async fn health_check(&self) -> AuthResult<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Clean up expired OAuth auth requests.
    ///
    /// Auth requests are transient PKCE state that should be cleaned up
    /// after they expire (~10 minutes after creation).
    pub async fn cleanup_expired_auth_requests(&self) -> AuthResult<u64> {
        let now = chrono::Utc::now().timestamp();
        let result = sqlx::query("DELETE FROM oauth_auth_requests WHERE expires_at < ?")
            .bind(now)
            .execute(&self.pool)
            .await?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            debug!("Cleaned up {} expired auth requests", deleted);
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_in_memory() {
        let db = AuthDb::open_in_memory().await.unwrap();
        db.health_check().await.unwrap();
    }

    #[tokio::test]
    async fn test_cleanup_expired_auth_requests() {
        let db = AuthDb::open_in_memory().await.unwrap();

        // Insert an expired auth request
        let expired_time = chrono::Utc::now().timestamp() - 3600; // 1 hour ago
        sqlx::query(
            r#"
            INSERT INTO oauth_auth_requests
            (state, authserver_url, scopes, request_uri, authserver_token_endpoint,
             pkce_verifier, dpop_key, dpop_nonce, expires_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind("test-state")
        .bind("https://auth.example.com")
        .bind("[]")
        .bind("urn:test:uri")
        .bind("https://auth.example.com/token")
        .bind("test-verifier")
        .bind("{}")
        .bind("test-nonce")
        .bind(expired_time)
        .execute(db.pool())
        .await
        .unwrap();

        // Clean up should delete it
        let deleted = db.cleanup_expired_auth_requests().await.unwrap();
        assert_eq!(deleted, 1);

        // Second cleanup should find nothing
        let deleted = db.cleanup_expired_auth_requests().await.unwrap();
        assert_eq!(deleted, 0);
    }
}
