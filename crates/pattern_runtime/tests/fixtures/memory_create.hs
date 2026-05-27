-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Memory metadata / Create / Replace exercise. Uses the Prelude-5
-- effect list; imports @Pattern.Memory@ qualified to keep the
-- narrower helper names (create, replace, writeWithDesc) disambiguated
-- from any future Prelude re-export changes.
module MemoryCreate (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory (Memory, BlockType(..), SchemaKind(..))
import qualified Pattern.Memory as M
import Pattern.Search
import Pattern.Recall
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log

agent :: Eff '[Memory, Search, Recall, Message, Display, Time, Log] ()
agent = do
  -- Explicitly create a block with full metadata.
  M.create "notes" "user notes block" BlockWorking SchemaText Nothing "first line"
  -- writeWithDesc updates both content and description on an existing block.
  M.putWithDesc "notes" "first line\nsecond line" "user notes (revised)"
  -- Replace exercises the string-replace path.
  M.replace "notes" "first" "HEAD"
  info "memory_create fixture done"
