//! Schema migration runners for memory.db and messages.db.
//!
//! Migrations are embedded at compile time via `include_str!` and applied
//! using `rusqlite_migration`. Each database has its own migration sequence.

use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Memory database migrations (main schema)
// ---------------------------------------------------------------------------

static MEMORY_MIGRATIONS: LazyLock<Migrations<'static>> = LazyLock::new(|| {
    Migrations::new(vec![
        M::up(include_str!("../migrations/memory/0001_initial.sql")),
        M::up(include_str!("../migrations/memory/0002_fts5.sql")),
        M::up(include_str!("../migrations/memory/0003_model_fields.sql")),
        M::up(include_str!("../migrations/memory/0004_memory_updates.sql")),
        M::up(include_str!(
            "../migrations/memory/0005_archival_fts_metadata.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0006_agent_atproto_endpoints.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0007_add_session_id_to_atproto_endpoints.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0008_member_capabilities.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0009_update_frontiers.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0010_collapse_block_types.sql"
        )),
    ])
});

// ---------------------------------------------------------------------------
// Messages database migrations (msg schema)
// ---------------------------------------------------------------------------

static MESSAGES_MIGRATIONS: LazyLock<Migrations<'static>> = LazyLock::new(|| {
    Migrations::new(vec![M::up(include_str!(
        "../migrations/messages/0001_messages_init.sql"
    ))])
});

/// Apply all pending memory database migrations.
pub fn run_memory_migrations(conn: &mut Connection) -> Result<(), rusqlite_migration::Error> {
    MEMORY_MIGRATIONS.to_latest(conn)
}

/// Apply all pending messages database migrations on a direct connection.
pub fn run_messages_migrations(conn: &mut Connection) -> Result<(), rusqlite_migration::Error> {
    MESSAGES_MIGRATIONS.to_latest(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_migrations_apply_cleanly() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_memory_migrations(&mut conn).unwrap();

        // Verify key tables exist.
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert!(tables.contains(&"agents".to_string()));
        assert!(tables.contains(&"memory_blocks".to_string()));
        assert!(tables.contains(&"archival_entries".to_string()));
    }

    #[test]
    fn messages_migrations_apply_cleanly() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_messages_migrations(&mut conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert!(tables.contains(&"messages".to_string()));
        assert!(tables.contains(&"queued_messages".to_string()));
    }

    #[test]
    fn memory_migrations_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_memory_migrations(&mut conn).unwrap();
        // Second call should be a no-op.
        run_memory_migrations(&mut conn).unwrap();
    }

    #[test]
    fn messages_migrations_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_messages_migrations(&mut conn).unwrap();
        run_messages_migrations(&mut conn).unwrap();
    }
}
