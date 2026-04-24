//! Markdown + YAML-frontmatter converter for Skill blocks.
//!
//! Skill files pair YAML frontmatter metadata with a markdown body. This
//! module handles parsing and emitting these files. See Phase 4 of the
//! v3-task-skill-blocks plan.

pub mod errors;
pub mod parse;

pub use errors::SkillParseError;
pub use parse::{SkillFile, parse};
