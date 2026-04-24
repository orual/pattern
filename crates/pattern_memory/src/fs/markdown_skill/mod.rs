//! Markdown + YAML-frontmatter converter for Skill blocks.
//!
//! Skill files pair YAML frontmatter metadata with a markdown body. This
//! module handles parsing and emitting these files. See Phase 4 of the
//! v3-task-skill-blocks plan.
//!
//! Task 5 lays down the error type; Tasks 6–7 fill in the parser and emitter.

pub mod errors;
pub use errors::SkillParseError;
