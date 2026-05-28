// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Query functions for the `skill_usage_stats` table (migration 0012).
//!
//! Skill usage stats are per-local-install observability: how many times *this*
//! runtime has loaded a skill, and which agent loaded it most recently. They are
//! NOT part of the replicated LoroDoc — writes here never touch the canonical
//! `.md` file and never affect the content hash.

use std::collections::HashMap;
use std::str::FromStr;

use jiff::Timestamp;
use pattern_core::types::ids::AgentId;
use pattern_core::types::{block::BlockHandle, memory_types::SkillUsageStats};

// region: record_usage

/// Record a single skill load event.
///
/// Uses an upsert: if no row exists for `block`, one is created with
/// `use_count = 1`. On conflict, `last_used`, `last_used_by`, and
/// `use_count` are atomically updated. The counter is monotonic — it only
/// increments, never decreases.
pub fn record_usage(
    tx: &rusqlite::Transaction,
    block: &BlockHandle,
    agent: &AgentId,
    at: Timestamp,
) -> rusqlite::Result<()> {
    // RFC 3339 text, consistent with the rest of the codebase (jiff::Timestamp
    // stored as RFC 3339). Timestamp::to_string() produces RFC 3339.
    let at_str = at.to_string();
    let agent_str = agent.as_str();
    let block_str = block.as_str();

    tx.execute(
        "INSERT INTO skill_usage_stats (block_handle, last_used, last_used_by, use_count)
         VALUES (?1, ?2, ?3, 1)
         ON CONFLICT(block_handle) DO UPDATE
         SET last_used     = excluded.last_used,
             last_used_by  = excluded.last_used_by,
             use_count     = skill_usage_stats.use_count + 1",
        rusqlite::params![block_str, at_str, agent_str],
    )?;
    Ok(())
}

// endregion: record_usage

// region: get_usage_stats

/// Retrieve usage stats for a single skill block.
///
/// Returns [`SkillUsageStats::default()`] when no row exists — missing rows
/// are not an error; they simply mean the skill has never been loaded on this
/// install.
pub fn get_usage_stats(
    conn: &rusqlite::Connection,
    block: &BlockHandle,
) -> rusqlite::Result<SkillUsageStats> {
    let block_str = block.as_str();

    let mut stmt = conn.prepare_cached(
        "SELECT last_used, last_used_by, use_count
         FROM skill_usage_stats
         WHERE block_handle = ?1",
    )?;

    let row = stmt.query_row(rusqlite::params![block_str], from_row);
    match row {
        Ok(stats) => Ok(stats),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(SkillUsageStats::default()),
        Err(e) => Err(e),
    }
}

// endregion: get_usage_stats

// region: get_usage_stats_batch

/// Retrieve usage stats for a batch of skill blocks in a single query.
///
/// Returns a map containing only blocks that have existing rows; handles with
/// no data are omitted from the result (callers treat absence as
/// `SkillUsageStats::default()`). The implementation avoids N+1 by issuing a
/// single `IN (...)` query over all requested handles.
///
/// When `blocks` is empty, an empty map is returned without hitting the DB.
pub fn get_usage_stats_batch(
    conn: &rusqlite::Connection,
    blocks: &[BlockHandle],
) -> rusqlite::Result<HashMap<BlockHandle, SkillUsageStats>> {
    if blocks.is_empty() {
        return Ok(HashMap::new());
    }

    // Build a single query with positional placeholders for the IN clause.
    // SmolStr doesn't impl ToSql, so we materialize to owned Strings.
    let owned: Vec<String> = blocks.iter().map(|b| b.as_str().to_owned()).collect();
    let placeholders: Vec<String> = (1..=owned.len()).map(|i| format!("?{i}")).collect();

    let sql = format!(
        "SELECT block_handle, last_used, last_used_by, use_count
         FROM skill_usage_stats
         WHERE block_handle IN ({})",
        placeholders.join(", ")
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = owned
        .iter()
        .map(|s| s as &dyn rusqlite::types::ToSql)
        .collect();

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        let handle_str: String = row.get(0)?;
        // In the batch query, column layout is: 0=block_handle, 1=last_used,
        // 2=last_used_by, 3=use_count. `from_row` uses (0, 1, 2), so we
        // call `from_row_offset` with the appropriate base.
        let stats = from_row_offset(row, 1)?;
        Ok((handle_str, stats))
    })?;

    let mut result = HashMap::with_capacity(blocks.len());
    for row in rows {
        let (handle_str, stats) = row?;
        result.insert(BlockHandle::new(&handle_str), stats);
    }
    Ok(result)
}

