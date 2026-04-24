# First-party skills

Skill `.md` files placed here ship with `pattern_runtime` and load with
`SkillTrustTier::FirstParty` (see
`crates/pattern_memory/src/skill.rs::assign_trust_tier`). The runtime
resolves this directory at compile time via
`concat!(env!("CARGO_MANIFEST_DIR"), "/resources/skills")` — see the
`FIRST_PARTY_SKILL_DIR` constant.

Skill file format: `---\n<YAML frontmatter>\n---\n\n<markdown body>`.
Required frontmatter keys: `name`, `trust_tier`. See
`pattern_core::types::memory_types::SkillMetadata` for the full schema.
