//! Per-connection initialization hook for r2d2 pooled connections.
//!
//! Every connection obtained from the pool (and every dedicated connection)
//! runs [`init_connection`] to set pragmas and attach the messages database.
//! sqlite-vec is registered process-globally in [`super::ConstellationDb::open`],
//! so it is available on all connections without per-connection work.

use std::path::Path;

use rusqlite::Connection;

/// Initialize a connection with performance pragmas and attach messages.db.
///
/// Called by `r2d2_sqlite::SqliteConnectionManager::with_init` for every
/// pooled connection, and manually for dedicated connections. Used for
/// file-based databases.
pub(crate) fn init_connection(conn: &mut Connection, messages_path: &Path) -> rusqlite::Result<()> {
    // Performance and correctness pragmas on the main (memory) database.
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA busy_timeout = 5000;
        PRAGMA cache_size = -65536;
        PRAGMA mmap_size = 268435456;
        PRAGMA temp_store = MEMORY;
        PRAGMA synchronous = NORMAL;
        ",
    )?;

    // Attach messages.db as the `msg` schema. SQLite auto-creates the file
    // if it does not exist.
    conn.execute(
        "ATTACH DATABASE ?1 AS msg",
        rusqlite::params![messages_path.to_string_lossy().as_ref()],
    )?;

    // Apply pragmas to the attached messages database.
    conn.execute_batch(
        "
        PRAGMA msg.journal_mode = WAL;
        PRAGMA msg.foreign_keys = ON;
        PRAGMA msg.synchronous = NORMAL;
        ",
    )?;

    Ok(())
}

/// Initialize an in-memory connection with pragmas and attach a shared-cache
/// messages URI as `msg`.
///
/// Used for test databases where both memory and messages live in shared-cache
/// in-memory URIs. The pragmas are the same as production minus WAL/mmap
/// (irrelevant for in-memory databases).
pub(crate) fn init_connection_in_memory(
    conn: &mut Connection,
    msg_uri: &str,
) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        PRAGMA cache_size = -65536;
        PRAGMA temp_store = MEMORY;
        ",
    )?;

    // Attach the messages shared-cache URI as `msg`.
    conn.execute("ATTACH DATABASE ?1 AS msg", rusqlite::params![msg_uri])?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_sets_expected_pragmas() {
        let tmp = TempDir::new().unwrap();
        let mem_path = tmp.path().join("memory.db");
        let msg_path = tmp.path().join("messages.db");

        let mut conn = Connection::open(&mem_path).unwrap();
        init_connection(&mut conn, &msg_path).unwrap();

        // Check main database pragmas.
        let journal_mode: String = conn
            .query_row("PRAGMA main.journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");

        let fk: i64 = conn
            .query_row("PRAGMA main.foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);

        // Check msg database pragmas.
        let msg_journal: String = conn
            .query_row("PRAGMA msg.journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(msg_journal.to_lowercase(), "wal");
    }

    #[test]
    fn init_creates_messages_db_file() {
        let tmp = TempDir::new().unwrap();
        let mem_path = tmp.path().join("memory.db");
        let msg_path = tmp.path().join("messages.db");

        assert!(!msg_path.exists());

        let mut conn = Connection::open(&mem_path).unwrap();
        init_connection(&mut conn, &msg_path).unwrap();

        // ATTACH auto-creates the file, but it may remain empty until
        // a write occurs. Force a write so the file materializes.
        conn.execute("CREATE TABLE IF NOT EXISTS msg._init_check (x INTEGER)", [])
            .unwrap();
        assert!(msg_path.exists());

        // Clean up the temp table.
        conn.execute("DROP TABLE IF EXISTS msg._init_check", [])
            .unwrap();
    }
}
