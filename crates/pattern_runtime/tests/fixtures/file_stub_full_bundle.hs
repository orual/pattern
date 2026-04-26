{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal agent against the full 15-handler SdkBundle that calls
-- `Pattern.File.read` — the File stub rejects with "not implemented",
-- which the session should surface as
-- `RuntimeError::SdkHandlerFailed { handler: "Pattern.File", ... }`.
--
-- Effect-row positions match SdkBundle (post Phase 4 Task 8):
--   0=Memory, 1=Search, 2=Recall, 3=Tasks, 4=Skills, 5=Message, 6=Display,
--   7=Time, 8=Log, 9=Shell, 10=File, 11=Mcp, 12=Spawn, 13=Diagnostics, 14=Port
module FileStubFullBundle (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Search
import Pattern.Recall
import Pattern.Tasks
import Pattern.Skills
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log
import Pattern.Shell
import qualified Pattern.File as F
import Pattern.Mcp
import Pattern.Spawn
import Pattern.Diagnostics
import qualified Pattern.Port as Port

-- Qualified import of Pattern.File avoids ambiguity with base Prelude.read
-- (this fixture has no NoImplicitPrelude pragma).
agent :: Eff '[Memory, Search, Recall, Tasks, Skills, Message, Display, Time, Log, Shell, F.File, Mcp, Spawn, Diagnostics, Port.Port] ()
agent = do
  _ <- F.read "/does/not/exist"
  pure ()
