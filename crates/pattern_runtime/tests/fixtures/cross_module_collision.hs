{-# LANGUAGE DataKinds, TypeOperators, OverloadedStrings #-}
-- | Cross-module DataCon collision validation fixture.
--
-- Exercises the hardest Memory/File collision: both modules have a
-- `Read :: String -> _` constructor with identical unqualified name AND
-- identical arity. The earlier arity-disambiguation fix couldn't help
-- these; only module-qualified lookup (the `#[core(module = "...")]`
-- derive attribute) can pick the right DataCon.
--
-- The agent calls both `M.read_` and `F.read_` in sequence. Decode must
-- succeed for both; dispatch then routes Memory.Read to the real handler
-- (errors with "no block named ..." — expected, the block was never
-- created) and File.Read to the stub (errors with "Pattern.File is not
-- implemented" — expected). The test asserts neither surfaces as
-- `UnknownDataConQualified` / `UnknownDataConNameArity`, which would
-- indicate a decode-path regression.
--
-- Effect-row positions match SdkBundle:
--   0=Memory, 1=Message, 2=Display, 3=Time, 4=Log,
--   5=Shell, 6=File, 7=Sources, 8=Mcp, 9=Rpc, 10=Spawn
-- Qualified imports resolve Haskell-level ambiguity between modules.
module CrossModuleCollision (agent) where

import Control.Monad.Freer (Eff)
import qualified Pattern.Memory  as M
import qualified Pattern.File    as F
import Pattern.Message
import Pattern.Display
import Pattern.Time
import Pattern.Log
import Pattern.Shell
import Pattern.Sources
import Pattern.Mcp
import Pattern.Rpc
import Pattern.Spawn

agent :: Eff '[M.Memory, Message, Display, Time, Log, Shell, F.File, Sources, Mcp, Rpc, Spawn] ()
agent = do
  -- Write to make sure Memory effect decode works (arity-3 variant — was
  -- already covered by the pre-module fix, retained for breadth).
  M.put "greeting" "hello"
  -- Both arity-1 Reads. Only module qualification can disambiguate these.
  _ <- M.get "greeting"
  _ <- F.read_ "/does/not/exist"
  pure ()
