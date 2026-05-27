// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Markdown + YAML-frontmatter converter for Skill blocks.
//!
//! Skill files pair YAML frontmatter metadata with a markdown body. This
//! module handles parsing and emitting these files. See Phase 4 of the
//! v3-task-skill-blocks plan.
//!
//! The [`loro_bridge`] submodule bridges between the parsed [`parse::SkillFile`]
//! representation and the LoroDoc containers used by the subscriber worker
//! and the external-edit inbound path.

pub mod emit;
pub mod errors;
pub mod loro_bridge;
pub mod parse;

pub use emit::{SkillEmitError, emit};
pub use errors::SkillParseError;
pub use loro_bridge::{
    project_extras_from_loro, project_metadata_from_loro, write_skill_to_loro_doc,
};
pub use parse::{SkillFile, parse};
