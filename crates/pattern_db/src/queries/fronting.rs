//! CRUD queries for `fronting_set` and `routing_rules` (migration 0013).
//!
//! The fronting set uses a singleton row pattern: there is always at most one
//! row in `fronting_set` with `id = "default"`. This keeps the load/save API
//! unconditional — load returns `Option<FrontingSet>` and save always upserts.
//!
//! `routing_rules` is an ON DELETE CASCADE child of `fronting_set`, so
//! `clear_fronting_set` removes both tables' data in one statement.

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};

use pattern_core::fronting::{FrontingSet, RoutingRule, RoutingTable};
use pattern_core::types::ids::PersonaId;

use crate::error::{DbError, DbResult};

/// The singleton row id used in `fronting_set`.
const SINGLETON_ID: &str = "default";

// ── load_fronting_set ─────────────────────────────────────────────────────────

/// Load the persisted `FrontingSet` from the database.
///
/// Returns `Ok(None)` when no fronting set has been saved yet (fresh DB).
/// Routing rules are loaded and compiled; returns an error if any `Regex`
/// pattern fails to compile.
pub fn load_fronting_set(conn: &Connection) -> DbResult<Option<FrontingSet>> {
    // Step 1: load the singleton header row.
    let header: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT active_personas, fallback_persona
             FROM fronting_set
             WHERE id = ?1",
            params![SINGLETON_ID],
            |row| {
                let active: String = row.get(0)?;
                let fallback: Option<String> = row.get(1)?;
                Ok((active, fallback))
            },
        )
        .optional()?;

    let Some((active_json, fallback_str)) = header else {
        return Ok(None);
    };

    // Step 2: deserialize active personas from JSON.
    let active_strs: Vec<String> = serde_json::from_str(&active_json)?;
    let active: Vec<PersonaId> = active_strs
        .iter()
        .map(|s| PersonaId::new(s.as_str()))
        .collect();

    let fallback: Option<PersonaId> = fallback_str.map(|s| PersonaId::new(s.as_str()));

    // Step 3: load routing rules ordered by priority descending (the table
    // has an index for this, but ORDER BY makes the result stable regardless).
    let mut stmt = conn.prepare(
        "SELECT id, pattern, target_persona, priority
         FROM routing_rules
         WHERE set_id = ?1
         ORDER BY priority DESC",
    )?;

    let rules: Vec<RoutingRule> = stmt
        .query_map(params![SINGLETON_ID], |row| {
            let id: String = row.get(0)?;
            let pattern_json: String = row.get(1)?;
            let target: String = row.get(2)?;
            let priority: i64 = row.get(3)?;
            Ok((id, pattern_json, target, priority))
        })?
        .map(|r| {
            let (id, pattern_json, target, priority) = r?;
            let pattern = serde_json::from_str(&pattern_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(RoutingRule::new(
                id,
                pattern,
                PersonaId::new(target.as_str()),
                priority as u32,
            ))
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Step 4: compile regex patterns.
    let routing = RoutingTable::try_from_rules(rules)
        .map_err(|e| DbError::invalid_data(format!("failed to compile routing rules: {e}")))?;

    Ok(Some(FrontingSet::from_parts(active, fallback, routing)))
}

// ── save_fronting_set ─────────────────────────────────────────────────────────

/// Persist `set` to the database, replacing any existing data.
///
/// Runs in a transaction: upserts the `fronting_set` row, deletes all existing
/// rules for this set, then inserts the new rules. Either all changes land or
/// none do.
pub fn save_fronting_set(conn: &mut Connection, set: &FrontingSet) -> DbResult<()> {
    let tx = conn.transaction()?;

    let now = Timestamp::now().to_string();

    // Serialize active persona list.
    let active_strs: Vec<&str> = set.active.iter().map(|id| id.as_str()).collect();
    let active_json = serde_json::to_string(&active_strs)?;

    let fallback_str: Option<&str> = set.fallback.as_deref();

    // Upsert the singleton header row.
    tx.execute(
        "INSERT INTO fronting_set (id, active_personas, fallback_persona, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE
         SET active_personas  = excluded.active_personas,
             fallback_persona = excluded.fallback_persona,
             updated_at       = excluded.updated_at",
        params![SINGLETON_ID, active_json, fallback_str, now],
    )?;

    // Remove existing routing rules for this set.
    tx.execute(
        "DELETE FROM routing_rules WHERE set_id = ?1",
        params![SINGLETON_ID],
    )?;

    // Insert new routing rules.
    for rule in &set.routing.rules {
        let pattern_json = serde_json::to_string(&rule.pattern)?;
        tx.execute(
            "INSERT INTO routing_rules (id, set_id, pattern, target_persona, priority, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                rule.id,
                SINGLETON_ID,
                pattern_json,
                rule.target.as_str(),
                rule.priority as i64,
                now,
            ],
        )?;
    }

    tx.commit()?;
    Ok(())
}

// ── clear_fronting_set ────────────────────────────────────────────────────────

/// Remove the fronting set and all its routing rules from the database.
///
/// Deleting the singleton row cascades to `routing_rules` via the FK
/// constraint. After this call, `load_fronting_set` returns `Ok(None)`.
pub fn clear_fronting_set(conn: &mut Connection) -> DbResult<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM fronting_set WHERE id = ?1",
        params![SINGLETON_ID],
    )?;
    tx.commit()?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use pattern_core::fronting::{MessagePattern, RoutingRule, RoutingTable};
    use pattern_core::types::ids::PersonaId;

    use super::*;
    use crate::migrations::run_memory_migrations;

    fn setup_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        run_memory_migrations(&mut conn).unwrap();
        conn
    }

    fn make_set_with_rules() -> FrontingSet {
        let rules = vec![
            RoutingRule::new(
                "rule-math",
                MessagePattern::Prefix("!math".to_string()),
                PersonaId::new("math-specialist"),
                10,
            ),
            RoutingRule::new(
                "rule-chat",
                MessagePattern::Contains("chat".to_string()),
                PersonaId::new("chat-specialist"),
                5,
            ),
        ];

        FrontingSet::from_parts(
            vec![PersonaId::new("alice"), PersonaId::new("bob")],
            Some(PersonaId::new("alice")),
            RoutingTable::try_from_rules(rules).unwrap(),
        )
    }

    // ── Round-trip: save then load must be deeply equal ───────────────────────

    #[test]
    fn round_trip_save_and_load() {
        let mut conn = setup_db();
        let original = make_set_with_rules();

        save_fronting_set(&mut conn, &original).unwrap();
        let loaded = load_fronting_set(&conn).unwrap().expect("should have data");

        // Active personas.
        assert_eq!(
            loaded.active.len(),
            original.active.len(),
            "active persona count must match"
        );
        for id in &original.active {
            assert!(
                loaded.active.contains(id),
                "active persona {id} must be present"
            );
        }

        // Fallback.
        assert_eq!(loaded.fallback, original.fallback, "fallback must match");

        // Rules.
        assert_eq!(
            loaded.routing.rules.len(),
            original.routing.rules.len(),
            "routing rule count must match"
        );

        let original_ids: Vec<&str> = original
            .routing
            .rules
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        let loaded_ids: Vec<&str> = loaded.routing.rules.iter().map(|r| r.id.as_str()).collect();
        for id in &original_ids {
            assert!(
                loaded_ids.contains(id),
                "rule {id} must be present after load"
            );
        }
    }

    // ── Load returns None on a fresh DB ───────────────────────────────────────

    #[test]
    fn load_returns_none_on_empty_db() {
        let conn = setup_db();
        let result = load_fronting_set(&conn).unwrap();
        assert!(result.is_none(), "fresh DB should return None");
    }

    // ── Save overwrites existing routing rules (not appends) ──────────────────

    #[test]
    fn save_overwrites_routing_rules_not_appends() {
        let mut conn = setup_db();

        // First save: rules A and B.
        let rules_ab = vec![
            RoutingRule::new(
                "rule-a",
                MessagePattern::Prefix("!a".to_string()),
                PersonaId::new("target-a"),
                10,
            ),
            RoutingRule::new(
                "rule-b",
                MessagePattern::Prefix("!b".to_string()),
                PersonaId::new("target-b"),
                5,
            ),
        ];
        let set_ab = FrontingSet::from_parts(
            vec![PersonaId::new("alice")],
            None,
            RoutingTable::try_from_rules(rules_ab).unwrap(),
        );
        save_fronting_set(&mut conn, &set_ab).unwrap();

        // Second save: rule C only.
        let rules_c = vec![RoutingRule::new(
            "rule-c",
            MessagePattern::Contains("c".to_string()),
            PersonaId::new("target-c"),
            1,
        )];
        let set_c = FrontingSet::from_parts(
            vec![PersonaId::new("bob")],
            None,
            RoutingTable::try_from_rules(rules_c).unwrap(),
        );
        save_fronting_set(&mut conn, &set_c).unwrap();

        // Verify only rule C remains.
        let loaded = load_fronting_set(&conn).unwrap().expect("should have data");
        assert_eq!(
            loaded.routing.rules.len(),
            1,
            "only rule-c must survive the second save"
        );
        assert_eq!(loaded.routing.rules[0].id, "rule-c");

        // Also verify the raw table count.
        let rule_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM routing_rules WHERE set_id = 'default'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            rule_count, 1,
            "routing_rules table must contain exactly 1 row after second save"
        );
    }

    // ── clear_fronting_set removes both tables' entries ───────────────────────

    #[test]
    fn clear_removes_both_tables_entries() {
        let mut conn = setup_db();

        save_fronting_set(&mut conn, &make_set_with_rules()).unwrap();

        // Confirm data is present.
        let set_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fronting_set WHERE id = 'default'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(set_count, 1, "fronting_set should have 1 row before clear");

        let rule_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM routing_rules WHERE set_id = 'default'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            rule_count > 0,
            "routing_rules should be non-empty before clear"
        );

        // Clear.
        clear_fronting_set(&mut conn).unwrap();

        // Verify both tables are empty.
        let set_count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fronting_set WHERE id = 'default'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(set_count_after, 0, "fronting_set must be empty after clear");

        let rule_count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM routing_rules WHERE set_id = 'default'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            rule_count_after, 0,
            "routing_rules must be empty after clear (cascade)"
        );

        // load must return None.
        let loaded = load_fronting_set(&conn).unwrap();
        assert!(loaded.is_none(), "load must return None after clear");
    }

    // ── FrontingSet with no active or fallback persists correctly ─────────────

    #[test]
    fn round_trip_empty_fronting_set() {
        let mut conn = setup_db();
        let empty = FrontingSet::default();

        save_fronting_set(&mut conn, &empty).unwrap();
        let loaded = load_fronting_set(&conn).unwrap().expect("should have data");

        assert!(loaded.active.is_empty(), "active must be empty");
        assert!(loaded.fallback.is_none(), "fallback must be None");
        assert!(loaded.routing.rules.is_empty(), "rules must be empty");
    }

    // ── FrontingSet with regex rule persists correctly ────────────────────────

    #[test]
    fn round_trip_fronting_set_with_regex_rule() {
        let mut conn = setup_db();

        let rules = vec![RoutingRule::new(
            "date-rule",
            MessagePattern::Regex(r"\d{4}-\d{2}-\d{2}".to_string()),
            PersonaId::new("scheduler"),
            20,
        )];

        let set = FrontingSet::from_parts(
            vec![PersonaId::new("supervisor")],
            None,
            RoutingTable::try_from_rules(rules).unwrap(),
        );

        save_fronting_set(&mut conn, &set).unwrap();
        let loaded = load_fronting_set(&conn).unwrap().expect("should have data");

        assert_eq!(loaded.routing.rules.len(), 1);
        assert_eq!(loaded.routing.rules[0].id, "date-rule");
        // The regex pattern source must round-trip.
        match &loaded.routing.rules[0].pattern {
            MessagePattern::Regex(src) => {
                assert_eq!(src, r"\d{4}-\d{2}-\d{2}");
            }
            other => panic!("expected Regex pattern, got {other:?}"),
        }
        // The compiled table is rebuilt by try_from_rules during load.
        let m = loaded.routing.first_match("Date: 2026-04-25");
        assert!(m.is_some(), "regex rule must match after load and compile");
    }

    // ── Migrations apply cleanly (includes fronting tables) ───────────────────

    #[test]
    fn migration_creates_fronting_tables() {
        let conn = setup_db();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert!(
            tables.contains(&"fronting_set".to_string()),
            "fronting_set table must exist; got {tables:?}"
        );
        assert!(
            tables.contains(&"routing_rules".to_string()),
            "routing_rules table must exist; got {tables:?}"
        );
    }
}
