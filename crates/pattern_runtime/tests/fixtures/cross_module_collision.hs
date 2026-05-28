-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Cross-module dispatch validation fixture.
--
-- The SDK today uses distinct unqualified constructor names across the
-- two modules: Memory exposes `Get`/`Put` (KV semantics) and File
-- exposes `Read`/`Write` (file semantics), so there is no naming
-- collision at decode time. This fixture exercises cross-module
-- dispatch anyway — regression guard against a future rename that
-- might reintroduce an unqualified-name overlap; the derive layer's
-- arity disambiguation + module-qualified lookup must continue to work.
--
-- The agent calls `M.put`, `M.get`, and `F.read` in sequence. Decode
-- must succeed for all three; dispatch then routes the Memory ops to
-- the real MemoryHandler (Put auto-creates, Get reads it back) and
-- File.Read to the stub (which errors with "not implemented" —
-- expected). The test asserts no `UnknownDataCon*` error appears,
-- guarding against decode-path regressions.
--
-- Effect-row positions match SdkBundle (post Phase 4 Task 8):
--   0=Memory, 1=Search, 2=Recall, 3=Message, 4=Display, 5=Time, 6=Log,
--   7=Shell, 8=File, 9=Mcp, 10=Spawn, 11=Port
-- Qualified imports resolve Haskell-level ambiguity between modules.
module CrossModuleCollision (agent) where

import Control.Monad.Freer (Eff)
import qualified Pattern.Memory  as M
import Pattern.Search
import Pattern.Recall
import qualified Pattern.File    as F
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log
import Pattern.Shell
import Pattern.Mcp
import Pattern.Spawn
import qualified Pattern.Port as Port

agent :: Eff '[M.Memory, Search, Recall, Message, Display, Time, Log, Shell, F.File, Mcp, Spawn, Port.Port] ()
agent = do
  -- Write to make sure Memory effect decode works (arity-3 variant — was
  -- already covered by the pre-module fix, retained for breadth).
  M.put "greeting" "hello"
  -- Both arity-1 Reads. Only module qualification can disambiguate these.
  _ <- M.get "greeting"
  _ <- F.read "/does/not/exist"
  pure ()