// endregion: get_usage_stats_batch

// region: from_row helpers

/// Parse `(last_used TEXT, last_used_by TEXT, use_count INTEGER)` columns
/// starting at `offset` into a [`SkillUsageStats`].
///
/// Column layout relative to `offset`:
/// - `offset + 0`: last_used (TEXT, nullable)
/// - `offset + 1`: last_used_by (TEXT, nullable)
/// - `offset + 2`: use_count (INTEGER)
///
/// This lets both `get_usage_stats` (offset = 0) and `get_usage_stats_batch`
/// (offset = 1, after the leading `block_handle` column) share the same parsing
/// logic without duplicating timestamp and agent parsing.
fn from_row_offset(row: &rusqlite::Row, offset: usize) -> rusqlite::Result<SkillUsageStats> {
    let last_used_str: Option<String> = row.get(offset)?;
    let last_used_by_str: Option<String> = row.get(offset + 1)?;
    let use_count: u64 = {
        // rusqlite maps INTEGER to i64. The counter is always non-negative so
        // we convert safely; negative values indicate DB corruption and are
        // reported as a type conversion failure rather than silently wrapping.
        let raw: i64 = row.get(offset + 2)?;
        u64::try_from(raw).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                offset + 2,
                rusqlite::types::Type::Integer,
                format!("use_count {raw} is negative; expected non-negative integer").into(),
            )
        })?
    };

    let last_used = last_used_str
        .as_deref()
        .map(|s| {
            Timestamp::from_str(s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    offset,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .transpose()?;

    let last_used_by = last_used_by_str.map(|s| AgentId::new(s.as_str()));

    Ok(SkillUsageStats {
        last_used,
        last_used_by,
        use_count,
    })
}

/// Parse a `(last_used TEXT, last_used_by TEXT, use_count INTEGER)` row
/// starting at column index 0. Convenience wrapper around [`from_row_offset`]
/// for the `get_usage_stats` single-handle query.
fn from_row(row: &rusqlite::Row) -> rusqlite::Result<SkillUsageStats> {
    from_row_offset(row, 0)
}

// endregion: from_row helpers

// region: tests

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> rusqlite::Connection {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::migrations::run_memory_migrations(&mut conn).unwrap();
        conn
    }

    fn make_block(name: &str) -> BlockHandle {
        BlockHandle::new(name)
    }

    fn make_agent(name: &str) -> AgentId {
        AgentId::new(name)
    }

    fn now() -> Timestamp {
        // Fixed test timestamp to avoid flakiness. Use a deterministic RFC 3339 value.
        Timestamp::from_str("2026-04-24T12:00:00Z").unwrap()
    }

    #[test]
    fn record_usage_inserts_and_increments() {
        // Call record_usage 3× on the same block; use_count must be 3 and
        // last_used must match the most-recent call's timestamp.
        let mut conn = setup_db();

        let block = make_block("my-skill");
        let agent = make_agent("agent-a");
        let t1 = Timestamp::from_str("2026-04-24T10:00:00Z").unwrap();
        let t2 = Timestamp::from_str("2026-04-24T11:00:00Z").unwrap();
        let t3 = Timestamp::from_str("2026-04-24T12:00:00Z").unwrap();

        {
            let tx = conn.transaction().unwrap();
            record_usage(&tx, &block, &agent, t1).unwrap();
            tx.commit().unwrap();
        }
        {
            let tx = conn.transaction().unwrap();
            record_usage(&tx, &block, &agent, t2).unwrap();
            tx.commit().unwrap();
        }
        {
            let tx = conn.transaction().unwrap();
            record_usage(&tx, &block, &agent, t3).unwrap();
            tx.commit().unwrap();
        }

        let stats = get_usage_stats(&conn, &block).unwrap();
        assert_eq!(stats.use_count, 3, "use_count must be 3 after 3 calls");
        assert_eq!(
            stats.last_used.as_ref().map(|t| t.to_string()),
            Some(t3.to_string()),
            "last_used must be the most recent timestamp"
        );
        assert_eq!(
            stats.last_used_by.as_ref().map(|a| a.as_str()),
            Some("agent-a"),
        );
    }

    #[test]
    fn get_usage_stats_default_for_unknown_block() {
        // A block with no row returns SkillUsageStats::default() — not an error.
        let conn = setup_db();
        let block = make_block("never-seen");

        let stats = get_usage_stats(&conn, &block).unwrap();
        assert_eq!(stats, SkillUsageStats::default());
        assert_eq!(stats.use_count, 0);
        assert!(stats.last_used.is_none());
        assert!(stats.last_used_by.is_none());
    }

    #[test]
    fn get_usage_stats_batch_for_mixed_presence() {
        // 5 handles; 3 have rows, 2 don't. Returned map must have exactly 3 entries.
        let mut conn = setup_db();

        let blocks: Vec<BlockHandle> = (1..=5).map(|i| make_block(&format!("skill-{i}"))).collect();
        let agent = make_agent("agent-b");
        let at = now();

        // Insert rows for blocks 1, 2, 3 only.
        for b in &blocks[0..3] {
            let tx = conn.transaction().unwrap();
            record_usage(&tx, b, &agent, at).unwrap();
            tx.commit().unwrap();
        }

        let map = get_usage_stats_batch(&conn, &blocks).unwrap();
        assert_eq!(
            map.len(),
            3,
            "batch result must contain exactly the 3 handles with rows"
        );
        for b in &blocks[0..3] {
            assert!(map.contains_key(b), "expected {b} in result");
            assert_eq!(map[b].use_count, 1);
        }
        for b in &blocks[3..5] {
            assert!(!map.contains_key(b), "unexpected {b} in result");
        }
    }

    #[test]
    fn get_usage_stats_batch_empty_slice_returns_empty_map() {
        let conn = setup_db();
        let result = get_usage_stats_batch(&conn, &[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn record_usage_with_different_agents_keeps_latest_agent() {
        // The most recent call's agent should be preserved as last_used_by.
        let mut conn = setup_db();
        let block = make_block("shared-skill");
        let t1 = Timestamp::from_str("2026-04-24T10:00:00Z").unwrap();
        let t2 = Timestamp::from_str("2026-04-24T11:00:00Z").unwrap();

        {
            let tx = conn.transaction().unwrap();
            record_usage(&tx, &block, &make_agent("agent-x"), t1).unwrap();
            tx.commit().unwrap();
        }
        {
            let tx = conn.transaction().unwrap();
            record_usage(&tx, &block, &make_agent("agent-y"), t2).unwrap();
            tx.commit().unwrap();
        }

        let stats = get_usage_stats(&conn, &block).unwrap();
        assert_eq!(stats.use_count, 2);
        assert_eq!(
            stats.last_used_by.as_ref().map(|a| a.as_str()),
            Some("agent-y"),
            "last_used_by must be the most recently recorded agent"
        );
    }

    #[test]
    fn content_hash_stability_record_usage_does_not_touch_canonical_file() {
        // This test documents the content-hash stability property.
        // skill_usage_stats is an sqlite-only table; record_usage never
        // touches the canonical .md file. Therefore:
        //   - emit(parse(file)) is byte-identical before and after N record_usage calls.
        //   - No content-hash echo-suppression carve-out is needed.
        //
        // We verify the sqlite side here: after 100 record_usage calls, the table row
        // reflects the count but we have made no file-system mutations.
        let mut conn = setup_db();
        let block = make_block("stable-skill");
        let agent = make_agent("agent-c");
        let at = now();

        for _ in 0..100 {
            let tx = conn.transaction().unwrap();
            record_usage(&tx, &block, &agent, at).unwrap();
            tx.commit().unwrap();
        }

        let stats = get_usage_stats(&conn, &block).unwrap();
        assert_eq!(stats.use_count, 100);
        // No file was written; this is enforced structurally (record_usage takes
        // only &Transaction + typed args, not a &Path or &[u8]). The canonical
        // .md bytes are unchanged by construction.
    }
}

// endregion: tests
