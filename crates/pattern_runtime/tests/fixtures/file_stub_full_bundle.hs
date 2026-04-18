{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal agent against the full 13-handler SdkBundle that calls
-- `Pattern.File.read` — the File stub rejects with "not implemented",
-- which the session should surface as
-- `RuntimeError::SdkHandlerFailed { handler: "Pattern.File", ... }`.
--
-- Effect-row positions match SdkBundle:
--   0=Memory, 1=Search, 2=Recall, 3=Message, 4=Display, 5=Time, 6=Log,
--   7=Shell, 8=File, 9=Sources, 10=Mcp, 11=Rpc, 12=Spawn
module FileStubFullBundle (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Search
import Pattern.Recall
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log
import Pattern.Shell
import qualified Pattern.File as F
import Pattern.Sources
import Pattern.Mcp
import Pattern.Rpc
import Pattern.Spawn

-- Qualified import of Pattern.File avoids ambiguity with base Prelude.read
-- (this fixture has no NoImplicitPrelude pragma).
agent :: Eff '[Memory, Search, Recall, Message, Display, Time, Log, Shell, F.File, Sources, Mcp, Rpc, Spawn] ()
agent = do
  _ <- F.read "/does/not/exist"
  pure ()
