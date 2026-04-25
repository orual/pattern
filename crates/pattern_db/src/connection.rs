//! Database connection management.
//!
//! [`ConstellationDb`] wraps an `r2d2::Pool<SqliteConnectionManager>` with
//! per-connection initialization (pragmas + ATTACH messages.db) and
//! process-global sqlite-vec registration.

mod init;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

use crate::error::{DbError, DbResult};

/// Connection to a constellation's paired databases (memory.db + messages.db).
///
/// Each constellation has its own SQLite database files, providing physical
/// isolation between constellations. The pool hands out connections that
/// have `messages.db` attached as the `msg` schema.
#[derive(Debug, Clone)]
pub struct ConstellationDb {
    pool: Pool<SqliteConnectionManager>,
    memory_path: PathBuf,
    messages_path: PathBuf,
}

impl ConstellationDb {
    /// Open or create constellation databases at the given paths.
    ///
    /// This will:
    /// 1. Register sqlite-vec extension globally (idempotent).
    /// 2. Run memory migrations on `memory_path`.
    /// 3. Run messages migrations on `messages_path`.
    /// 4. Build an r2d2 pool where every connection sets pragmas and
    ///    ATTACHes messages.db as `msg`.
    pub fn open(
        memory_path: impl Into<PathBuf>,
        messages_path: impl Into<PathBuf>,
    ) -> DbResult<Self> {
        let memory_path = memory_path.into();
        let messages_path = messages_path.into();

        // Ensure parent directories exist.
        for p in [&memory_path, &messages_path] {
            if let Some(parent) = p.parent()
                && !parent.exists()
            {
                std::fs::create_dir_all(parent)?;
            }
        }

        info!(
            "opening constellation databases: memory={}, messages={}",
            memory_path.display(),
            messages_path.display()
        );

        // Process-global sqlite-vec registration. After this call every
        // subsequently-opened connection auto-loads sqlite-vec.
        // Idempotent: repeated calls are no-ops.
        register_sqlite_vec();

        // Run migrations on temporary direct connections (not from the pool)
        // so the pool's init_connection can assume tables already exist.
        Self::run_migrations(&memory_path, &messages_path)?;

        // Build the pool.
        let pool = Self::build_pool(&memory_path, &messages_path)?;
        debug!("connection pool built (max_size=10)");

        Ok(Self {
            pool,
            memory_path,
            messages_path,
        })
    }

    /// Open an in-memory constellation database (for testing).
    ///
    /// Uses shared-cache URIs so multiple pool connections share the same
    /// in-memory databases. Messages are ATTACHed as `msg` identically to
    /// production, so all SQL uses `msg.messages` uniformly.
    pub fn open_in_memory() -> DbResult<Self> {
        use rusqlite::OpenFlags;

        register_sqlite_vec();

        // Generate unique shared-cache URIs so each ConstellationDb instance
        // gets its own isolated pair of in-memory databases.
        let mem_uri = format!(
            "file:mem_{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4().simple()
        );
        let msg_uri = format!(
            "file:msg_{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4().simple()
        );

        let uri_flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;

        // Open msg DB and run migrations FIRST, before the pool ATTACHes
        // it. This ensures the pool's eager-init connections see the
        // already-migrated schema; otherwise their ATTACH cache holds the
        // pre-migration column list and DDL applied on a separate
        // connection won't propagate to those statements.
        //
        // We hold `msg_conn` alive across pool construction so the
        // shared-cache in-memory msg DB doesn't vanish before the pool's
        // first ATTACH bumps the refcount.
        let mut msg_conn = Connection::open_with_flags(&msg_uri, uri_flags)?;
        crate::migrations::run_messages_migrations(&mut msg_conn)?;

        // Build pool. The pool eagerly opens min_idle connections, which
        // keeps the shared-cache in-memory databases alive.
        let msg_uri_owned = msg_uri.clone();
        let manager = SqliteConnectionManager::file(&mem_uri)
            .with_flags(uri_flags)
            .with_init(move |conn| init::init_connection_in_memory(conn, &msg_uri_owned));

        let pool = Pool::builder()
            .max_size(4)
            .min_idle(Some(1))
            .build(manager)
            .map_err(DbError::Pool)?;

        // Run memory migrations on a pool connection.
        {
            let mut conn = pool.get().map_err(DbError::Pool)?;
            crate::migrations::run_memory_migrations(&mut conn)?;
        }

        // Now safe to drop the temporary msg connection — the pool's
        // ATTACHed msg DB keeps the shared-cache in-memory database alive.
        drop(msg_conn);

        debug!("in-memory constellation database opened (shared-cache URIs)");

        Ok(Self {
            pool,
            memory_path: PathBuf::from(&mem_uri),
            messages_path: PathBuf::from(&msg_uri),
        })
    }

    /// Get a pooled connection.
    ///
    /// The returned connection has pragmas set and messages.db attached
    /// (unless in-memory mode).
    pub fn get(&self) -> DbResult<r2d2::PooledConnection<SqliteConnectionManager>> {
        self.pool.get().map_err(DbError::Pool)
    }

