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
        M::up(include_str!(
            "../migrations/memory/0011_task_block_index.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0012_skill_usage_stats.sql"
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
    fn skill_usage_stats_migration_applies_clean() {
        // Verify migration 0012 creates the skill_usage_stats table with the
        // expected schema. Tests that the WITHOUT ROWID table is created and
        // that basic upsert semantics work on a fresh database.
        let mut conn = Connection::open_in_memory().unwrap();
        run_memory_migrations(&mut conn).unwrap();

        // Table must exist.
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            tables.contains(&"skill_usage_stats".to_string()),
            "skill_usage_stats table must exist after migrations; got {tables:?}"
        );

        // Smoke-test: insert and read back.
        conn.execute(
            "INSERT INTO skill_usage_stats (block_handle, last_used, last_used_by, use_count)
             VALUES ('test-skill', '2026-04-24T12:00:00Z', 'agent-a', 1)",
            [],
        )
        .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT use_count FROM skill_usage_stats WHERE block_handle = 'test-skill'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Verify ON CONFLICT upsert increments the counter.
        conn.execute(
            "INSERT INTO skill_usage_stats (block_handle, last_used, last_used_by, use_count)
             VALUES ('test-skill', '2026-04-24T13:00:00Z', 'agent-b', 1)
             ON CONFLICT(block_handle) DO UPDATE
             SET last_used    = excluded.last_used,
                 last_used_by = excluded.last_used_by,
                 use_count    = skill_usage_stats.use_count + 1",
            [],
        )
        .unwrap();

        let (count2, last_by): (i64, String) = conn
            .query_row(
                "SELECT use_count, last_used_by FROM skill_usage_stats WHERE block_handle = 'test-skill'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count2, 2, "use_count should be 2 after upsert");
        assert_eq!(
            last_by, "agent-b",
            "last_used_by should be the latest agent"
        );
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
