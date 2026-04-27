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
        M::up(include_str!("../migrations/memory/0013_fronting.sql")),
        M::up(include_str!(
            "../migrations/memory/0014_agents_extend.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0015_persona_relationships.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0016_drop_legacy_coordination.sql"
        )),
        M::up(include_str!(
            "../migrations/memory/0017_persona_status.sql"
        )),
    ])
});

// ---------------------------------------------------------------------------
// Messages database migrations (msg schema)
// ---------------------------------------------------------------------------

static MESSAGES_MIGRATIONS: LazyLock<Migrations<'static>> = LazyLock::new(|| {
    Migrations::new(vec![
        M::up(include_str!(
            "../migrations/messages/0001_messages_init.sql"
        )),
        M::up(include_str!(
            "../migrations/messages/0002_message_attachments.sql"
        )),
    ])
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
    fn agents_extended_columns_exist_after_migration() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_memory_migrations(&mut conn).unwrap();

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(agents)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert!(cols.contains(&"config_path".to_string()), "missing config_path; cols = {cols:?}");
        assert!(
            cols.contains(&"project_attachments".to_string()),
            "missing project_attachments; cols = {cols:?}"
        );

        // Default value applies on insert without an explicit project_attachments.
        conn.execute(
            "INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config, enabled_tools, status, created_at, updated_at)
             VALUES ('p1', 'persona-one', 'anthropic', 'claude-sonnet-4-6', 'sys', '{}', '[]', 'active', '2026-04-26T00:00:00Z', '2026-04-26T00:00:00Z')",
            [],
        )
        .unwrap();

        let pa: String = conn
            .query_row(
                "SELECT project_attachments FROM agents WHERE id = 'p1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pa, "[]", "project_attachments default should be empty JSON array");
    }

    #[test]
    fn persona_relationships_unique_constraint_dedupes_edges() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_memory_migrations(&mut conn).unwrap();

        // Seed two personas to satisfy the FK.
        for id in ["alice", "bob"] {
            conn.execute(
                "INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config, enabled_tools, status, created_at, updated_at)
                 VALUES (?, ?, 'anthropic', 'claude-sonnet-4-6', 'sys', '{}', '[]', 'active', '2026-04-26T00:00:00Z', '2026-04-26T00:00:00Z')",
                rusqlite::params![id, id],
            ).unwrap();
        }

        conn.execute(
            "INSERT INTO persona_relationships (id, from_persona, to_persona, kind, created_at)
             VALUES ('e1', 'alice', 'bob', 'supervisor_of', '2026-04-26T00:00:00Z')",
            [],
        )
        .unwrap();

        // Duplicate edge with same (from, to, kind) must be rejected.
        let dup = conn.execute(
            "INSERT INTO persona_relationships (id, from_persona, to_persona, kind, created_at)
             VALUES ('e2', 'alice', 'bob', 'supervisor_of', '2026-04-26T00:00:00Z')",
            [],
        );
        assert!(dup.is_err(), "UNIQUE(from_persona, to_persona, kind) must reject duplicate edge");

        // Different `kind` between the same pair is allowed.
        conn.execute(
            "INSERT INTO persona_relationships (id, from_persona, to_persona, kind, created_at)
             VALUES ('e3', 'alice', 'bob', 'peer_with', '2026-04-26T00:00:00Z')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn persona_relationships_cascade_delete_on_persona_drop() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_memory_migrations(&mut conn).unwrap();
        // Cascade requires foreign_keys ON.
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();

        for id in ["alice", "bob"] {
            conn.execute(
                "INSERT INTO agents (id, name, model_provider, model_name, system_prompt, config, enabled_tools, status, created_at, updated_at)
                 VALUES (?, ?, 'anthropic', 'claude-sonnet-4-6', 'sys', '{}', '[]', 'active', '2026-04-26T00:00:00Z', '2026-04-26T00:00:00Z')",
                rusqlite::params![id, id],
            ).unwrap();
        }
        conn.execute(
            "INSERT INTO persona_relationships (id, from_persona, to_persona, kind, created_at)
             VALUES ('e1', 'alice', 'bob', 'supervisor_of', '2026-04-26T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO persona_groups (id, name, created_at) VALUES ('g1', 'core-team', '2026-04-26T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO persona_group_members (group_id, persona_id, joined_at)
             VALUES ('g1', 'alice', '2026-04-26T00:00:00Z')",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM agents WHERE id = 'alice'", []).unwrap();

        let edge_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM persona_relationships WHERE from_persona = 'alice' OR to_persona = 'alice'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(edge_count, 0, "relationship edges should cascade-delete with persona");

        let mem_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM persona_group_members WHERE persona_id = 'alice'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mem_count, 0, "group memberships should cascade-delete with persona");
    }

    #[test]
    fn persona_groups_unique_per_project_scope() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_memory_migrations(&mut conn).unwrap();

        // Same name in different projects is allowed.
        conn.execute(
            "INSERT INTO persona_groups (id, name, project_id, created_at)
             VALUES ('g1', 'reviewers', 'proj-a', '2026-04-26T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO persona_groups (id, name, project_id, created_at)
             VALUES ('g2', 'reviewers', 'proj-b', '2026-04-26T00:00:00Z')",
            [],
        )
        .unwrap();

        // Same name + same project is rejected.
        let dup = conn.execute(
            "INSERT INTO persona_groups (id, name, project_id, created_at)
             VALUES ('g3', 'reviewers', 'proj-a', '2026-04-26T00:00:00Z')",
            [],
        );
        assert!(
            dup.is_err(),
            "UNIQUE(name, project_id) must reject duplicate group within a project"
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
