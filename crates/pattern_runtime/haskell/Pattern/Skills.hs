-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE GADTs #-}
-- | Pattern.Skills — skill-block operations.
--
-- 'Skills' provides the agent-facing API for discovering, loading, and
-- querying skill blocks. Skill blocks carry structured Markdown that
-- agents can inject into their composed context via 'Load'.
--
-- JSON-encoded payloads ('SkillInfo', 'SkillMetadata', 'SkillUsageStats')
-- are passed as opaque 'Text' blobs. The runtime decodes them on the Rust
-- side; agents that want typed construction should use the helpers below
-- or build the JSON via @Pattern.Aeson@.
--
-- This module is always imported qualified:
--
-- > import qualified Pattern.Skills as Skills
-- > Skills.listSkills
-- > Skills.loadSkill myHandle
--
-- Constructor names match the Rust 'SkillsReq' variants exactly so that
-- the @#[core(module = \"Pattern.Skills\", name = \"...\")]@ derive
-- attributes decode them without manual mapping.
module Pattern.Skills
  ( -- * Effect algebra
    Skills (..)
    -- * JSON-payload type aliases
  , BlockHandle
  , SkillInfo
  , SkillMetadata
  , SkillUsageStats
    -- * Helpers
  , listSkills
  , getSkillMetadata
  , loadSkill
  , searchSkills
  , getSkillUsageStats
  ) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Opaque handle identifying a skill memory block.
type BlockHandle = Text

-- | JSON-encoded 'SkillInfo' record returned as an element of 'List'
-- and 'Search' results. Schema:
--
-- > {
-- >   "handle": BlockHandle,
-- >   "name": Text,
-- >   "description": Text?,          -- from skill frontmatter
-- >   "trust_tier": Text,            -- kebab-case: "first-party" | "project-local" | "plugin-installed" | "ad-hoc"
-- >   "keywords": [Text],
-- >   "last_used": Text?             -- ISO 8601 timestamp or null
-- > }
type SkillInfo = Text

-- | JSON-encoded 'SkillMetadata' record (typed frontmatter). Schema:
--
-- > {
-- >   "name": Text,
-- >   "description": Text?,
-- >   "trust_tier": Text,
-- >   "keywords": [Text],
-- >   "hooks": Value                  -- arbitrary hook config from frontmatter
-- > }
--
-- Usage-stat fields (last_used, use_count) are NOT included here; use
-- 'GetUsageStats' for those.
type SkillMetadata = Text

-- | JSON-encoded 'SkillUsageStats' record. Schema:
--
-- > {
-- >   "handle": BlockHandle,
-- >   "use_count": Int,
-- >   "last_used": Text?,            -- ISO 8601 timestamp or null (null = never loaded)
-- >   "last_used_by": Text?          -- agent-id or null
-- > }
type SkillUsageStats = Text

-- | Skill effect algebra.
--
-- Constructor names match the Rust 'SkillsReq' variants exactly so that
-- the @#[core(module = \"Pattern.Skills\", name = \"...\")]@ derive
-- attributes decode them without manual mapping.
data Skills a where
  -- | Enumerate all skill blocks visible in the current scope.
  -- Returns a JSON-encoded @[SkillInfo]@.
  List          :: Skills Text
  -- | Fetch typed frontmatter for the given skill block.
  -- Returns a JSON-encoded @Maybe SkillMetadata@ ('Nothing' if the
  -- handle refers to a non-Skill block).
  GetMetadata   :: BlockHandle -> Skills Text
  -- | Load a skill block: returns the rendered
  -- @[skill:loaded] … [skill:loaded:end]@ text (markers + frontmatter line
  -- + full body) as the tool result, and records a usage-stat row in
  -- sqlite. The returned string becomes the tool_result content; because
  -- tool_result messages are part of conversation history, the skill body
  -- naturally persists across subsequent turns without a separate
  -- pseudo-message pipe.
  Load          :: BlockHandle -> Skills Text
  -- | Full-text search over skill name, description, keywords, and body.
  -- Returns a JSON-encoded @[SkillInfo]@ ordered by BM25 relevance.
  Search        :: Text        -> Skills Text
  -- | Fetch sqlite usage statistics for the given skill block.
  -- Returns a JSON-encoded 'SkillUsageStats'. Returns
  -- @{use_count:0, last_used:null, last_used_by:null}@ when the skill
  -- has never been loaded.
  GetUsageStats :: BlockHandle -> Skills Text

-- | Enumerate all skill blocks visible in the current scope.
--
-- Returns a JSON-encoded list of 'SkillInfo' records. Use
-- @Pattern.Aeson@ to decode individual fields.
listSkills :: Member Skills effs => Eff effs Text
listSkills = send List

-- | Fetch typed frontmatter for the given skill block.
--
-- Returns a JSON-encoded @Maybe SkillMetadata@. A 'Nothing' result means
-- the handle exists but is not a Skill block.
getSkillMetadata :: Member Skills effs => BlockHandle -> Eff effs Text
getSkillMetadata h = send (GetMetadata h)

-- | Load a skill block.
--
-- Returns the rendered @[skill:loaded] name=\"…\" trust_tier=\"…\"@ +
-- frontmatter line + full body + @[skill:loaded:end]@ as 'Text', delivered
-- as the tool_result content. Persists across subsequent turns naturally
-- via conversation history.
--
-- Also records a usage-stat row (increments @use_count@) in sqlite.
-- Loading the same skill twice produces two tool_result messages (no
-- dedup in v1, by design).
loadSkill :: Member Skills effs => BlockHandle -> Eff effs Text
loadSkill h = send (Load h)

-- | Search skill blocks by FTS5 query.
--
-- Searches over skill name, description, keywords, and body text.
-- Returns a JSON-encoded @[SkillInfo]@ ordered by BM25 relevance score.
searchSkills :: Member Skills effs => Text -> Eff effs Text
searchSkills q = send (Search q)

-- | Fetch sqlite usage statistics for the given skill block.
--
-- Returns a JSON-encoded 'SkillUsageStats'. If the skill has never been
-- loaded, returns @{use_count: 0, last_used: null, last_used_by: null}@.
getSkillUsageStats :: Member Skills effs => BlockHandle -> Eff effs Text
getSkillUsageStats h = send (GetUsageStats h)
