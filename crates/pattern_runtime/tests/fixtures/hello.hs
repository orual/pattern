{-# LANGUAGE DataKinds, TypeOperators, GADTs, OverloadedStrings #-}
-- | Minimal hello-world agent for the end-to-end integration test.
--
-- Defines effect GADTs inline rather than importing from Pattern.Time/Log
-- because tidepool-extract's multi-module include-path compilation currently
-- produces constructor tag mismatches (CASE TRAP). The constructor names
-- match the Rust-side `TimeReq` / `LogReq` `FromCore` derivations byte-for-byte.
--
-- Once tidepool fixes multi-module DataCon tag handling, this fixture should
-- be updated to import from Pattern.Time and Pattern.Log directly.
module Hello (agent) where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- Inline Time effect matching Pattern.Time's GADT.
data Time a where
  Now   :: Time Int
  Sleep :: Int -> Time ()

now :: Member Time effs => Eff effs Int
now = send Now

-- Inline Log effect matching Pattern.Log's GADT.
data Log a where
  Debug :: Text -> Log ()
  Info  :: Text -> Log ()
  Warn  :: Text -> Log ()
  Error :: Text -> Log ()

logInfo :: Member Log effs => Text -> Eff effs ()
logInfo msg = send (Info msg)

agent :: Eff '[Time, Log] ()
agent = do
  _t <- now
  logInfo "hello from haskell"
