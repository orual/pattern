//! Handler for `Pattern.Skills` — skill-operation surface (list, get_metadata, load, search, get_usage_stats).
//!
//! The handler wires five methods: list, get_metadata, load, search, and get_usage_stats.
//! Methods delegate to MemoryStore and pattern_db for data access.

/// Skills handler (implementation details in later tasks).
pub struct SkillsHandler;
