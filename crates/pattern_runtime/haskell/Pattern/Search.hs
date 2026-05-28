-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE GADTs #-}
-- | Pattern.Search — scoped search across message history and archival
-- entries.
--
-- This module provides cross-agent and constellation-wide search
-- operations. Scope defaults to 'CurrentAgent' when omitted; the
-- runtime's permission resolver validates cross-agent access against
-- shared-block and group-membership records.
--
-- 'SearchMessages' / 'SearchArchival' / 'SearchAll' follow the naming
-- pattern of v2's @SearchDomain@ (Conversations / ArchivalMemory / All).
-- The @Search@-prefix avoids GADT constructor collisions with
-- @Pattern.Memory@ (which has its own @Search@).
module Pattern.Search where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Search query string.
type SearchQuery = Text

-- | Search scope descriptor. Scheme-prefixed string:
-- @"current"@ for current agent, @"agent:<id>"@ for a specific agent,
-- @"agents:<id1>,<id2>"@ for multiple agents, @"constellation"@ for
-- all agents. Parsed by the runtime.
type Scope = Text

-- | Search result entry (structured by the runtime).
type SearchHit = Text

-- | Search effect algebra.
data Search a where
  SearchMessages :: SearchQuery -> Maybe Scope -> Search [SearchHit]
  SearchArchival :: SearchQuery -> Maybe Scope -> Search [SearchHit]
  SearchAll      :: SearchQuery -> Maybe Scope -> Search [SearchHit]

-- | Search message history. Scope defaults to current agent when
-- 'Nothing'.
messages :: Member Search effs => SearchQuery -> Maybe Scope -> Eff effs [SearchHit]
messages q s = send (SearchMessages q s)

-- | Search archival entries. Scope defaults to current agent when
-- 'Nothing'.
archival :: Member Search effs => SearchQuery -> Maybe Scope -> Eff effs [SearchHit]
archival q s = send (SearchArchival q s)

-- | Search all domains (messages + archival + blocks). Scope defaults
-- to current agent when 'Nothing'. Trailing underscore avoids collision
-- with 'Pattern.Prelude.all' when this module is imported unqualified.
all_ :: Member Search effs => SearchQuery -> Maybe Scope -> Eff effs [SearchHit]
all_ q s = send (SearchAll q s)
