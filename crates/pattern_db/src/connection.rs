//! Database connection management.

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tracing::{debug, info};

use crate::error::DbResult;

/// Connection to a constellation's database.
///
/// Each constellation has its own SQLite database file, providing physical
/// isolation between constellations.
#[derive(Debug, Clone)]
pub struct ConstellationDb {
    pool: SqlitePool,
}

impl ConstellationDb {
    /// Open or create a constellation database at the given path.
    ///
    /// This will:
    /// 1. Register sqlite-vec extension globally (if not already done)
    /// 2. Create the database file if it doesn't exist
    /// 3. Run any pending migrations
    /// 4. Configure SQLite for optimal performance (WAL mode, etc.)
    pub async fn open(path: impl AsRef<Path>) -> DbResult<Self> {
        // Register sqlite-vec before any connections are created.
        // This is idempotent - safe to call multiple times.
        crate::vector::init_sqlite_vec();

        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let path_str = path.to_string_lossy();
        info!("Opening constellation database: {}", path_str);

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // Recommended SQLite pragmas for performance
            .pragma("cache_size", "-64000") // 64MB cache
            .pragma("synchronous", "NORMAL") // Safe with WAL
            .pragma("temp_store", "MEMORY")
            .pragma("mmap_size", "268435456") // 256MB mmap
            .pragma("foreign_keys", "ON");

        let pool = SqlitePoolOptions::new()
            .max_connections(5) // SQLite is single-writer, but readers can parallelize
            .connect_with(options)
            .await?;

        debug!("Database connection established");

        // Run migrations
        Self::run_migrations(&pool).await?;

        Ok(Self { pool })
    }

    /// Open an in-memory database (for testing).
    pub async fn open_in_memory() -> DbResult<Self> {
        // Register sqlite-vec before any connections are created.
        crate::vector::init_sqlite_vec();

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
    async fn run_migrations(pool: &SqlitePool) -> DbResult<()> {
        debug!("Running database migrations");
        sqlx::migrate!("./migrations").run(pool).await?;
        info!("Database migrations complete");
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
    pub async fn health_check(&self) -> DbResult<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Get database statistics.
    pub async fn stats(&self) -> DbResult<DbStats> {
        let agents: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM agents")
            .fetch_one(&self.pool)
            .await?;

        let messages: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
            .fetch_one(&self.pool)
            .await?;

        let memory_blocks: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM memory_blocks")
            .fetch_one(&self.pool)
            .await?;

        Ok(DbStats {
            agent_count: agents.0 as u64,
            message_count: messages.0 as u64,
            memory_block_count: memory_blocks.0 as u64,
        })
    }

    /// Vacuum the database to reclaim space.
    pub async fn vacuum(&self) -> DbResult<()> {
        info!("Vacuuming database");
        sqlx::query("VACUUM").execute(&self.pool).await?;
        Ok(())
    }

    /// Checkpoint the WAL file.
    pub async fn checkpoint(&self) -> DbResult<()> {
        debug!("Checkpointing WAL");
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Database statistics.
#[derive(Debug, Clone)]
pub struct DbStats {
    pub agent_count: u64,
    pub message_count: u64,
    pub memory_block_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_in_memory() {
        let db = ConstellationDb::open_in_memory().await.unwrap();
        db.health_check().await.unwrap();

        let stats = db.stats().await.unwrap();
        assert_eq!(stats.agent_count, 0);
        assert_eq!(stats.message_count, 0);
        assert_eq!(stats.memory_block_count, 0);
    }
}
