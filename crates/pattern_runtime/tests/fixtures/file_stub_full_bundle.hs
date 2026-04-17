{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Minimal agent against the full 11-handler SdkBundle that calls
-- `Pattern.File.read_` — the File stub rejects with "not implemented",
-- which the session should surface as
-- `RuntimeError::SdkHandlerFailed { handler: "Pattern.File", ... }`.
--
-- Effect-row positions match SdkBundle:
--   0=Memory, 1=Message, 2=Display, 3=Time, 4=Log,
--   5=Shell, 6=File, 7=Sources, 8=Mcp, 9=Rpc, 10=Spawn
module FileStubFullBundle (agent) where

import Control.Monad.Freer (Eff)
import Pattern.Memory
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log
import Pattern.Shell
import Pattern.File
import Pattern.Sources
import Pattern.Mcp
import Pattern.Rpc
import Pattern.Spawn

agent :: Eff '[Memory, Message, Display, Time, Log, Shell, File, Sources, Mcp, Rpc, Spawn] ()
agent = do
  _ <- read_ "/does/not/exist"
  pure ()