    /// Open a fresh non-pool connection with the same init_connection hook.
    ///
    /// Used by the eval worker for its session lifetime (Phase 3).
    /// For file-based databases, uses `init_connection`. For shared-cache
    /// in-memory URIs, uses `init_connection_in_memory`.
    pub fn dedicated_connection(&self) -> DbResult<Connection> {
        let mem_path_str = self.memory_path.to_string_lossy();
        let is_uri = mem_path_str.starts_with("file:");

        if is_uri {
            // Shared-cache in-memory URI: open with URI flags and attach msg URI.
            let uri_flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_URI;
            let mut conn = Connection::open_with_flags(&self.memory_path, uri_flags)?;
            let msg_uri = self.messages_path.to_string_lossy();
            init::init_connection_in_memory(&mut conn, &msg_uri)?;
            Ok(conn)
        } else {
            let mut conn = Connection::open(&self.memory_path)?;
            init::init_connection(&mut conn, &self.messages_path)?;
            Ok(conn)
        }
    }

    /// Path to the memory database file.
    pub fn memory_path(&self) -> &Path {
        &self.memory_path
    }

    /// Path to the messages database file.
    pub fn messages_path(&self) -> &Path {
        &self.messages_path
    }

    /// Get database statistics.
    pub fn stats(&self) -> DbResult<crate::queries::stats::DbStats> {
        let conn = self.get()?;
        crate::queries::stats::get_stats(&conn)
    }

    /// Check if the database is healthy.
    pub fn health_check(&self) -> DbResult<()> {
        let conn = self.get()?;
        conn.query_row("SELECT 1", [], |_| Ok(()))?;
        Ok(())
    }

    /// Vacuum the database to reclaim space.
    pub fn vacuum(&self) -> DbResult<()> {
        info!("vacuuming database");
        let conn = self.get()?;
        conn.execute_batch("VACUUM")?;
        Ok(())
    }

    /// Checkpoint the WAL file.
    pub fn checkpoint(&self) -> DbResult<()> {
        debug!("checkpointing WAL");
        let conn = self.get()?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }

    /// Run migrations on both databases using temporary direct connections.
    fn run_migrations(memory_path: &Path, messages_path: &Path) -> DbResult<()> {
        debug!("running memory migrations");
        {
            let mut mem_conn = Connection::open(memory_path)?;
            crate::migrations::run_memory_migrations(&mut mem_conn)?;
        }
        debug!("running messages migrations");
        {
            let mut msg_conn = Connection::open(messages_path)?;
            crate::migrations::run_messages_migrations(&mut msg_conn)?;
        }
        info!("database migrations complete");
        Ok(())
    }

    /// Build the r2d2 pool with per-connection init hook.
    fn build_pool(
        memory_path: &Path,
        messages_path: &Path,
    ) -> DbResult<Pool<SqliteConnectionManager>> {
        let messages_path_owned = messages_path.to_path_buf();
        let manager = SqliteConnectionManager::file(memory_path)
            .with_init(move |conn| init::init_connection(conn, &messages_path_owned));

        Pool::builder()
            .max_size(10)
            .min_idle(Some(2))
            .connection_timeout(std::time::Duration::from_secs(30))
            .build(manager)
            .map_err(DbError::Pool)
    }
}

/// Register sqlite-vec as a process-global auto-extension.
///
/// After this call, every subsequently-opened SQLite connection
/// automatically has sqlite-vec available. Idempotent.
fn register_sqlite_vec() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        unsafe {
            let init_fn = sqlite_vec::sqlite3_vec_init as *const ();
            // Safety: sqlite3_vec_init matches the auto-extension function signature.
            // The transmute converts from *const () to the C callback type expected
            // by sqlite3_auto_extension.
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut i8,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(init_fn)));
        }
        tracing::debug!("sqlite-vec extension registered globally");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_on_fresh_temp_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem_path = tmp.path().join("memory.db");
        let msg_path = tmp.path().join("messages.db");

        let db = ConstellationDb::open(&mem_path, &msg_path).unwrap();
        db.health_check().unwrap();

        // Both files should exist.
        assert!(mem_path.exists());
        assert!(msg_path.exists());

        let stats = db.stats().unwrap();
        assert_eq!(stats.agent_count, 0);
        assert_eq!(stats.memory_block_count, 0);
    }

    #[test]
    fn test_open_in_memory() {
        let db = ConstellationDb::open_in_memory().unwrap();
        db.health_check().unwrap();

        let stats = db.stats().unwrap();
        assert_eq!(stats.agent_count, 0);
        assert_eq!(stats.message_count, 0);
        assert_eq!(stats.memory_block_count, 0);
    }

    #[test]
    fn test_dedicated_connection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem_path = tmp.path().join("memory.db");
        let msg_path = tmp.path().join("messages.db");

        let db = ConstellationDb::open(&mem_path, &msg_path).unwrap();
        let conn = db.dedicated_connection().unwrap();

        // Should be able to query both schemas.
        let result: i64 = conn.query_row("SELECT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(result, 1);
    }

    #[test]
    fn test_creates_parent_directories() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("a/b/c");
        let mem_path = nested.join("memory.db");
        let msg_path = nested.join("messages.db");

        let db = ConstellationDb::open(&mem_path, &msg_path).unwrap();
        db.health_check().unwrap();
        assert!(nested.exists());
    }
}
