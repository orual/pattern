//! Mirror of `Pattern.Skills` (`haskell/Pattern/Skills.hs`).
//!
//! Five skill-operation variants supporting the SDK surface methods:
//! `list`, `get_metadata`, `load`, `search`, and `get_usage_stats`.

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Skills` GADT.
#[derive(Debug, FromCore)]
pub enum SkillsReq {
    // variants added per-method in later tasks
}
